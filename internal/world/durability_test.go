package world

import (
	"testing"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
)

// TestDurability follows QualityDec / GetQualityLevel / ItemRepair: a hit
// wears the weapon by 1, the attack drops to half at half quality and to
// nothing at a tenth, armour wears in the shield-helmet-upper-gloves-
// lower-shoes rotation, and repair restores the quality plus the gems'
// for round(missing / 400 * Buy / 30) gold.
func TestDurability(t *testing.T) {
	weapon := &db.ItemDef{Type: 1, TypeID: 1, Quality: 100, Effect1: 41, Effect2: 10, Buy: 3000}
	shield := &db.ItemDef{Type: 19, TypeID: 1, Quality: 100, Effect3: 30, Buy: 300}
	helmet := &db.ItemDef{Type: 16, TypeID: 1, Quality: 100, Effect3: 20, Buy: 300}
	gem := &db.ItemDef{Type: gemItemType, TypeID: 7, Quality: 50}
	db.SetDefsForTest(&db.Defs{Exp: map[db.ExpKey]uint32{}, Items: map[db.ItemKey]*db.ItemDef{
		{Type: 1, TypeID: 1}: weapon, {Type: 19, TypeID: 1}: shield, {Type: 16, TypeID: 1}: helmet, {Type: gemItemType, TypeID: 7}: gem}})
	logger, _ := logx.New("rapture_world_test")
	s := &Session{char: &db.Character{Money: 1000}, log: logger.Tagged("unit"), logger: logger}
	s.items = []db.Item{
		{UID: 1, Bag: 0, Slot: weaponSlot, Type: 1, TypeID: 1, Quality: 100, Gems: [6]uint8{7}},
		{UID: 2, Bag: 0, Slot: 6, Type: 19, TypeID: 1, Quality: 100},
		{UID: 3, Bag: 0, Slot: 0, Type: 16, TypeID: 1, Quality: 100},
	}
	attack := func() uint32 { return GetEquipmentStats(s.items).ItemAttack }
	if a := attack(); a != 41 {
		t.Fatalf("full attack %d", a)
	}
	s.wearWeapon()
	if s.items[0].Quality != 99 || !s.wornItems[1] {
		t.Fatalf("weapon quality %d after a hit", s.items[0].Quality)
	}
	s.items[0].Quality = 50
	if a := attack(); a != 20 {
		t.Fatalf("attack at half quality %d, want 20", a)
	}
	s.items[0].Quality = 10
	if a := attack(); a != 0 {
		t.Fatalf("attack at a tenth %d, want 0", a)
	}
	s.wearArmour()
	s.wearArmour()
	s.wearArmour() // upper (slot 1) empty: skips to gloves, then lower, shoes: nothing to wear
	if s.items[1].Quality != 99 || s.items[2].Quality != 99 {
		t.Fatalf("armour rotation shield %d helmet %d", s.items[1].Quality, s.items[2].Quality)
	}
	// Repair: max 100 + 50 (gem); missing 140 -> round(140/400 * 3000/30) = 35.
	s.handleItemRepair([]byte{0x54, 0x05, 0, weaponSlot})
	if s.items[0].Quality != 150 || s.char.Money != 965 {
		t.Fatalf("repair quality %d money %d, want 150 / 965", s.items[0].Quality, s.char.Money)
	}
	s.char.Money = 0
	s.handleItemRepair([]byte{0x54, 0x05, 0, 6})
	if s.items[1].Quality != 99 {
		t.Fatal("repaired without money")
	}
}
