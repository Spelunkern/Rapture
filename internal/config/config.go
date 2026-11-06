// Package config resolves runtime settings from the environment, falling
// back to .local/runtime.env (written by tools/sqlserver.py provision).
package config

import (
	"bufio"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"strings"
)

const ServerID = 1

// LoadRuntimeEnv reads KEY=VALUE lines from .local/runtime.env into the
// process environment without overriding variables that are already set.
func LoadRuntimeEnv() { LoadRuntimeEnvFrom(".") }

// LoadRuntimeEnvFrom is LoadRuntimeEnv for the checkout at root.
func LoadRuntimeEnvFrom(root string) {
	file, err := os.Open(filepath.Join(root, ".local", "runtime.env"))
	if err != nil {
		return
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		key, value, ok := strings.Cut(strings.TrimSpace(scanner.Text()), "=")
		if !ok || key == "" || strings.HasPrefix(key, "#") {
			continue
		}
		if _, set := os.LookupEnv(key); !set {
			os.Setenv(key, value)
		}
	}
}

func env(key, fallback string) string {
	if value, ok := os.LookupEnv(key); ok && value != "" {
		return value
	}
	return fallback
}

// DSN returns the SQL Server connection string for one database.
func DSN(database string) (string, error) {
	user, password := env("RAPTURE_DB_USER", ""), env("RAPTURE_DB_PASSWORD", "")
	if user == "" || password == "" {
		return "", fmt.Errorf("RAPTURE_DB_USER/RAPTURE_DB_PASSWORD not set; run tools/sqlserver.py setup")
	}
	query := url.Values{}
	query.Set("database", database)
	query.Set("encrypt", env("RAPTURE_DB_ENCRYPT", "disable"))
	query.Set("app name", "Rapture")
	u := url.URL{
		Scheme:   "sqlserver",
		User:     url.UserPassword(user, password),
		Host:     env("RAPTURE_DB_HOST", "127.0.0.1") + ":" + env("RAPTURE_DB_PORT", "14346"),
		RawQuery: query.Encode(),
	}
	return u.String(), nil
}

// DataDir holds map data (data/maps/*.svmap).
func DataDir() string { return env("RAPTURE_DATA_DIR", "data") }

// PublicIP is the world address sent to clients after server selection.
func PublicIP() string { return env("RAPTURE_PUBLIC_IP", "127.0.0.1") }

func LoginListen() string { return env("RAPTURE_LOGIN_LISTEN", "127.0.0.1:30800") }
func WorldListen() string { return env("RAPTURE_WORLD_LISTEN", "127.0.0.1:30810") }
