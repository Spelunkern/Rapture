// Command rapture_world is the Shaiya 5.4 game server on the original
// EP4.5 SQL Server databases: character select, world simulation (maps,
// NPCs, mobs, combat, skills), items, trade and the item mall.
package main

import (
	"fmt"
	"net"
	"os"
	"sync/atomic"

	"github.com/Spelunkern/Rapture/internal/config"
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
	"github.com/Spelunkern/Rapture/internal/netx"
	"github.com/Spelunkern/Rapture/internal/world"
)

const maxConnections = 500

var nextConnection atomic.Int64

func main() {
	config.LoadRuntimeEnv()
	logger, err := logx.New("rapture_world")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	logger.Printf("Starting Rapture World")
	if err := db.Open(db.UserData, db.GameData, db.GameDefs); err != nil {
		logger.Printf("Rapture World database error: %v", err)
		os.Exit(1)
	}
	defs, err := db.Definitions()
	if err != nil {
		logger.Printf("Rapture World failed to load PS_GameDefs: %v", err)
		os.Exit(1)
	}
	logger.Printf("Rapture World loaded definitions items=%d skills=%d mobs=%d products=%d status=%d base_items=%d",
		len(defs.Items), len(defs.Skills), len(defs.Mobs), len(defs.Products), len(defs.Status), len(defs.BaseItems))
	if err := world.LoadGameConfig(logger); err != nil {
		logger.Printf("Rapture World failed to load data/game (copy the EP4.5 PS_Game/Data files there): %v", err)
		os.Exit(1)
	}
	if err := world.LoadGuilds(logger); err != nil {
		logger.Printf("Rapture World failed to load guilds: %v", err)
	}
	if err := world.LoadSvmaps(logger); err != nil {
		logger.Printf("Rapture World failed to load maps: %v", err)
		os.Exit(1)
	}
	world.StartSweeps(logger)

	listener, err := net.Listen("tcp", config.WorldListen())
	if err != nil {
		logger.Printf("Rapture World failed to bind %s: %v", config.WorldListen(), err)
		os.Exit(1)
	}
	logger.Printf("Rapture World listening on %s", config.WorldListen())
	limiter := netx.NewLimiter(maxConnections)
	for {
		conn, err := listener.Accept()
		if err != nil {
			logger.Printf("Rapture World accept error: %v", err)
			continue
		}
		if !limiter.Acquire() {
			logger.Printf("Rapture World rejected connection: at capacity (%d)", maxConnections)
			conn.Close()
			continue
		}
		netx.Tune(conn)
		go func() {
			defer limiter.Release()
			defer conn.Close()
			log := logger.Tagged(fmt.Sprintf("conn%d", nextConnection.Add(1)))
			defer func() {
				if r := recover(); r != nil {
					log.Printf("Rapture World session panic: %v", r)
				}
			}()
			if err := world.Serve(conn, logger, log); err != nil {
				log.Printf("Rapture World client error: %v", err)
			}
		}()
	}
}
