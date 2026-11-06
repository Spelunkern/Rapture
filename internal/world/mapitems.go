package world

import (
	"sync"
	"sync/atomic"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

const (
	// A dropped item is reserved for its owner for 30 s, open to anyone
	// until 60 s, then despawns.
	mapItemOwnerWindow = 30 * time.Second
	mapItemDespawnAge  = 60 * time.Second
	moneyItemType      = 26
)

// MapItem is an item or gold lying on a map.
type MapItem struct {
	ID               uint32
	Item             db.Item
	Money            uint32
	OwnerID          uint32
	DroppedAt        time.Time
	PosX, PosY, PosZ float32
}

var (
	mapItemsMu  sync.Mutex
	mapItems    = map[uint16][]MapItem{}
	nextMapItem atomic.Uint32
)

func nextMapItemID() uint32 { return max(nextMapItem.Add(1), 1) }

func addMapItem(mapID uint16, it MapItem) {
	mapItemsMu.Lock()
	mapItems[mapID] = append(mapItems[mapID], it)
	mapItemsMu.Unlock()
}

func findMapItem(mapID uint16, id uint32) (MapItem, bool) {
	mapItemsMu.Lock()
	defer mapItemsMu.Unlock()
	for _, it := range mapItems[mapID] {
		if it.ID == id {
			return it, true
		}
	}
	return MapItem{}, false
}

func removeMapItem(mapID uint16, id uint32) {
	mapItemsMu.Lock()
	defer mapItemsMu.Unlock()
	items := mapItems[mapID]
	for i, it := range items {
		if it.ID == id {
			mapItems[mapID] = append(items[:i], items[i+1:]...)
			return
		}
	}
}

func setMapItemCount(mapID uint16, id uint32, count uint8) (MapItem, bool) {
	mapItemsMu.Lock()
	defer mapItemsMu.Unlock()
	for i := range mapItems[mapID] {
		if mapItems[mapID][i].ID == id {
			mapItems[mapID][i].Item.Count = count
			return mapItems[mapID][i], true
		}
	}
	return MapItem{}, false
}

func itemsOnMap(mapID uint16) []MapItem {
	mapItemsMu.Lock()
	defer mapItemsMu.Unlock()
	return append([]MapItem(nil), mapItems[mapID]...)
}

type expiredItem struct {
	MapID uint16
	Item  MapItem
}

func sweepExpiredMapItems() []expiredItem {
	mapItemsMu.Lock()
	defer mapItemsMu.Unlock()
	var out []expiredItem
	for mapID, items := range mapItems {
		kept := items[:0]
		for _, it := range items {
			if time.Since(it.DroppedAt) >= mapItemDespawnAge {
				out = append(out, expiredItem{mapID, it})
			} else {
				kept = append(kept, it)
			}
		}
		mapItems[mapID] = kept
	}
	return out
}

func buildMapAddItem(m *MapItem) []byte {
	w := packet.New(packet.MapAddItem)
	w.U32(m.ID)
	w.U8(1)
	if m.Money > 0 {
		w.Bytes([]byte{moneyItemType, 0, 1})
	} else {
		w.Bytes([]byte{m.Item.Type, m.Item.TypeID, m.Item.Count})
	}
	w.F32(m.PosX)
	w.F32(m.PosY)
	w.F32(m.PosZ)
	if m.Money > 0 {
		w.U32(0)
	} else {
		w.U32(m.OwnerID)
	}
	return w.MustFinish()
}

func buildMapRemoveItem(id uint32) []byte {
	w := packet.New(packet.MapRemoveItem)
	w.U32(id)
	return w.MustFinish()
}
