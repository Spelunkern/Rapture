//go:build !windows

package main

import (
	"os/exec"
	"syscall"
)

// detach starts the server in its own session so closing the console's
// terminal does not take the server down.
func detach(cmd *exec.Cmd) { cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true} }
