//go:build integration

package world

import (
	"io"
	"net"
	"os"
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/config"
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
)

// TestGuildRankingBattle enters map 50 while open (the guild's own
// instance, one entry counted), kills a battle mob for its Money2 points,
// closes the battle (everyone to the bind point, the instance removed)
// and runs the ranking 30 minutes later: rank 1 with points + min(25 x
// entries, points / 2), which opens the guild bank.
func TestGuildRankingBattle(t *testing.T) {
	if _, err := os.Stat("data/game"); err != nil {
		os.Chdir("../..")
	}
	config.LoadRuntimeEnv()
	if err := db.Open(db.UserData, db.GameData, db.GameDefs); err != nil {
		t.Fatal(err)
	}
	if _, err := db.ReloadDefsForTest(); err != nil {
		t.Fatal(err)
	}
	logger, err := logx.New("rapture_world_test")
	if err != nil {
		t.Fatal(err)
	}
	if err := LoadGameConfig(logger); err != nil {
		t.Fatal(err)
	}
	for _, m := range []uint16{53, 54} {
		if !hasGrid(m) {
			if l, err := loadSvmapFile(m, mapDataDir()+"/"+itoa(m)+".svmap", db.MustDefs()); err == nil {
				RegisterMapGrid(m, l.mapSize, l.mask)
			}
		}
	}
	const guildID = 990500
	defer func() {
		pool, _ := db.Pool(db.GameData)
		pool.Exec(`DELETE FROM dbo.GuildDetails WHERE GuildID = @p1`, int32(guildID))
		guildMu.Lock()
		delete(guildsByID, guildID)
		guildMu.Unlock()
		grbNow = time.Now
	}()
	a, b := net.Pipe()
	go io.Copy(io.Discard, b)
	ch := &db.Character{ID: 990501, Name: "GrbTest", Level: 40, MapID: 53, PosX: 89, PosY: 8, PosZ: 115}
	s := &Session{char: ch, log: logger.Tagged("grb"), logger: logger, faction: FactionLight, out: &connWriter{conn: a},
		skillCooldowns: map[uint16]time.Time{}}
	s.player = RegisterPlayer(*ch, nil, false, FactionLight, s.out, logger)
	defer func() { s.player.Unregister() }()
	g := &guild{ID: guildID, Name: "GrbTestGuild", Country: 0, Rank: grbUnranked,
		members: map[uint32]*guildMember{}, applicants: map[uint32]*guildMember{}}
	guildMu.Lock()
	g.members[ch.ID] = &guildMember{CharID: ch.ID, Name: ch.Name, Rank: guildRankMaster, player: s.player}
	guildsByID[guildID] = g
	s.player.guild.guild = g
	guildMu.Unlock()

	sunday := func(h, m int) time.Time { return time.Date(2026, 11, 8, h, m, 0, 0, time.Local) }
	grbNow = func() time.Time { return sunday(20, 30) }
	s.applyTeleport(50, 507.78, 3.08, 98.41, "test")
	if ch.MapID != 53 {
		t.Fatal("entered the battle before it opened")
	}
	grbNow = func() time.Time { return sunday(21, 30) }
	TickGuildRanking(sunday(21, 30), logger)
	s.applyTeleport(50, 507.78, 3.08, 98.41, "test")
	zone := ch.MapID
	if !isInstanceZone(zone) || realMap(zone) != 50 || g.grbEntries != 1 {
		t.Fatalf("battle zone %d (map %d) entries %d", zone, realMap(zone), g.grbEntries)
	}
	var mob MapMob
	for _, m := range EntitiesOnMap(zone).Mobs {
		if m.MoneyMin == 0 && m.MoneyMax > 1 {
			mob = m
			break
		}
	}
	if mob.ID == 0 {
		t.Fatal("no battle mob in the instance")
	}
	if !grbKill(zone, &mob, ch.ID) || g.grbPoints != mob.MoneyMax {
		t.Fatalf("points %d after killing a %d-point mob", g.grbPoints, mob.MoneyMax)
	}
	if grbKill(53, &mob, ch.ID) {
		t.Fatal("points outside the battle map")
	}

	TickGuildRanking(sunday(22, 1), logger)
	tp := s.player.TakeTeleport()
	if tp == nil || tp.MapID != 53 {
		t.Fatalf("not sent to the bind point at the close: %+v", tp)
	}
	s.applyTeleport(tp.MapID, tp.X, tp.Y, tp.Z, "test close")
	TickGuildRanking(sunday(22, 2), logger)
	if len(instanceZones(50)) != 0 {
		t.Fatal("empty instance kept after the close")
	}
	points := mob.MoneyMax
	TickGuildRanking(sunday(22, 32), logger)
	want := points + min(25, points/2)
	if g.Rank != 1 || g.Points != want {
		t.Fatalf("rank %d points %d, want 1 / %d", g.Rank, g.Points, want)
	}
	var rank int
	pool, _ := db.Pool(db.GameData)
	pool.QueryRow(`SELECT Rank FROM dbo.GuildDetails WHERE GuildID = @p1`, int32(guildID)).Scan(&rank)
	if rank != 1 {
		t.Fatalf("stored rank %d", rank)
	}
}

func itoa(v uint16) string {
	if v == 0 {
		return "0"
	}
	var b []byte
	for v > 0 {
		b = append([]byte{byte('0' + v%10)}, b...)
		v /= 10
	}
	return string(b)
}
