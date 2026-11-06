//go:build !windows

package control

import (
	"os/exec"
	"syscall"
)

// detach starts the server in its own session so closing the console or
// app (or its terminal) does not take the server down.
func detach(cmd *exec.Cmd) { cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true} }
