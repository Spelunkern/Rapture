//go:build windows

package control

import (
	"os/exec"
	"syscall"
)

const createNoWindow = 0x08000000

// detach starts the server without opening a console window.
func detach(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{CreationFlags: createNoWindow}
}
