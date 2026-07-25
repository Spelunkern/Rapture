//! Rapture Console: a minimal terminal replacement for the old egui-based
//! panel. Same job -- start/stop the `rapture_login` and `rapture_world`
//! processes -- just as a plain stdin command loop instead of a GUI window,
//! so it can run in any terminal (opened via a .bat on Windows or a .sh on
//! Linux) with nothing else to install.
//!
//! Only two commands for now, per the user's explicit "extremadamente
//! simple" design: `start server` and `shutdown server`. Shutdown is a
//! direct kill (matching what the old panel's "Stop Server" button already
//! did) -- no graceful drain, by explicit choice.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Binary names without a platform extension -- `std::env::consts::
/// EXE_SUFFIX` supplies `.exe` on Windows and an empty string on Linux/
/// macOS, both for the paths built below and for matching process names
/// via `sysinfo` (which reports names including any platform extension).
const LOGIN_BIN_NAME: &str = "rapture_login";
const WORLD_BIN_NAME: &str = "rapture_world";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// The repo root, derived from this exe's own location (`target/release/
/// rapture_console[.exe]`) rather than a hardcoded absolute path -- makes
/// the console work regardless of where the repo is checked out.
fn repo_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent()?.parent()?.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn bin_path(bin_name: &str) -> PathBuf {
    repo_root().join(format!(
        "target/release/{bin_name}{}",
        std::env::consts::EXE_SUFFIX
    ))
}

fn start_process(exe: &Path) -> Result<(), String> {
    if !exe.exists() {
        return Err(format!(
            "missing {} -- run tools/build-rust first",
            exe.display()
        ));
    }

    let mut command = Command::new(exe);
    command.current_dir(repo_root());
    apply_no_window(&mut command);
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("failed to start {}: {error}", exe.display()))
}

/// Matches a `sysinfo` process by binary name, ignoring the platform's exe
/// extension and case.
fn process_matches(process: &sysinfo::Process, bin_name: &str) -> bool {
    let expected = format!("{bin_name}{}", std::env::consts::EXE_SUFFIX);
    process
        .name()
        .to_str()
        .is_some_and(|name| name.eq_ignore_ascii_case(&expected))
}

fn stop_process(system: &mut sysinfo::System, bin_name: &str) {
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    for process in system.processes().values() {
        if process_matches(process, bin_name) {
            process.kill();
        }
    }
}

fn is_process_running(system: &mut sysinfo::System, bin_name: &str) -> bool {
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    system
        .processes()
        .values()
        .any(|process| process_matches(process, bin_name))
}

#[cfg(windows)]
fn apply_no_window(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn apply_no_window(_command: &mut Command) {}

fn print_status(system: &mut sysinfo::System) {
    let login = if is_process_running(system, LOGIN_BIN_NAME) { "ON" } else { "OFF" };
    let world = if is_process_running(system, WORLD_BIN_NAME) { "ON" } else { "OFF" };
    println!("login: {login} | world: {world}");
}

fn start_server(system: &mut sysinfo::System) {
    // Kill any existing instances first (matches the old panel's "Start
    // Server" button, which always did a stop-then-start to avoid ending up
    // with duplicate processes from repeated clicks).
    stop_process(system, LOGIN_BIN_NAME);
    stop_process(system, WORLD_BIN_NAME);

    match start_process(&bin_path(LOGIN_BIN_NAME)) {
        Ok(()) => println!("login: started"),
        Err(error) => println!("login: error -- {error}"),
    }
    match start_process(&bin_path(WORLD_BIN_NAME)) {
        Ok(()) => println!("world: started"),
        Err(error) => println!("world: error -- {error}"),
    }
    print_status(system);
}

fn shutdown_server(system: &mut sysinfo::System) {
    stop_process(system, LOGIN_BIN_NAME);
    stop_process(system, WORLD_BIN_NAME);
    println!("login: stopped");
    println!("world: stopped");
    print_status(system);
}

fn main() {
    let mut system = sysinfo::System::new();

    println!("Rapture Console");
    print_status(&mut system);
    println!("Commands: \"start server\" | \"shutdown server\"");

    let stdin = io::stdin();
    loop {
        print!("> ");
        let _ = io::stdout().flush();

        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            // EOF (stdin closed) -- exit cleanly rather than spin.
            break;
        }
        let command = line.trim().to_ascii_lowercase();

        match command.as_str() {
            "start server" => start_server(&mut system),
            "shutdown server" => shutdown_server(&mut system),
            "" => {}
            _ => println!("unknown command. Use \"start server\" or \"shutdown server\"."),
        }
    }
}
