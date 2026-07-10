#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const LOGIN_PROCESS: &str = "rapture_login.exe";
const WORLD_PROCESS: &str = "rapture_world.exe";
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// The repo root, derived from this exe's own location (`target/release/
/// rapture_panel.exe`) rather than a hardcoded absolute path -- makes the
/// panel work regardless of where the repo is checked out.
fn repo_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent()?.parent()?.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn login_exe() -> PathBuf {
    repo_root().join("target/release/rapture_login.exe")
}

fn world_exe() -> PathBuf {
    repo_root().join("target/release/rapture_world.exe")
}

struct PanelApp {
    login_running: bool,
    world_running: bool,
    last_refresh: Instant,
}

impl Default for PanelApp {
    fn default() -> Self {
        let mut app = Self {
            login_running: false,
            world_running: false,
            last_refresh: Instant::now() - Duration::from_secs(60),
        };
        app.refresh_status();
        app
    }
}

impl eframe::App for PanelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.last_refresh.elapsed() >= Duration::from_secs(1) {
            self.refresh_status();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    status_pill(ui, "Login", self.login_running);
                    ui.separator();
                    status_pill(ui, "World", self.world_running);
                });

                ui.add_space(18.0);
                if ui.button("Start Server").clicked() {
                    let _ = stop_process(LOGIN_PROCESS);
                    let _ = stop_process(WORLD_PROCESS);
                    let _ = start_process(&login_exe());
                    let _ = start_process(&world_exe());
                    self.refresh_status();
                }

                if ui.button("Stop Server").clicked() {
                    let _ = stop_process(LOGIN_PROCESS);
                    let _ = stop_process(WORLD_PROCESS);
                    self.refresh_status();
                }
            });
        });

        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

impl PanelApp {
    fn refresh_status(&mut self) {
        self.login_running = is_process_running(LOGIN_PROCESS);
        self.world_running = is_process_running(WORLD_PROCESS);
        self.last_refresh = Instant::now();
    }
}

fn status_pill(ui: &mut egui::Ui, label: &str, running: bool) {
    let text = if running { "ON" } else { "OFF" };
    let color = if running {
        egui::Color32::from_rgb(35, 130, 80)
    } else {
        egui::Color32::from_rgb(150, 55, 55)
    };

    ui.colored_label(color, format!("{label}: {text}"));
}

fn start_process(exe: &Path) -> Result<String, String> {
    if !exe.exists() {
        return Err(format!("Missing {}. Run build-rust.ps1 first.", exe.display()));
    }

    let mut command = Command::new(exe);
    command.current_dir(repo_root());
    apply_no_window(&mut command);
    command
        .spawn()
        .map(|_| format!("Started {}", file_name(exe)))
        .map_err(|error| format!("Failed to start {}: {error}", exe.display()))
}

fn stop_process(process: &str) -> Result<String, String> {
    let mut command = Command::new("taskkill");
    command.args(["/IM", process, "/F"]);
    apply_no_window(&mut command);

    let _ = command.output();
    Ok(format!("Stopped {process}"))
}

fn is_process_running(process: &str) -> bool {
    let mut command = Command::new("tasklist");
    command.args([
        "/FI",
        &format!("IMAGENAME eq {process}"),
        "/FO",
        "CSV",
        "/NH",
    ]);
    apply_no_window(&mut command);

    let Ok(output) = command.output() else {
        return false;
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    stdout.contains(&process.to_ascii_lowercase())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(windows)]
fn apply_no_window(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn apply_no_window(_command: &mut Command) {}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rapture Emulator")
            .with_inner_size([320.0, 130.0])
            .with_resizable(false),
        ..Default::default()
    };

    eframe::run_native(
        "Rapture Emulator",
        options,
        Box::new(|_cc| Ok(Box::<PanelApp>::default())),
    )
}
