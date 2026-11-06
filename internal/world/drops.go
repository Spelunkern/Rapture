package world

import (
	"fmt"
	"math/rand/v2"
	"path/filepath"
	"sort"
	"strconv"
	"sync"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
)

// Mob loot as the original server rolls it (CMob loot path, recovered by
// Eternity's mob_drops): MobItems orders 1-6 use a per-cent rate and 7-9 a
// per-million rate (order 0 is unused); each hit picks a random item of
// that grade, subject to the item's Drop time limit; mobs with gold also
// try the SequenceItemCreate.ini table; gold drops 30% of the time as
// Money1 + rand(Money2).

func chanceSucceeds(percent uint32) bool {
	switch {
	case percent == 0:
		return false
	case percent >= 100:
		return true
	}
	return rand.Uint32N(100) < percent
}

// dropLimitIntervals are the original Drop selectors (ms per window).
var dropLimitIntervals = [7]uint32{0, 1440000, 14400000, 28800000, 43200000, 86400000, 259200000}

type gradeEntry struct {
	def *db.ItemDef
	// Drop limit: at most one drop per window, the window start
	// randomized within its last tenth.
	limited          bool
	delay, randDelay time.Duration
	nextAt           time.Time
}

type sequenceGroup struct {
	divisor uint32
	grades  [10]uint16 // by mob level / 10
}

var dropCatalog struct {
	mu        sync.Mutex
	grades    map[uint16][]*gradeEntry
	sequences []sequenceGroup
}

func isQuestItemType(t uint8) bool { return t == 27 || t == 28 || t == 29 }

// buildDropCatalog groups items by grade, unrestricted items first, then
// those with a Server or Drop restriction, as CGameData's init does.
func buildDropCatalog() {
	defs := db.MustDefs()
	var all []*db.ItemDef
	for _, d := range defs.Items {
		if d.Grade > 0 && d.Grade <= 1024 && d.Type != 0 && d.TypeID != 0 && !isQuestItemType(d.Type) {
			all = append(all, d)
		}
	}
	sort.Slice(all, func(i, j int) bool {
		if all[i].Type != all[j].Type {
			return all[i].Type < all[j].Type
		}
		return all[i].TypeID < all[j].TypeID
	})
	grades := map[uint16][]*gradeEntry{}
	for pass := 0; pass < 2; pass++ {
		for _, d := range all {
			restricted := (d.Server != 0 && d.Server < 10) || d.Drop != 0
			if restricted != (pass == 1) {
				continue
			}
			e := &gradeEntry{def: d}
			if d.Drop != 0 && int(d.Drop) < len(dropLimitIntervals) {
				interval := time.Duration(dropLimitIntervals[d.Drop]) * time.Millisecond
				e.limited, e.randDelay = true, interval/10
				e.delay = interval - e.randDelay
			}
			grades[d.Grade] = append(grades[d.Grade], e)
		}
	}
	dropCatalog.mu.Lock()
	dropCatalog.grades = grades
	dropCatalog.mu.Unlock()
}

// loadSequenceItems reads SequenceItemCreate.ini.
func loadSequenceItems(log *logx.Logger) error {
	ini, err := readIni(filepath.Join(gameDataDir(), "SequenceItemCreate.ini"))
	if err != nil {
		return err
	}
	count, _ := strconv.Atoi(ini["SEQUENCE_GROUP_COUNT"]["COUNT"])
	var groups []sequenceGroup
	for i := 1; i <= count; i++ {
		sec := ini[fmt.Sprintf("SEQUENCE_GROUP_%d", i)]
		div, _ := strconv.ParseUint(sec["SEQUENCE"], 10, 32)
		g := sequenceGroup{divisor: uint32(div)}
		for l := range g.grades {
			v, _ := strconv.Atoi(sec[fmt.Sprintf("GRADE_%d", l)])
			g.grades[l] = uint16(max(v, 0))
		}
		groups = append(groups, g)
	}
	dropCatalog.mu.Lock()
	dropCatalog.sequences = groups
	dropCatalog.mu.Unlock()
	log.Printf("Rapture World loaded SequenceItemCreate.ini groups=%d", len(groups))
	return nil
}

