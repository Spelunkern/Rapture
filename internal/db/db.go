// Package db is Rapture's SQL Server access layer over the original EP4.5
// databases (PS_UserData, PS_GameData, PS_GameDefs, ...). Data access goes
// through the original stored procedures where one exists.
package db

import (
	"context"
	"database/sql"
	"fmt"
	"sync"
	"time"

	"github.com/Spelunkern/Rapture/internal/config"
	_ "github.com/microsoft/go-mssqldb"
)

const (
	UserData = "PS_UserData"
	GameData = "PS_GameData"
	GameDefs = "PS_GameDefs"
	Billing  = "PS_Billing"
)

// Timeout bounds every database round trip.
const Timeout = 10 * time.Second

var (
	mu    sync.Mutex
	pools = map[string]*sql.DB{}
)

// Pool returns the shared connection pool for one database, opening and
// pinging it on first use.
func Pool(database string) (*sql.DB, error) {
	mu.Lock()
	defer mu.Unlock()
	if pool, ok := pools[database]; ok {
		return pool, nil
	}
	dsn, err := config.DSN(database)
	if err != nil {
		return nil, err
	}
	pool, err := sql.Open("sqlserver", dsn)
	if err != nil {
		return nil, err
	}
	pool.SetMaxOpenConns(16)
	pool.SetConnMaxIdleTime(5 * time.Minute)
	ctx, cancel := context.WithTimeout(context.Background(), Timeout)
	defer cancel()
	if err := pool.PingContext(ctx); err != nil {
		pool.Close()
		return nil, fmt.Errorf("connect %s: %w", database, err)
	}
	pools[database] = pool
	return pool, nil
}

// Open eagerly connects to every listed database so configuration errors
// surface at startup.
func Open(databases ...string) error {
	for _, database := range databases {
		if _, err := Pool(database); err != nil {
			return err
		}
	}
	return nil
}

func ctx() (context.Context, context.CancelFunc) {
	return context.WithTimeout(context.Background(), Timeout)
}
