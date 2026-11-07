package world

import (
	"sort"
	"sync"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// The guild ranking battle follows ps_game.exe: a Map.ini CreateType R map
// (map type 5) opens on its CreateTime slots (CMap::IsGuildRankMapOpenTime
// 0x00428d20); each guild enters its own instance (CWorld::GetZoneByMapID),
// every entry counting for the guild (CUser::ZoneEnterUserMove, +0xa0); a
// mob with Money1 0 and Money2 > 0 killed there gives its Money2 to the
// killer's guild instead of gold (CMob::ItemDrop -> CGuild::SendRankingZone
// 0x0042c6e0, 0x0D27 to the members in the battle); notices go out on
// GetGuildRankNotices' minutes (0x0D26); at the close everyone is moved to
// the bind point (CZone::UpdateCloseGuildRanking -> MoveAllBindPos) and 30
// minutes later CGuildMgr::UpdateGuildPointRank 0x00432b70 ranks each
// country's guilds and stores the result.

const (
	grbCreateType   = "R"
	grbRankDelay    = 30 * time.Minute // CWorldSendThread::Run +0x1b7740
	grbNoticeOpen   = 100
	grbNoticeEnd10  = 101
	grbNoticeEnd1   = 102
	grbUnranked     = 31
	grbRankedGuilds = 30
	grbEntryWeight  = 25
)

// grbNow is the clock the battle entry checks (tests replace it).
var grbNow = time.Now

// grbSlot is one Map.ini CreateTime entry.
type grbSlot struct{ Day, Hour, Hours int }

// slotHours is the slot's start hour and end hour (past 24 runs into the
// next day, as the original's wrap).
func (s grbSlot) end() int { return s.Hour + s.Hours }

func nextDay(d int) int {
	if d == 6 {
		return 0
	}
	return d + 1
}

// grbOpen is IsGuildRankMapOpenTime.
func grbOpen(slots []grbSlot, now time.Time) bool {
	day, hour := int(now.Weekday()), now.Hour()
	for _, s := range slots {
		end := s.end()
		if day == s.Day && hour >= s.Hour && hour < end {
			return true
		}
		if end > 24 && day == nextDay(s.Day) && hour < end-24 {
			return true
		}
	}
	return false
}

// grbNotice is GetGuildRankNotices: the start hour at 30 and 10 minutes
// before opening, 100 at the opening, 101 ten and 102 one minute before
// the close.
func grbNotice(slots []grbSlot, now time.Time) (uint8, bool) {
	day, hour, minute := int(now.Weekday()), now.Hour(), now.Minute()
	for _, s := range slots {
		end := s.end()
		if day == s.Day {
			if hour < s.Hour || hour >= end {
				if s.Hour != 0 && hour == s.Hour-1 && (minute == 30 || minute == 50) {
					return uint8(s.Hour), true
				}
				continue
			}
			switch {
			case hour == s.Hour && minute == 0:
				return grbNoticeOpen, true
			case hour == end-1 && minute == 50:
				return grbNoticeEnd10, true
			case hour == end-1 && minute == 59:
				return grbNoticeEnd1, true
			}
		} else if end > 24 && day == nextDay(s.Day) && hour < end-24 {
			if hour == end-24-1 && minute == 50 {
				return grbNoticeEnd10, true
			}
			if hour == end-24-1 && minute == 59 {
				return grbNoticeEnd1, true
			}
		}
	}
	return 0, false
}

var grbState = struct {
	sync.Mutex
	open        map[uint16]bool   // map -> was open at the last tick
	instance    map[uint32]uint16 // guild id -> its battle zone
	lastMinute  int64
	rankAt      time.Time
	maxPoints   [2]uint32 // g_dwRankingGetMaxPoint per country
	maxPointsID [2]uint32
}{open: map[uint16]bool{}, instance: map[uint32]uint16{}}

func grbMaps() []uint16 {
	var out []uint16
	for id, z := range mapConfig.Zones {
		if z.CreateType == grbCreateType {
			out = append(out, id)
		}
	}
	return out
}

// enterGuildRanking resolves a teleport to a battle map: the guild's own
// instance while the battle is open, otherwise no zone (the move fails).
func (s *Session) enterGuildRanking(mapID uint16) (uint16, bool) {
	if s.player == nil {
		return 0, false
	}
	guildMu.Lock()
	g := s.player.guild.guild
	guildMu.Unlock()
	cfg := mapConfig.Zones[mapID]
	if g == nil || !grbOpen(cfg.Schedule, grbNow()) {
		return 0, false
	}
	grbState.Lock()
	zone, ok := grbState.instance[g.ID]
	if ok && realMap(zone) != mapID {
		ok = false
	}
	if !ok {
		z, err := createInstance(mapID, g.ID)
		if err != nil {
			grbState.Unlock()
			s.log.Printf("Rapture World guild ranking instance failed: %v", err)
			return 0, false
		}
		zone = z
		grbState.instance[g.ID] = zone
	}
	grbState.Unlock()
	if s.char.MapID != zone {
		guildMu.Lock()
		g.grbEntries++
		guildMu.Unlock()
	}
	return zone, true
}

// grbKill is CMob::ItemDrop's map type 5 branch; true when the mob gave
// ranking points instead of gold.
func grbKill(zone uint16, mob *MapMob, killerID uint32) bool {
	if mapCreateType(zone) != grbCreateType || mob.MoneyMin != 0 || mob.MoneyMax == 0 {
		return false
	}
	p := FindPlayerByID(killerID)
	if p == nil {
		return true
	}
	guildMu.Lock()
	defer guildMu.Unlock()
	g := p.guild.guild
	if g == nil {
		return true
	}
	g.grbPoints += uint32(min(mob.MoneyMax, 65535))
	c := int(g.Country) & 1
	grbState.Lock()
	if g.grbPoints > grbState.maxPoints[c] {
		grbState.maxPoints[c], grbState.maxPointsID[c] = g.grbPoints, g.ID
	}
	maxPoints, maxID := grbState.maxPoints[c], grbState.maxPointsID[c]
	grbState.Unlock()
	w := packet.New(packet.GuildRankingPoints)
	w.U32(g.grbPoints)
	w.U32(maxPoints)
	w.U32(maxID)
	frame := w.MustFinish()
	for _, m := range g.members {
		if m.player != nil && mapCreateType(m.player.MapID()) == grbCreateType {
			m.player.Send(frame, "0x0D27 guild ranking points")
		}
	}
	return true
}

// TickGuildRanking runs the battle schedule: notices, the close and the
// ranking 30 minutes after it.
func TickGuildRanking(now time.Time, log *logx.Logger) {
	minute := now.Unix() / 60
	grbState.Lock()
	newMinute := minute != grbState.lastMinute
	grbState.lastMinute = minute
	grbState.Unlock()
	for _, mapID := range grbMaps() {
		cfg := mapConfig.Zones[mapID]
		open := grbOpen(cfg.Schedule, now)
		if newMinute {
			if t, ok := grbNotice(cfg.Schedule, now); ok {
				w := packet.New(packet.GuildRankingNotice)
				w.U16(mapID)
				w.U8(t)
				broadcast(AllPlayers(), w.MustFinish(), "0x0D26 guild ranking notice", log)
			}
		}
		grbState.Lock()
		wasOpen := grbState.open[mapID]
		grbState.open[mapID] = open
		if wasOpen && !open {
			grbState.rankAt = now.Add(grbRankDelay)
		}
		grbState.Unlock()
		if !open {
			grbCloseInstances(mapID, log)
		}
	}
	grbState.Lock()
	due := !grbState.rankAt.IsZero() && !now.Before(grbState.rankAt)
	if due {
		grbState.rankAt = time.Time{}
	}
	grbState.Unlock()
	if due {
		updateGuildRanking(log)
	}
}

// grbCloseInstances is MoveAllBindPos at the close, then the empty
// instances are removed.
func grbCloseInstances(mapID uint16, log *logx.Logger) {
	for _, zone := range instanceZones(mapID) {
		players := PlayersOnMap(zone)
		for _, p := range players {
			c := p.Snapshot()
			if m, x, y, z, ok := bindPositionFound(zone, p.Faction(), c.PosX, c.PosY, c.PosZ); ok {
				p.QueueTeleport(m, x, y, z)
			}
		}
		if len(players) == 0 {
			grbState.Lock()
			for id, z := range grbState.instance {
				if z == zone {
					delete(grbState.instance, id)
				}
			}
			grbState.Unlock()
			destroyInstance(zone)
			log.Printf("Rapture World guild ranking instance %d (map %d) closed", zone, mapID)
		}
	}
}

// updateGuildRanking is CGuildMgr::UpdateGuildPointRank: the value is the
// battle points plus min(25 x entries + duel bonus, points / 2); guilds
// with a value are ranked per country by it (equal values: the first guild
// keeps the place, the others their rank, as the original's map insert),
// the top 30 get 1-30 and the rest 31. Every guild is stored and each
// country gets its 0x0D37 list.
func updateGuildRanking(log *logx.Logger) {
	guildMu.Lock()
	type entry struct {
		g     *guild
		value uint32
	}
	var ranked [2][]entry
	ids := make([]uint32, 0, len(guildsByID))
	for id := range guildsByID {
		ids = append(ids, id)
	}
	sort.Slice(ids, func(i, j int) bool { return ids[i] < ids[j] })
	var rows []db.GuildRow
	seen := [2]map[uint32]bool{{}, {}}
	for _, id := range ids {
		g := guildsByID[id]
		value := uint32(0)
		if g.grbPoints != 0 {
			bonus := g.grbEntries*grbEntryWeight + g.duelBonus
			value = g.grbPoints + min(bonus, g.grbPoints/2)
		}
		g.grbPoints, g.grbEntries, g.duelBonus = 0, 0, 0
		g.Points = value
		c := int(g.Country) & 1
		if value == 0 {
			g.Rank, g.UseHouse = grbUnranked, 0
		} else if !seen[c][value] {
			seen[c][value] = true
			ranked[c] = append(ranked[c], entry{g, value})
		}
	}
	lists := [2][]byte{}
	for c := range ranked {
		sort.SliceStable(ranked[c], func(i, j int) bool { return ranked[c][i].value > ranked[c][j].value })
		w := packet.New(packet.GuildRankingList)
		count := 0
		for i, e := range ranked[c] {
			if i < grbRankedGuilds {
				e.g.Rank = uint8(i + 1)
				count++
			} else {
				e.g.Rank, e.g.UseHouse = grbUnranked, 0
			}
		}
		w.U8(uint8(count))
		for _, e := range ranked[c][:count] {
			w.U32(e.g.ID)
			w.U32(e.value)
			w.U8(e.g.Rank)
		}
		lists[c] = w.MustFinish()
	}
	for _, id := range ids {
		g := guildsByID[id]
		rows = append(rows, db.GuildRow{ID: g.ID, Points: g.Points, Rank: g.Rank, Etin: g.Etin, Remark: g.Remark,
			BuyHouse: g.BuyHouse, UseHouse: g.UseHouse, EtinReturn: g.EtinReturn, KeepEtin: g.KeepEtin})
	}
	grbState.Lock()
	grbState.maxPoints, grbState.maxPointsID = [2]uint32{}, [2]uint32{}
	grbState.Unlock()
	guildMu.Unlock()
	for _, r := range rows {
		if err := db.SaveGuildRanking(r); err != nil {
			log.Printf("Rapture World guild ranking save failed guild=%d: %v", r.ID, err)
		}
	}
	for c, frame := range lists {
		guildSendCountry(uint8(c), frame, "0x0D37 guild ranking")
	}
	log.Printf("Rapture World guild ranking updated guilds=%d", len(rows))
}
