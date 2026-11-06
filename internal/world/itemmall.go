package world

import (
	"errors"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

const (
	pointBuyOK              = 0
	pointBuyNoProduct       = 1
	pointBuyNotEnoughPoints = 2
	pointBuyNotEnoughSlots  = 3

	giftProductNotFound = 6
	giftNotEnoughPoints = 10
)

func time32() uint32 { return uint32(time.Now().Unix()) }

type grantedItem struct{ Bag, Slot, Type, TypeID, Count uint8 }

func buildPointBuyResult(result uint8, points uint32, code string, cost uint32, items []grantedItem) []byte {
	w := packet.New(packet.PointBuy)
	w.U8(result)
	w.U32(points)
	w.FixedString(code, 21)
	w.U32(time32())
	w.U32(cost)
	w.U8(uint8(len(items)))
	for _, g := range items {
		w.Bytes([]byte{g.Bag, g.Slot, g.Type, g.TypeID, g.Count})
	}
	return w.MustFinish()
}

func buildGiftSendResult(result uint8, target string, points uint32, code string, cost uint32) []byte {
	w := packet.New(packet.PointGiftSend)
	w.U8(result)
	w.FixedString(target, 21)
	w.U32(points)
	w.FixedString(code, 21)
	w.U32(time32())
	w.U32(cost)
	return w.MustFinish()
}

// buildGiftBoxList (0x2606) lists UserStoredPointItems (slots 0-239).
func buildGiftBoxList(userUID uint32) ([]byte, error) {
	gifts, err := db.GiftBox(userUID)
	if err != nil {
		return nil, err
	}
	w := packet.New(packet.PointStoredItemList)
	w.U8(uint8(min(len(gifts), db.GiftBoxSlots)))
	for _, g := range gifts {
		w.Bytes([]byte{g.Slot, g.Type, g.TypeID, g.Count})
	}
	return w.MustFinish(), nil
}

func (s *Session) sendGiftBox() {
	if list, err := buildGiftBoxList(s.accountID); err == nil {
		s.send(list, "0x2606 gift box list")
	} else {
		s.log.Printf("Rapture World failed to read gift box: %v", err)
	}
}

// handlePointBuy (0x2602): product code.
func (s *Session) handlePointBuy(p []byte) {
	if s.char == nil {
		return
	}
	code := ""
	if len(p) > 2 {
		code = packet.FixedASCII(p[2:])
	}
	prod := db.MustDefs().Products[code]
	if prod == nil {
		s.send(buildPointBuyResult(pointBuyNoProduct, s.points, code, 0, nil), "0x2602 point buy result")
		return
	}
	if s.points < prod.BuyCost {
		s.send(buildPointBuyResult(pointBuyNotEnoughPoints, s.points, code, prod.BuyCost, nil), "0x2602 point buy result")
		return
	}
	need := 0
	for _, it := range prod.Items {
		ms := maxStackOf(it.Type, it.TypeID)
		need += (int(it.Count) + int(ms) - 1) / int(ms)
	}
	if need > len(freeInventorySlots(s.items)) {
		s.send(buildPointBuyResult(pointBuyNotEnoughSlots, s.points, code, prod.BuyCost, nil), "0x2602 point buy result")
		return
	}
	left, err := db.SpendPoints(s.accountID, s.char.ID, prod.BuyCost, code, "", 1)
	if err != nil {
		s.log.Printf("Rapture World point buy failed: %v", err)
		if errors.Is(err, db.ErrNotEnoughPoints) {
			s.points = left
		}
		s.send(buildPointBuyResult(pointBuyNotEnoughPoints, s.points, code, prod.BuyCost, nil), "0x2602 point buy result")
		return
	}
	s.points = left
	defs := db.MustDefs()
	var granted []grantedItem
	for _, pi := range prod.Items {
		def := defs.Item(pi.Type, pi.TypeID)
		if def == nil {
			continue
		}
		ms := def.MaxStack()
		for remaining := pi.Count; remaining > 0; {
			free := freeInventorySlots(s.items)
			if len(free) == 0 {
				break
			}
			n := min(remaining, ms)
			it := db.Item{Bag: free[0][0], Slot: free[0][1], Type: pi.Type, TypeID: pi.TypeID, Quality: def.Quality, Count: n, MakeType: "B"}
			if err := db.AddCharacterItem(s.char.ID, &it); err != nil {
				s.log.Printf("Rapture World point buy item insert failed: %v", err)
				break
			}
			s.items = append(s.items, it)
			s.send(buildAddItem(&it), "0x0205 point buy add item")
			granted = append(granted, grantedItem{it.Bag, it.Slot, it.Type, it.TypeID, n})
			remaining -= n
		}
	}
	s.log.Printf("Rapture World point buy product=%s cost=%d items=%d points_left=%d", code, prod.BuyCost, len(granted), s.points)
	s.send(buildPointBuyResult(pointBuyOK, s.points, code, prod.BuyCost, granted), "0x2602 point buy result")
}

// handleGiftSend (0x2603): product code[21], target name[21].
func (s *Session) handleGiftSend(p []byte) {
	if len(p) < 44 || s.char == nil {
		return
	}
	code, target := packet.FixedASCII(p[2:23]), packet.FixedASCII(p[23:44])
	reply := func(result uint8, cost uint32) {
		s.send(buildGiftSendResult(result, target, s.points, code, cost), "0x2603 gift send result")
	}
	prod := db.MustDefs().Products[code]
	if prod == nil || prod.BuyCost == 0 {
		reply(giftProductNotFound, 0)
		return
	}
	if s.points < prod.BuyCost {
		reply(giftNotEnoughPoints, prod.BuyCost)
		return
	}
	result, targetUID, err := db.StoreGift(target, s.accountID, prod)
	if err != nil {
		s.log.Printf("Rapture World gift store failed: %v", err)
	}
	if result != db.GiftOK {
		reply(result, prod.BuyCost)
		return
	}
	left, err := db.SpendPoints(s.accountID, s.char.ID, prod.BuyCost, code, target, 2)
	if err != nil {
		s.log.Printf("Rapture World gift point deduction failed after storing the gift: %v", err)
	}
	s.points = left
	s.log.Printf("Rapture World gift sent product=%s to=%q account=%d", code, target, targetUID)
	reply(db.GiftOK, prod.BuyCost)
	// An online recipient sees the gift without relogging.
	if rp := FindPlayerByName(target); rp != nil {
		if list, err := buildGiftBoxList(targetUID); err == nil {
			rp.Send(list, "0x2606 gift box push")
		}
	}
}

// handleGiftClaim (0x2607): gift-box slot.
func (s *Session) handleGiftClaim(p []byte) {
	if len(p) < 3 || s.char == nil || p[2] >= db.GiftBoxSlots {
		return
	}
	slot := p[2]
	gifts, err := db.GiftBox(s.accountID)
	if err != nil {
		return
	}
	var entry *db.GiftEntry
	for i := range gifts {
		if gifts[i].Slot == slot {
			entry = &gifts[i]
		}
	}
	if entry == nil {
		return
	}
	def := db.MustDefs().Item(entry.Type, entry.TypeID)
	free := freeInventorySlots(s.items)
	if def == nil || len(free) == 0 {
		return
	}
	it := db.Item{Bag: free[0][0], Slot: free[0][1], Type: entry.Type, TypeID: entry.TypeID, Quality: def.Quality,
		Count: min(entry.Count, def.MaxStack()), MakeType: "B"}
	ok, err := db.ClaimGift(s.accountID, s.char.ID, slot, &it)
	if err != nil || !ok {
		s.log.Printf("Rapture World gift claim failed slot=%d: %v", slot, err)
		return
	}
	s.items = append(s.items, it)
	s.log.Printf("Rapture World gift claimed slot=%d item=%d/%d", slot, it.Type, it.TypeID)
	s.send(tradePacket(packet.PointStoredItemGet, slot, it.Bag, it.Slot), "0x2607 gift claim result")
}
