package world

import (
	"encoding/binary"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// The account warehouse (PS_GameData.dbo.UserStoredItems) is bag 100 in
// the client's move packets.
const (
	WarehouseBag   = 100
	WarehouseSlots = 120
)

func findStored(items []db.Item, slot uint8) int {
	for i := range items {
		if items[i].Slot == slot {
			return i
		}
	}
	return -1
}

func buildWarehouseItemList(items []db.Item) []byte {
	w := packet.New(packet.WarehouseItemList)
	w.U32(0) // warehouse gold is not used
	w.U8(uint8(len(items)))
	for i := range items {
		it := &items[i]
		w.U8(it.Slot)
		w.U8(it.Type)
		w.U8(it.TypeID)
		w.U16(it.Quality)
		w.Bytes(it.Gems[:])
		w.U8(it.Count)
		w.FixedString(it.CraftName, 21)
	}
	return w.MustFinish()
}

// withMoney appends the character's money to a withdraw 0x0204 and
// rewrites the frame length.
func withMoney(frame []byte, money uint32) []byte {
	frame = binary.LittleEndian.AppendUint32(frame, money)
	binary.LittleEndian.PutUint16(frame, uint16(len(frame)))
	return frame
}

func copyPtr(it *db.Item) *db.Item {
	if it == nil {
		return nil
	}
	c := *it
	return &c
}

func (s *Session) handleWarehouseMove(srcBag, srcSlot, destBag, destSlot uint8) {
	if (srcBag == WarehouseBag && srcSlot >= WarehouseSlots) || (destBag == WarehouseBag && destSlot >= WarehouseSlots) {
		return
	}
	var frame []byte
	switch {
	case srcBag == WarehouseBag && destBag == WarehouseBag:
		frame = s.warehouseRearrange(srcSlot, destSlot)
	case srcBag == WarehouseBag:
		if !isValidItemPosition(destBag, destSlot) || destBag == 0 {
			return
		}
		frame = withMoney(s.warehouseWithdraw(srcSlot, destBag, destSlot), s.char.Money)
	default:
		if !isValidItemPosition(srcBag, srcSlot) || srcBag == 0 {
			return
		}
		frame = s.warehouseDeposit(srcBag, srcSlot, destSlot)
	}
	if frame != nil {
		s.send(frame, "0x0204 warehouse item move")
	}
}

func (s *Session) warehouseRearrange(src, dest uint8) []byte {
	uid := s.accountID
	si, di := findStored(s.stored, src), findStored(s.stored, dest)
	if si < 0 {
		var d *db.Item
		if di >= 0 {
			d = &s.stored[di]
		}
		return buildBagToBag(WarehouseBag, src, nil, WarehouseBag, dest, d)
	}
	if src == dest {
		return buildBagToBag(WarehouseBag, src, &s.stored[si], WarehouseBag, dest, &s.stored[si])
	}
	if di >= 0 {
		a, b := &s.stored[si], &s.stored[di]
		if ms := maxStackOf(a.Type, a.TypeID); itemsMatch(a, b) && ms > 1 && b.Count < ms {
			moved := min(a.Count, ms-b.Count)
			b.Count += moved
			a.Count -= moved
			db.SaveStoredItem(uid, b)
			bCopy := *b
			var aAfter *db.Item
			if a.Count == 0 {
				db.DeleteStoredItems(uid, a.UID)
				s.stored = append(s.stored[:si], s.stored[si+1:]...)
			} else {
				db.SaveStoredItem(uid, a)
				aAfter = copyPtr(a)
			}
			return buildBagToBag(WarehouseBag, src, aAfter, WarehouseBag, dest, &bCopy)
		}
		a.Slot, b.Slot = dest, src
		db.SaveStoredItem(uid, a)
		db.SaveStoredItem(uid, b)
		return buildBagToBag(WarehouseBag, src, b, WarehouseBag, dest, a)
	}
	s.stored[si].Slot = dest
	db.SaveStoredItem(uid, &s.stored[si])
	return buildBagToBag(WarehouseBag, src, nil, WarehouseBag, dest, &s.stored[si])
}

func (s *Session) warehouseDeposit(srcBag, srcSlot, dest uint8) []byte {
	ch := s.char
	si, di := findItem(s.items, srcBag, srcSlot), findStored(s.stored, dest)
	if si < 0 {
		var d *db.Item
		if di >= 0 {
			d = &s.stored[di]
		}
		return buildBagToBag(srcBag, srcSlot, nil, WarehouseBag, dest, d)
	}
	src := s.items[si]
	if di >= 0 {
		b := &s.stored[di]
		if ms := maxStackOf(src.Type, src.TypeID); itemsMatch(&src, b) && ms > 1 && b.Count < ms {
			moved := min(src.Count, ms-b.Count)
			b.Count += moved
			db.SaveStoredItem(s.accountID, b)
			s.items[si].Count -= moved
			db.SaveCharacterItem(ch.ID, &s.items[si])
			var after *db.Item
			if s.items[si].Count == 0 {
				s.items = append(s.items[:si], s.items[si+1:]...)
			} else {
				after = copyPtr(&s.items[si])
			}
			return buildBagToBag(srcBag, srcSlot, after, WarehouseBag, dest, b)
		}
		return buildBagToBag(srcBag, srcSlot, &src, WarehouseBag, dest, b)
	}
	stored := src
	stored.UID, stored.Bag, stored.Slot = 0, 0, dest
	if err := db.AddStoredItem(s.accountID, &stored); err != nil {
		s.log.Printf("Rapture World warehouse deposit failed: %v", err)
		return buildBagToBag(srcBag, srcSlot, &src, WarehouseBag, dest, nil)
	}
	if err := db.DeleteCharacterItems(ch.ID, src.UID); err != nil {
		s.log.Printf("Rapture World warehouse deposit cleanup failed: %v", err)
	}
	s.items = append(s.items[:si], s.items[si+1:]...)
	s.stored = append(s.stored, stored)
	s.log.Printf("Rapture World deposited item %d/%d into warehouse slot=%d", src.Type, src.TypeID, dest)
	return buildBagToBag(srcBag, srcSlot, nil, WarehouseBag, dest, &stored)
}

func (s *Session) warehouseWithdraw(src, destBag, destSlot uint8) []byte {
	ch := s.char
	si, di := findStored(s.stored, src), findItem(s.items, destBag, destSlot)
	if si < 0 {
		var d *db.Item
		if di >= 0 {
			d = &s.items[di]
		}
		return buildBagToBag(WarehouseBag, src, nil, destBag, destSlot, d)
	}
	a := s.stored[si]
	if di >= 0 {
		b := &s.items[di]
		if ms := maxStackOf(a.Type, a.TypeID); itemsCanStack(&a, b) && ms > 1 && b.Count < ms {
			moved := min(a.Count, ms-b.Count)
			b.Count += moved
			db.SaveCharacterItem(ch.ID, b)
			bCopy := *b
			s.stored[si].Count -= moved
			var after *db.Item
			if s.stored[si].Count == 0 {
				db.DeleteStoredItems(s.accountID, a.UID)
				s.stored = append(s.stored[:si], s.stored[si+1:]...)
			} else {
				db.SaveStoredItem(s.accountID, &s.stored[si])
				after = copyPtr(&s.stored[si])
			}
			return buildBagToBag(WarehouseBag, src, after, destBag, destSlot, &bCopy)
		}
		return buildBagToBag(WarehouseBag, src, &a, destBag, destSlot, b)
	}
	it := a
	it.UID, it.Bag, it.Slot = 0, destBag, destSlot
	if err := db.AddCharacterItem(ch.ID, &it); err != nil {
		s.log.Printf("Rapture World warehouse withdraw failed: %v", err)
		return buildBagToBag(WarehouseBag, src, &a, destBag, destSlot, nil)
	}
	if err := db.DeleteStoredItems(s.accountID, a.UID); err != nil {
		s.log.Printf("Rapture World warehouse withdraw cleanup failed: %v", err)
	}
	s.stored = append(s.stored[:si], s.stored[si+1:]...)
	s.items = append(s.items, it)
	s.log.Printf("Rapture World withdrew item %d/%d from warehouse slot=%d to (%d,%d)", a.Type, a.TypeID, src, destBag, destSlot)
	return buildBagToBag(WarehouseBag, src, nil, destBag, destSlot, &it)
}

// --- NPC shop ---

func buildShopSellResult(result, bag, slot, itemType, typeID, remaining uint8, money uint32) []byte {
	w := packet.New(packet.ShopSell)
	w.Bytes([]byte{result, bag, slot, itemType, typeID, remaining})
	w.U32(money)
	return w.MustFinish()
}

// handleShopSell (0x0703): bag, slot, count. Items.Sell is per unit.
func (s *Session) handleShopSell(p []byte) {
	if len(p) < 5 || s.char == nil {
		return
	}
	ch := s.char
	bag, slot, req := p[2], p[3], max(p[4], 1)
	i := findItem(s.items, bag, slot)
	if i < 0 {
		s.send(buildShopSellResult(1, bag, slot, 0, 0, 0, ch.Money), "0x0703 shop sell rejected")
		return
	}
	it := s.items[i]
	def := db.MustDefs().Item(it.Type, it.TypeID)
	if def == nil {
		s.send(buildShopSellResult(1, bag, slot, 0, 0, 0, ch.Money), "0x0703 shop sell rejected")
		return
	}
	sold := min(req, it.Count)
	proceeds := satMul(uint32(max(def.Sell, 0)), uint32(sold))
	s.items[i].Count -= sold
	if err := db.SaveCharacterItem(ch.ID, &s.items[i]); err != nil {
		s.log.Printf("Rapture World shop sell failed: %v", err)
		s.items[i].Count = it.Count
		return
	}
	remaining := s.items[i].Count
	if remaining == 0 {
		s.items = append(s.items[:i], s.items[i+1:]...)
	}
	ch.Money = uint32(min(uint64(ch.Money)+uint64(proceeds), db.MoneyCap))
	s.persist()
	s.log.Printf("Rapture World sold item %d/%d count=%d proceeds=%d", it.Type, it.TypeID, sold, proceeds)
	s.send(buildShopSellResult(0, bag, slot, it.Type, it.TypeID, remaining, ch.Money), "0x0703 shop sell")
}