// pickGradeItem is MobDropCatalog::select: a random item of the grade,
// dropped only if its time limit allows it.
func pickGradeItem(grade uint16) *db.ItemDef {
	dropCatalog.mu.Lock()
	defer dropCatalog.mu.Unlock()
	list := dropCatalog.grades[grade]
	if len(list) == 0 {
		return nil
	}
	e := list[rand.IntN(len(list))]
	if e.limited {
		now := time.Now()
		if now.Before(e.nextAt) {
			return nil
		}
		jitter := time.Duration(0)
		if e.randDelay > 0 {
			jitter = time.Duration(rand.Int64N(int64(e.randDelay)))
		}
		e.nextAt = now.Add(e.delay + jitter)
	}
	return e.def
}

// sequenceItem is MobDropCatalog::sequence: a kill whose object id is a
// multiple of a group's SEQUENCE drops from that group's grade for the
// mob's level band.
func sequenceItem(level uint16, objectID uint32) *db.ItemDef {
	if level == 0 || level >= 100 || objectID == 0 {
		return nil
	}
	dropCatalog.mu.Lock()
	groups := dropCatalog.sequences
	dropCatalog.mu.Unlock()
	for _, g := range groups {
		if g.divisor != 0 && objectID%g.divisor == 0 {
			if grade := g.grades[level/10]; grade != 0 {
				return pickGradeItem(grade)
			}
			return nil
		}
	}
	return nil
}

// dropPosition is CMob::GetDropPosRand: within half a unit of the mob, with
// Y 0 (the server has no terrain heights; the client places it).
func dropPosition(x, z float32) (float32, float32) {
	return x + float32(rand.IntN(1000))/1000 - 0.5, z + float32(rand.IntN(1000))/1000 - 0.5
}

// rollMobDrops rolls one kill's loot, registers it on the map and returns
// the 0x0401 packets.
//
// With two or more party members in range, the gold is split evenly among
// them instead of dropped (at least 1 each).
func rollMobDrops(mapID uint16, mob *MapMob, ownerID uint32, party []*ConnectedPlayer, log logPrinter) [][]byte {
	defs := db.MustDefs()
	var packets [][]byte
	place := func(def *db.ItemDef, money uint32) {
		x, z := dropPosition(mob.PosX, mob.PosZ)
		mi := MapItem{ID: nextMapItemID(), OwnerID: ownerID, DroppedAt: time.Now(), PosX: x, PosZ: z}
		if def != nil {
			mi.Item = db.Item{Type: def.Type, TypeID: def.TypeID, Quality: def.Quality, Count: 1}
		} else {
			mi.Item, mi.Money = db.Item{Type: moneyItemType, Count: 1}, money
		}
		addMapItem(mapID, mi)
		packets = append(packets, buildMapAddItem(&mi))
	}
	for _, d := range defs.MobDrops[mob.MobID] {
		if d.ItemOrder < 1 || d.ItemOrder > 9 {
			continue
		}
		var roll uint32
		if d.ItemOrder < 7 {
			roll = rand.Uint32N(100)
		} else {
			roll = rand.Uint32N(1_000_000)
		}
		if roll >= d.DropRate {
			continue
		}
		if def := pickGradeItem(d.Grade); def != nil {
			place(def, 0)
			log.Printf("Rapture World mob drop mob_id=%d order=%d grade=%d item=%d/%d", mob.MobID, d.ItemOrder, d.Grade, def.Type, def.TypeID)
		}
	}
	if mob.MoneyMin > 0 {
		if def := sequenceItem(mob.Level, mob.ID); def != nil {
			place(def, 0)
			log.Printf("Rapture World sequence drop mob_id=%d item=%d/%d", mob.MobID, def.Type, def.TypeID)
		}
	}
	if rand.IntN(100) < 30 {
		amount := mob.MoneyMin
		if mob.MoneyMax > 0 {
			amount += rand.Uint32N(mob.MoneyMax)
		}
		switch {
		case amount == 0:
		case len(party) > 1:
			share := max(amount/uint32(len(party)), 1)
			for _, m := range party {
				m.QueueGold(share)
			}
		default:
			place(nil, amount)
		}
	}
	return packets
}
