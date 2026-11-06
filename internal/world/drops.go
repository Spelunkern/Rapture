package world

import (
	"math/rand/v2"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
)

func chanceSucceeds(percent uint32) bool {
	switch {
	case percent == 0:
		return false
	case percent >= 100:
		return true
	}
	return rand.Uint32N(100) < percent
}

func rollMoneyAmount(base, extra uint32) (uint32, bool) {
	if base == 0 && extra == 0 || !chanceSucceeds(25) {
		return 0, false
	}
	var add uint32
	if extra > 0 {
		add = rand.Uint32N(extra + 1)
	}
	return satAdd(base, add), true
}

// rollMobDrops rolls PS_GameDefs.dbo.MobItems (grade + rate) and the gold
// drop, registers the map items and returns their 0x0401 packets.
func rollMobDrops(mapID, mobID uint16, moneyMin, moneyMax, ownerID uint32, x, y, z float32, log logPrinter) [][]byte {
	defs := db.MustDefs()
	var packets [][]byte
	for _, d := range defs.MobDrops[mobID] {
		if !chanceSucceeds(min(d.DropRate, 100)) {
			continue
		}
		items := defs.ItemsByGrade[d.Grade]
		if len(items) == 0 {
			log.Printf("Rapture World mob drop skipped mob_id=%d order=%d grade=%d (grade has no items)", mobID, d.ItemOrder, d.Grade)
			continue
		}
		def := items[rand.IntN(len(items))]
		mi := MapItem{ID: nextMapItemID(), Item: db.Item{Type: def.Type, TypeID: def.TypeID, Quality: def.Quality, Count: 1},
			OwnerID: ownerID, DroppedAt: time.Now(), PosX: x, PosY: y, PosZ: z}
		addMapItem(mapID, mi)
		packets = append(packets, buildMapAddItem(&mi))
		log.Printf("Rapture World mob drop mob_id=%d grade=%d rate=%d item=%d/%d map_item_id=%d", mobID, d.Grade, d.DropRate, def.Type, def.TypeID, mi.ID)
	}
	if amount, ok := rollMoneyAmount(moneyMin, moneyMax); ok {
		mi := MapItem{ID: nextMapItemID(), Item: db.Item{Type: moneyItemType, Count: 1}, Money: amount,
			OwnerID: ownerID, DroppedAt: time.Now(), PosX: x, PosY: y, PosZ: z}
		addMapItem(mapID, mi)
		packets = append(packets, buildMapAddItem(&mi))
		log.Printf("Rapture World mob money drop mob_id=%d amount=%d map_item_id=%d", mobID, amount, mi.ID)
	}
	return packets
}
