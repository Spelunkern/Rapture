package world

import (
	"math"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// Item durability follows ps_game.exe: CGameData::LoadConstItem derives
// wQuality[1] = Quality * 0.5 and wQuality[2] = Quality * 0.1 and each
// Effect at level 1 as Effect * 0.5 (level 2 gives 0); CItem::
// GetQualityLevel is 2 at or below the tenth, 1 at or below the half, 0
// above. CUser::QualityByAttack wears the weapon 1 point per hit,
// QualityByDefense one armour piece per hit taken (shield, helmet, upper,
// gloves, lower, shoes in turn), unless ability 36 is active or the user
// has a shape. A level change re-adds the item's effects (ItemEquipment
// AttackRemQ/AddQ) and SetAttack. CUser::ItemRepair restores the item
// quality plus its gems' quality.

const abilityNoWear = 36

// armourWearOrder is QualityByDefense's slot rotation.
var armourWearOrder = [6]uint8{6, 0, 1, 3, 2, 4}

func qualityLevel(quality, max uint16) uint8 {
	switch {
	case quality <= uint16(float32(max)*0.1):
		return 2
	case quality <= uint16(float32(max)*0.5):
		return 1
	}
	return 0
}

// effectAtLevel is the wEffect[level] table.
func effectAtLevel(v int16, level uint8) uint32 {
	switch level {
	case 0:
		return pos16(v)
	case 1:
		return uint32(uint16(float32(pos16(v)) * 0.5))
	}
	return 0
}

// itemQualityLevel is the item's level from its stored quality.
func itemQualityLevel(it *db.Item, d *db.ItemDef) uint8 {
	return qualityLevel(it.Quality, d.Quality)
}

// maxItemQuality is ItemRepair's target: the item's Quality plus every gem's.
func maxItemQuality(it *db.Item, d *db.ItemDef) uint32 {
	q := uint32(d.Quality)
	for _, g := range it.Gems {
		if g == 0 {
			continue
		}
		if gd := db.MustDefs().Item(gemItemType, g); gd != nil {
			q += uint32(gd.Quality)
		}
	}
	return q
}

// wearItem is CUser::QualityDec: false when the slot is empty or its
// quality already 0.
func (s *Session) wearItem(slot uint8) bool {
	i := findItem(s.items, 0, slot)
	if i < 0 || s.items[i].Quality == 0 {
		return false
	}
	it := &s.items[i]
	d := db.MustDefs().Item(it.Type, it.TypeID)
	before := uint8(0)
	if d != nil {
		before = itemQualityLevel(it, d)
	}
	it.Quality--
	if s.wornItems == nil {
		s.wornItems = map[int64]bool{}
	}
	s.wornItems[it.UID] = true
	if d != nil && itemQualityLevel(it, d) != before {
		s.sendStatPackets()
	}
	return true
}

func (s *Session) wearExempt() bool {
	return s.mounted || s.hasBenefit(abilityNoWear)
}

// wearWeapon is QualityByAttack, after a hit.
func (s *Session) wearWeapon() {
	if !s.wearExempt() {
		s.wearItem(weaponSlot)
	}
}

// wearArmour is QualityByDefense, on every hit taken.
func (s *Session) wearArmour() {
	if s.wearExempt() {
		return
	}
	// From the current index to the end of the order; the index wraps to
	// 0 after the last slot but the scan does not continue past it.
	for i := s.armourWearNext; i < len(armourWearOrder); i++ {
		worn := s.wearItem(armourWearOrder[i])
		s.armourWearNext++
		if s.armourWearNext >= len(armourWearOrder) {
			s.armourWearNext = 0
		}
		if worn {
			return
		}
	}
}

// saveWornItems persists the qualities changed since the last save.
func (s *Session) saveWornItems() {
	if s.char == nil || len(s.wornItems) == 0 {
		return
	}
	for i := range s.items {
		if s.wornItems[s.items[i].UID] {
			if err := db.SaveCharacterItem(s.char.ID, &s.items[i]); err != nil {
				s.log.Printf("Rapture World failed to save item quality: %v", err)
			}
		}
	}
	s.wornItems = nil
}

// repairCost is ItemRepair's price for one item: round((max - quality) /
// 400 * Buy / 30), at least 1 (guild-house rates are not modelled).
func repairCost(it *db.Item, d *db.ItemDef) (uint32, uint16, bool) {
	full := maxItemQuality(it, d)
	if full <= uint32(it.Quality) {
		return 0, 0, false
	}
	cost := math.Round(float64(float32(full-uint32(it.Quality))/400) * float64(float32(d.Buy)/30))
	return uint32(max(cost, 1)), uint16(min(full, 65535)), true
}

// handleItemRepair (0x0554): bag, slot; 0xFF/0xFF repairs every equipped
// item for their summed price. Not enough money does nothing.
func (s *Session) handleItemRepair(p []byte) {
	if len(p) < 4 || s.char == nil || s.dead {
		return
	}
	bag, slot := p[2], p[3]
	ch, defs := s.char, db.MustDefs()
	reply := func() {
		w := packet.New(packet.ItemRepair)
		w.U8(bag)
		w.U8(slot)
		w.U32(ch.Money)
		s.send(w.MustFinish(), "0x0554 item repair")
	}
	if bag == 0xff {
		if slot != 0xff {
			return
		}
		total := 0.0
		for i := range s.items {
			it := &s.items[i]
			if it.Bag != 0 || it.Slot >= 13 {
				continue
			}
			if d := defs.Item(it.Type, it.TypeID); d != nil {
				full := maxItemQuality(it, d)
				total = math.Round(float64(float32(int32(full)-int32(it.Quality))/400)*float64(float32(d.Buy)/30) + total)
			}
		}
		cost := uint32(max(total, 1))
		if ch.Money < cost {
			return
		}
		ch.Money -= cost
		changed := false
		for i := range s.items {
			it := &s.items[i]
			if it.Bag != 0 || it.Slot >= 13 {
				continue
			}
			if d := defs.Item(it.Type, it.TypeID); d != nil {
				if full := uint16(min(maxItemQuality(it, d), 65535)); full != it.Quality {
					it.Quality = full
					changed = true
					if err := db.SaveCharacterItem(ch.ID, it); err != nil {
						s.log.Printf("Rapture World failed to save repaired item: %v", err)
					}
				}
			}
		}
		s.persist()
		if changed {
			s.sendStatPackets()
		}
		reply()
		return
	}
	if bag > 5 || (bag == 0 && slot >= 13) || (bag != 0 && slot >= 24) {
		return
	}
	i := findItem(s.items, bag, slot)
	if i < 0 {
		return
	}
	it := &s.items[i]
	d := defs.Item(it.Type, it.TypeID)
	if d == nil {
		return
	}
	cost, full, ok := repairCost(it, d)
	if !ok || ch.Money < cost {
		return
	}
	ch.Money -= cost
	before := itemQualityLevel(it, d)
	it.Quality = full
	if err := db.SaveCharacterItem(ch.ID, it); err != nil {
		s.log.Printf("Rapture World failed to save repaired item: %v", err)
	}
	s.persist()
	if bag == 0 && itemQualityLevel(it, d) != before {
		s.sendStatPackets()
	}
	reply()
}
