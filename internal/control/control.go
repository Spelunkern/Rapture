// Package control starts and stops this checkout's rapture_login and
// rapture_world binaries (build/). Servers run detached, so whatever
// starts them can exit without taking them down.
package control

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
)

// Servers are started in this order and stopped together.
var Servers = []string{"rapture_login", "rapture_world"}

func exeName(name string) string {
	if runtime.GOOS == "windows" {
		return name + ".exe"
	}
	return name
}

// RepoRoot is the parent of the build/ directory holding the running
// executable.
func RepoRoot() string {
	exe, err := os.Executable()
	if err != nil {
		return "."
	}
	if resolved, err := filepath.EvalSymlinks(exe); err == nil {
		exe = resolved
	}
	return filepath.Dir(filepath.Dir(exe))
}

func binPath(name string) string { return filepath.Join(RepoRoot(), "build", exeName(name)) }

// pids lists running processes started from this checkout's build/<name>.
func pids(name string) []int {
	if runtime.GOOS == "windows" {
		out, _ := exec.Command("tasklist", "/FI", "IMAGENAME eq "+exeName(name), "/FO", "CSV", "/NH").Output()
		var found []int
		for _, line := range strings.Split(string(out), "\n") {
			fields := strings.Split(line, ",")
			if len(fields) > 1 {
				if pid, err := strconv.Atoi(strings.Trim(fields[1], "\" \r")); err == nil {
					found = append(found, pid)
				}
			}
		}
		return found
	}
	want := binPath(name)
	entries, _ := os.ReadDir("/proc")
	var found []int
	for _, entry := range entries {
		pid, err := strconv.Atoi(entry.Name())
		if err != nil {
			continue
		}
		// A rebuild replaces the file under a running server, whose exe link
		// then reads "<path> (deleted)"; it is still this checkout's server.
		if target, err := os.Readlink(filepath.Join("/proc", entry.Name(), "exe")); err == nil &&
			(target == want || target == want+" (deleted)") {
			found = append(found, pid)
		}
	}
	return found
}

// Running reports whether the named server is running.
func Running(name string) bool { return len(pids(name)) > 0 }

// AllRunning reports whether every server is running.
func AllRunning() bool {
	for _, name := range Servers {
		if !Running(name) {
			return false
		}
	}
	return true
}

// AnyRunning reports whether at least one server is running.
func AnyRunning() bool {
	for _, name := range Servers {
		if Running(name) {
			return true
		}
	}
	return false
}

// Stop kills every running instance of the named server.
func Stop(name string) {
	for _, pid := range pids(name) {
		if runtime.GOOS == "windows" {
			exec.Command("taskkill", "/PID", strconv.Itoa(pid), "/F").Run()
		} else if process, err := os.FindProcess(pid); err == nil {
			process.Kill()
			process.Wait()
		}
	}
}

// Start launches the named server detached, from the repository root.
func Start(name string) error {
	path := binPath(name)
	if _, err := os.Stat(path); err != nil {
		return fmt.Errorf("missing %s -- run tools/build.sh first", path)
	}
	cmd := exec.Command(path)
	cmd.Dir = RepoRoot()
	detach(cmd)
	if err := cmd.Start(); err != nil {
		return err
	}
	return cmd.Process.Release()
}

// StartAll (re)starts every server, stopping old instances first.
func StartAll() error {
	for _, name := range Servers {
		Stop(name)
		if err := Start(name); err != nil {
			return fmt.Errorf("%s: %w", name, err)
		}
	}
	return nil
}

// StopAll stops every server.
func StopAll() {
	for _, name := range Servers {
		Stop(name)
	}
}
