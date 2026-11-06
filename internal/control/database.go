package control

import (
	"context"
	"database/sql"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	_ "github.com/microsoft/go-mssqldb"

	"github.com/Spelunkern/Rapture/internal/config"
)

// Container is Rapture's own SQL Server container (tools/sqlserver.py).
const Container = "rapture-sqlserver"

const databaseTimeout = 120 * time.Second

func containerRunning() (bool, error) {
	out, err := exec.Command("docker", "inspect", "-f", "{{.State.Running}}", Container).Output()
	if err != nil {
		return false, fmt.Errorf("container %s not found -- run tools/sqlserver.py setup", Container)
	}
	return strings.TrimSpace(string(out)) == "true", nil
}

// DatabaseRunning reports whether the SQL Server container is up.
func DatabaseRunning() bool {
	running, _ := containerRunning()
	return running
}

// EnsureDatabase starts the SQL Server container if it is stopped and waits
// until it accepts a login, so the servers do not exit on "connection refused".
func EnsureDatabase() error {
	running, err := containerRunning()
	if err != nil {
		return err
	}
	if !running {
		if out, err := exec.Command("docker", "start", Container).CombinedOutput(); err != nil {
			return fmt.Errorf("docker start %s: %v: %s", Container, err, strings.TrimSpace(string(out)))
		}
	}
	config.LoadRuntimeEnvFrom(RepoRoot())
	dsn, err := config.DSN("PS_UserData")
	if err != nil {
		return err
	}
	pool, err := sql.Open("sqlserver", dsn)
	if err != nil {
		return err
	}
	defer pool.Close()
	deadline := time.Now().Add(databaseTimeout)
	for {
		ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		err = pool.PingContext(ctx)
		cancel()
		if err == nil {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("SQL Server not ready after %s: %v", databaseTimeout, err)
		}
		time.Sleep(time.Second)
	}
}

// StopDatabase stops the SQL Server container to free its memory.
func StopDatabase() {
	if DatabaseRunning() {
		exec.Command("docker", "stop", Container).Run()
	}
}

// lastLogLine returns the last non-empty line of logs/<name>.log.
func lastLogLine(name string) string {
	data, err := os.ReadFile(filepath.Join(RepoRoot(), "logs", name+".log"))
	if err != nil {
		return ""
	}
	lines := strings.Split(strings.TrimRight(string(data), "\n"), "\n")
	return strings.TrimSpace(lines[len(lines)-1])
}
