package world

import (
	"fmt"
	"math/rand/v2"
	"strings"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// Enchanting (lapisia), gem linking and recreation runes. All of them edit
// CharItems.Craftname / Gem1-6 through usp_Save_Char_Item_Mod_E.

const (
	lapisiaItemType        = 95
	enchantMaxLevel        = 20
	weaponEnchantCostMoney = 2_500_000
	armorEnchantCostMoney  = 1_040_000
	maxGemSlots            = 6
	composeResultSuccess   = 0
	composeResultFailure   = 1
)

func craftString(pairs [10]uint32) string {
	var b strings.Builder
	for _, v := range pairs {
		fmt.Fprintf(&b, "%02d", min(v, 99))
	}
	return b.String()
}

func craftPairs(c CraftStats) [10]uint32 {
	return [10]uint32{c.Str, c.Dex, c.Rec, c.Int, c.Wis, c.Luc, c.MaxHP / 100, c.MaxMP / 100, c.MaxSP / 100, uint32(c.EnchantLevel)}
}

// --- enchant (0x0805) ---

func buildEnchantResult(result, lBag, lSlot, lCount, mBag, mSlot, mCount, iBag, iSlot uint8, money uint32, craft string) []byte {
	w := packet.New(packet.EnchantAdd)
	w.Bytes([]byte{result, lBag, lSlot, lCount, mBag, mSlot, mCount, iBag, iSlot})
	w.U32(money)
	w.FixedString(craft, 21)
	return w.MustFinish()
}

// handleEnchantAdd: lapisia bag/slot, material bag/slot, _, item bag/slot.
// The material must match the item's Items.ReqLuc (Type*1000+TypeID).
func (s *Session) handleEnchantAdd(p []byte) {
	if len(p) < 9 || s.char == nil {
		return
	}
	ch := s.char
	lBag, lSlot, mBag, mSlot, iBag, iSlot := p[2], p[3], p[4], p[5], p[7], p[8]
	if !isValidItemPosition(lBag, lSlot) || !isValidItemPosition(mBag, mSlot) || !isValidItemPosition(iBag, iSlot) {
		return
	}
	li, mi, ii := findItem(s.items, lBag, lSlot), findItem(s.items, mBag, mSlot), findItem(s.items, iBag, iSlot)
	if li < 0 || mi < 0 || ii < 0 {
		return
	}
	lap, mat, item := s.items[li], s.items[mi], s.items[ii]
	fail := func(why string) {
		s.log.Printf("Rapture World rejected enchant: %s", why)
		s.send(buildEnchantResult(0, lBag, lSlot, lap.Count, mBag, mSlot, mat.Count, iBag, iSlot, ch.Money, item.CraftName), "0x0805 enchant add result")
	}
	weapon, armor := IsWeaponItemType(item.Type), IsArmorItemType(item.Type)
	switch {
	case lap.Count == 0 || mat.Count == 0:
		fail("empty lapisia/material")
		return
	case !weapon && !armor:
		fail("not a weapon or armor")
		return
	case lap.Type != lapisiaItemType || (weapon && (lap.TypeID < 1 || lap.TypeID > 5)) || (armor && (lap.TypeID < 6 || lap.TypeID > 10)):
		fail("wrong lapisia")
		return
	}
	def := db.MustDefs().Item(item.Type, item.TypeID)
	if def == nil || def.ReqLuc == 0 {
		fail("item not enchantable")
		return
	}
	if mat.ItemID() != def.ReqLuc {
		fail(fmt.Sprintf("material %d does not match required %d", mat.ItemID(), def.ReqLuc))
		return
	}
	craft, _ := ParseCraftName(item.CraftName)
	if craft.EnchantLevel >= enchantMaxLevel {
		fail("already at max level")
		return
	}
	cost := uint32(armorEnchantCostMoney)
	if weapon {
		cost = weaponEnchantCostMoney
	}
	if ch.Money < cost {
		fail("not enough gold")
		return
	}
	ch.Money -= cost
	lapLeft, matLeft := lap.Count-1, mat.Count-1
	uidL, uidM, uidI := lap.UID, mat.UID, item.UID
	s.adjustCount(uidL, lapLeft)
	s.adjustCount(uidM, matLeft)
	rate := enchantConfig.DefenseRate[craft.EnchantLevel]
	if weapon {
		rate = enchantConfig.WeaponRate[craft.EnchantLevel]
	}
	if rand.Uint32N(1_000_000) >= rate { // ItemEnchant.ini, per million
		s.persist()
		s.send(buildEnchantResult(0, lBag, lSlot, lapLeft, mBag, mSlot, matLeft, iBag, iSlot, ch.Money, item.CraftName), "0x0805 enchant add result")
		return
	}
	pairs := craftPairs(craft)
	pairs[9]++
	name := craftString(pairs)
	if i := findItemByUID(s.items, uidI); i >= 0 {
		s.items[i].CraftName = name
		db.SaveCharacterItem(ch.ID, &s.items[i])
	}
	s.persist()
	s.log.Printf("Rapture World enchant success item=%d/%d new_level=%d craftname=%s money=%d", item.Type, item.TypeID, pairs[9], name, ch.Money)
	s.send(buildEnchantResult(1, lBag, lSlot, lapLeft, mBag, mSlot, matLeft, iBag, iSlot, ch.Money, name), "0x0805 enchant add result")
	if iBag == 0 {
		s.equipmentChanged([]uint8{iSlot})
	}
}

// adjustCount sets an item's count by UID (deleting at zero) and persists.
func (s *Session) adjustCount(uid int64, count uint8) {
	i := findItemByUID(s.items, uid)
	if i < 0 {
		return
	}
	s.items[i].Count = count
	if err := db.SaveCharacterItem(s.char.ID, &s.items[i]); err != nil {
		s.log.Printf("Rapture World failed to update item count: %v", err)
	}
	if count == 0 {
		s.items = append(s.items[:i], s.items[i+1:]...)
	}
}

// --- gems (0x0801 / 0x0802) ---

func buildGemAddResult(result, gBag, gSlot, gCount, iBag, iSlot, pos, gemID uint8, money uint32) []byte {
	w := packet.New(packet.GemAdd)
	w.Bytes([]byte{result, gBag, gSlot, gCount, iBag, iSlot, pos, gemID})
	w.U32(0) // four unused bytes (client reader 0x0801: 12 bytes + money)
	w.U32(money)
	return w.MustFinish()
}

// maxPools returns the effective max HP/SP/MP (0x050B order 0, 1, 2).
func (s *Session) maxPools() [3]uint32 {
	st := s.stats()
	return [3]uint32{st.MaxHP, st.MaxSP, st.MaxMP}
}

// sendChangedPools sends 0x050B only for pools that changed; a full stat
// resend crashes the client after item mutations.
func (s *Session) sendChangedPools(before [3]uint32, why string) {
	after := s.maxPools()
	for t := range 3 {
		if before[t] != after[t] {
			s.send(buildMaxEnergy(s.char, uint8(t), after[t]), "0x050B max energy ("+why+")")
		}
	}
}

func (s *Session) handleGemAdd(p []byte) {
	if len(p) < 8 || s.char == nil {
		return
	}
	ch := s.char
	gBag, gSlot, iBag, iSlot := p[2], p[3], p[4], p[5]
	if !isValidItemPosition(gBag, gSlot) || !isValidItemPosition(iBag, iSlot) {
		return
	}
	gi, ii := findItem(s.items, gBag, gSlot), findItem(s.items, iBag, iSlot)
	if gi < 0 || ii < 0 {
		return
	}
	gem, item := s.items[gi], s.items[ii]
	fail := func(why string) {
		s.log.Printf("Rapture World rejected gem add: %s", why)
		s.send(buildGemAddResult(0, gBag, gSlot, gem.Count, iBag, iSlot, 0, 0, ch.Money), "0x0801 gem add result")
	}
	if gem.Type != gemItemType || gem.Count == 0 {
		fail("not a gem")
		return
	}
	defs := db.MustDefs()
	target, gemDef := defs.Item(item.Type, item.TypeID), defs.Item(gemItemType, gem.TypeID)
	if target == nil {
		fail("unknown target")
		return
	}
	slots := int(min(target.Slot, maxGemSlots))
	pos := -1
	for i := 0; i < slots; i++ {
		if item.Gems[i] == gem.TypeID {
			fail("gem already linked")
			return
		}
		if item.Gems[i] == 0 && pos < 0 {
			pos = i
		}
	}
	if pos < 0 {
		fail("no free socket")
		return
	}
	if gemDef != nil && int(target.ReqLevel) < int(max(gemDef.ReqInt, 0)) {
		fail("item level below gem requirement")
		return
	}
	before := s.maxPools()
	s.items[ii].Gems[pos] = gem.TypeID
	db.SaveCharacterItem(ch.ID, &s.items[ii])
	s.adjustCount(gem.UID, gem.Count-1)
	s.log.Printf("Rapture World gem add gem=%d item=%d/%d position=%d", gem.TypeID, item.Type, item.TypeID, pos)
	s.send(buildGemAddResult(1, gBag, gSlot, gem.Count-1, iBag, iSlot, uint8(pos), gem.TypeID, ch.Money), "0x0801 gem add result")
	if iBag == 0 {
		s.sendChangedPools(before, "gem add")
	}
	if s.player != nil {
		s.player.UpdateItems(s.items)
	}
}

// handleGemRemove: item bag, slot (the client sends 4- and 8-byte forms).
func (s *Session) handleGemRemove(p []byte) {
	if len(p) < 4 || s.char == nil {
		return
	}
	ch := s.char
	iBag, iSlot := p[2], p[3]
	if !isValidItemPosition(iBag, iSlot) {
		return
	}
	ii := findItem(s.items, iBag, iSlot)
	if ii < 0 {
		return
	}
	uid := s.items[ii].UID
	before := s.maxPools()
	var removed, ids, bags, slots [maxGemSlots]uint8
	var adds [][]byte
	extracted := false
	for pos := range maxGemSlots {
		i := findItemByUID(s.items, uid)
		gemID := s.items[i].Gems[pos]
		if gemID == 0 || db.MustDefs().Item(gemItemType, gemID) == nil {
			continue
		}
		free := freeInventorySlots(s.items)
		if len(free) == 0 {
			continue
		}
		g := db.Item{Bag: free[0][0], Slot: free[0][1], Type: gemItemType, TypeID: gemID, Count: 1}
		if err := db.AddCharacterItem(ch.ID, &g); err != nil {
			s.log.Printf("Rapture World gem remove failed: %v", err)
			continue
		}
		s.items[i].Gems[pos] = 0
		db.SaveCharacterItem(ch.ID, &s.items[i])
		s.items = append(s.items, g)
		adds = append(adds, buildAddItem(&g))
		removed[pos], ids[pos], bags[pos], slots[pos] = 1, gemID, g.Bag, g.Slot
		extracted = true
	}
	for _, a := range adds {
		s.send(a, "0x0205 gem remove add item")
	}
	w := packet.New(packet.GemRemove)
	w.Bool(extracted)
	w.U8(iBag)
	w.U8(iSlot)
	w.Bytes(removed[:])
	w.Bytes(ids[:])
	w.Bytes(bags[:])
	w.Bytes(slots[:])
	w.U32(ch.Money)
	s.send(w.MustFinish(), "0x0802 gem remove result")
	if extracted && iBag == 0 {
		s.sendChangedPools(before, "gem remove")
	}
	if s.player != nil {
		s.player.UpdateItems(s.items)
	}
}

// --- recreation rune (0x0806) ---

var jewelryItemTypes = []uint8{22, 23, 40}

func rollsStatsOnly(t uint8) bool {
	if IsWeaponItemType(t) {
		return true
	}
	for _, j := range jewelryItemTypes {
		if j == t {
			return true
		}
	}
	return false
}

// rollCraftName picks `count` distinct stats (6 for weapons/jewelry, 9
// otherwise) at 1..maxValue each, keeping the enchant level.
func rollCraftName(count, maxValue uint8, statsOnly bool, previous string) string {
	pool := 9
	if statsOnly {
		pool = 6
	}
	var pairs [10]uint32
	for _, i := range rand.Perm(pool)[:min(int(count), pool)] {
		pairs[i] = uint32(rand.IntN(int(max(maxValue, 1)))) + 1
	}
	if c, ok := ParseCraftName(previous); ok {
		pairs[9] = uint32(c.EnchantLevel)
	}
	return craftString(pairs)
}

func buildComposeResult(result, bag, slot uint8, craft string) []byte {
	w := packet.New(packet.ItemCompose)
	w.Bytes([]byte{result, bag, slot})
	w.FixedString(craft, 21)
	return w.MustFinish()
}

// handleItemCompose: rune bag/slot, item bag/slot. Items.ReqWis is the max
// roll and Items.Server the number of rolled stats.
func (s *Session) handleItemCompose(p []byte) {
	if len(p) < 6 || s.char == nil {
		return
	}
	ch := s.char
	rBag, rSlot, iBag, iSlot := p[2], p[3], p[4], p[5]
	if !isValidItemPosition(rBag, rSlot) || !isValidItemPosition(iBag, iSlot) {
		return
	}
	ri, ii := findItem(s.items, rBag, rSlot), findItem(s.items, iBag, iSlot)
	if ri < 0 || ii < 0 {
		s.send(buildComposeResult(composeResultFailure, iBag, iSlot, ""), "0x0806 item compose result")
		return
	}
	rune, item := s.items[ri], s.items[ii]
	fail := func(why string) {
		s.log.Printf("Rapture World rejected item compose: %s", why)
		s.send(buildComposeResult(composeResultFailure, iBag, iSlot, item.CraftName), "0x0806 item compose result")
	}
	defs := db.MustDefs()
	runeDef, itemDef := defs.Item(rune.Type, rune.TypeID), defs.Item(item.Type, item.TypeID)
	switch {
	case rune.Count == 0 || runeDef == nil || runeDef.Special != specialRecreationRune:
		fail("not a recreation rune")
		return
	case itemDef == nil || itemDef.ReqWis <= 0 || itemDef.Server == 0:
		fail("item not recreatable")
		return
	}
	before := s.maxPools()
	name := rollCraftName(min(itemDef.Server, 9), uint8(min(itemDef.ReqWis, 99)), rollsStatsOnly(item.Type), item.CraftName)
	s.items[ii].CraftName = name
	db.SaveCharacterItem(ch.ID, &s.items[ii])
	s.adjustCount(rune.UID, rune.Count-1)
	consumed := rune
	consumed.Count--
	s.log.Printf("Rapture World item compose item=%d/%d craftname=%s", item.Type, item.TypeID, name)
	// Captured order: 0x050B (changed pools), 0x0806, 0x050A.
	if iBag == 0 {
		s.sendChangedPools(before, "compose")
	}
	s.send(buildComposeResult(composeResultSuccess, iBag, iSlot, name), "0x0806 item compose result")
	s.send(buildUseItemResult(ch.ID, &consumed), "0x050A rune consume (compose)")
	if s.player != nil {
		s.player.UpdateItems(s.items)
	}
}
