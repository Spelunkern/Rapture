// Command rapture_console starts and stops rapture_login and rapture_world
// from a plain terminal prompt: "start server" and "shutdown server".
package main

import (
	"bufio"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
)

var servers = []string{"rapture_login", "rapture_world"}

func exeName(name string) string {
	if runtime.GOOS == "windows" {
		return name + ".exe"
	}
	return name
}

// repoRoot is the parent of the build/ directory holding this executable.
func repoRoot() string {
	exe, err := os.Executable()
	if err != nil {
		return "."
	}
	return filepath.Dir(filepath.Dir(exe))
}

func binPath(name string) string { return filepath.Join(repoRoot(), "build", exeName(name)) }

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
		if target, err := os.Readlink(filepath.Join("/proc", entry.Name(), "exe")); err == nil && target == want {
			found = append(found, pid)
		}
	}
	return found
}

func stop(name string) {
	for _, pid := range pids(name) {
		if runtime.GOOS == "windows" {
			exec.Command("taskkill", "/PID", strconv.Itoa(pid), "/F").Run()
		} else if process, err := os.FindProcess(pid); err == nil {
			process.Kill()
			process.Wait()
		}
	}
}

func start(name string) error {
	path := binPath(name)
	if _, err := os.Stat(path); err != nil {
		return fmt.Errorf("missing %s -- run tools/build.sh first", path)
	}
	cmd := exec.Command(path)
	cmd.Dir = repoRoot()
	detach(cmd)
	if err := cmd.Start(); err != nil {
		return err
	}
	return cmd.Process.Release()
}

func status() {
	state := func(name string) string {
		if len(pids(name)) > 0 {
			return "ON"
		}
		return "OFF"
	}
	fmt.Printf("login: %s | world: %s\n", state("rapture_login"), state("rapture_world"))
}

func main() {
	fmt.Println("Rapture Console")
	status()
	fmt.Println(`Commands: "start server" | "shutdown server"`)
	input := bufio.NewScanner(os.Stdin)
	for {
		fmt.Print("> ")
		if !input.Scan() {
			return
		}
		switch strings.ToLower(strings.TrimSpace(input.Text())) {
		case "start server":
			for _, name := range servers {
				stop(name)
				if err := start(name); err != nil {
					fmt.Printf("%s: error -- %v\n", strings.TrimPrefix(name, "rapture_"), err)
				} else {
					fmt.Printf("%s: started\n", strings.TrimPrefix(name, "rapture_"))
				}
			}
			status()
		case "shutdown server":
			for _, name := range servers {
				stop(name)
				fmt.Printf("%s: stopped\n", strings.TrimPrefix(name, "rapture_"))
			}
			status()
		case "":
		default:
			fmt.Println(`unknown command. Use "start server" or "shutdown server".`)
		}
	}
}
