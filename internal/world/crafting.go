package world

import (
	"fmt"
	"math"
	"math/rand/v2"
	"strings"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// Enchanting (lapisia), gem linking and recreation runes. All of them edit
// CharItems.Craftname / Gem1-6 through usp_Save_Char_Item_Mod_E.

const (
	lapisiaItemType      = 95
	enchantMaxLevel      = 20
	maxGemSlots          = 6
	composeResultSuccess = 0
	composeResultFailure = 1
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

// lapisiaMoney is GetGemMoneyByGrade (0x0045f6e0) by the lapisia's ReqIg.
func lapisiaMoney(reqIg uint8) uint32 {
	low := [11]uint32{1000, 4095, 11250, 22965, 41280, 137900, 365000, 480000, 627000, 814000, 1040000}
	high := [5]uint32{2500000, 3500000, 5500000, 7500000, 9800000}
	switch {
	case reqIg >= 30 && reqIg <= 40:
		return low[reqIg-30]
	case reqIg >= 96 && reqIg <= 100:
		return high[reqIg-96]
	}
	return 0
}

// lapisiaFits is CUser::EnableLapisianAdd's target check: real type 24
// always; otherwise the lapisia's own fields name what it enchants
// (ReqLevel weapons, Country type 16, the six job flags types 17-22, Grow
// 23, ReqStr 25).
func lapisiaFits(lap *db.ItemDef, rt uint8) bool {
	switch {
	case rt == 24:
		return true
	case rt >= 1 && rt <= 15:
		return lap.ReqLevel != 0
	case rt == 16:
		return lap.Country != 0
	case rt >= 17 && rt <= 22:
		return lap.Classes[rt-17]
	case rt == 23:
		return lap.Grow != 0
	case rt == 25:
		return lap.ReqStr != 0
	}
	return false
}

// handleEnchantAdd (0x0805) is CUser::ItemLapisianAdd (0x0045fb20):
// lapisia bag/slot, material bag/slot, material count, item bag/slot.
// The item (count 1, Server set) needs its ReqLuc as material; below step
// 20 the lapisia's GetGemMoneyByGrade is paid and SRandom(1,000,000) plus
// the material count must fall under ItemEnchant.ini's rate. Either way
// the lapisia and the material count are used up; a failed lapisia with
// ReqVg set breaks the item (quality 0).
func (s *Session) handleEnchantAdd(p []byte) {
	if len(p) < 9 || s.char == nil {
		return
	}
	ch := s.char
	lBag, lSlot, mBag, mSlot, count, iBag, iSlot := p[2], p[3], p[4], p[5], min(p[6], 100), p[7], p[8]
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
	defs := db.MustDefs()
	lapDef, def := defs.Item(lap.Type, lap.TypeID), defs.Item(item.Type, item.TypeID)
	switch {
	case lapDef == nil || def == nil || lap.Type != lapisiaItemType || item.Count != 1:
		fail("not a lapisia or not a single item")
		return
	case count == 0 || mat.Count < count:
		fail("material count")
		return
	case def.Server == 0 || def.ReqLuc == 0 || mat.ItemID() != def.ReqLuc:
		fail(fmt.Sprintf("material %d does not match required %d", mat.ItemID(), def.ReqLuc))
		return
	case !lapisiaFits(lapDef, realType(item.Type)):
		fail("lapisia does not fit the item")
		return
	}
	weapon := realType(item.Type) >= 1 && realType(item.Type) <= 15
	craft, _ := ParseCraftName(item.CraftName)
	step := craft.EnchantLevel
	if step > 50 {
		step -= 50
	}
	if step >= enchantMaxLevel {
		fail("already at max level")
		return
	}
	cost := lapisiaMoney(lapDef.ReqIg)
	if ch.Money < cost {
		fail("not enough gold")
		return
	}
	ch.Money -= cost
	lapLeft, matLeft := lap.Count-1, mat.Count-count
	uidI := item.UID
	s.adjustCount(lap.UID, lapLeft)
	s.adjustCount(mat.UID, matLeft)
	rate := enchantConfig.DefenseRate[step]
	if weapon {
		rate = enchantConfig.WeaponRate[step]
	}
	if rand.Uint32N(1_000_000)+uint32(count) >= rate {
		name := item.CraftName
		if lapDef.ReqVg != 0 {
			if i := findItemByUID(s.items, uidI); i >= 0 {
				s.items[i].Quality = 0
				db.SaveCharacterItem(ch.ID, &s.items[i])
			}
		}
		s.persist()
		s.log.Printf("Rapture World enchant failed item=%d/%d step=%d broke=%t", item.Type, item.TypeID, step, lapDef.ReqVg != 0)
		s.send(buildEnchantResult(0, lBag, lSlot, lapLeft, mBag, mSlot, matLeft, iBag, iSlot, ch.Money, name), "0x0805 enchant add result")
		if lapDef.ReqVg != 0 && iBag == 0 {
			s.equipmentChanged([]uint8{iSlot})
		}
		return
	}
	pairs := craftPairs(craft)
	next := uint32(step) + 1
	if !weapon {
		next += 50
	}
	pairs[9] = next
	name := craftString(pairs)
	if i := findItemByUID(s.items, uidI); i >= 0 {
		s.items[i].CraftName = name
		db.SaveCharacterItem(ch.ID, &s.items[i])
	}
	s.persist()
	s.log.Printf("Rapture World enchant success item=%d/%d new_step=%d craftname=%s money=%d", item.Type, item.TypeID, step+1, name, ch.Money)
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

// gemFits is CItem::EnableGem (0x004ba050): the item's Slot is a
// capacity each linked gem uses its own Slot of; the same gem twice is
// refused.
func gemFits(item *db.Item, target, gemDef *db.ItemDef) bool {
	used := 0
	for _, g := range item.Gems {
		if g == 0 {
			continue
		}
		if g == gemDef.TypeID {
			return false
		}
		if d := db.MustDefs().Item(gemItemType, g); d != nil {
			used += int(d.Slot)
		}
	}
	return int(gemDef.Slot) <= int(target.Slot)-used
}

// findSpecialItem finds a usable item of real type 26 with the given
// Special in the bags (CUser::FindItemGemAddItemProtect looks for '9',
// the Lucky Charm).
func (s *Session) findSpecialItem(special uint8) int {
	for i := range s.items {
		it := &s.items[i]
		if it.Bag == 0 || it.Count == 0 || realType(it.Type) != 26 {
			continue
		}
		if d := db.MustDefs().Item(it.Type, it.TypeID); d != nil && d.Special == special {
			return i
		}
	}
	return -1
}

const (
	specialGemSuccessUp = 36 // '$' Linking Hammer: ReqVg percent on the chance
	specialGemProtect   = 57 // '9' Lucky Charm: keeps the item on a failure
)

// handleGemAdd (0x0801) is CUser::ItemGemAdd (0x00461310): gem bag/slot,
// item bag/slot, optional hammer bag/slot. The item (count 1) must take
// the gem (the lapisia-style target fields and EnableGem's capacity); the
// gem's GetGemMoneyByGrade is paid; the chance is level x 0.05 + 289 -
// 8 x ReqIg (at least 1), times the hammer's ReqVg %, capped at 50.
// The gem is always used up; a failed gem with ReqVg set destroys the
// item unless a Lucky Charm is carried (and used instead).
func (s *Session) handleGemAdd(p []byte) {
	if len(p) < 6 || s.char == nil {
		return
	}
	ch := s.char
	gBag, gSlot, iBag, iSlot := p[2], p[3], p[4], p[5]
	var hBag, hSlot uint8
	if len(p) >= 8 {
		hBag, hSlot = p[6], p[7]
	}
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
	defs := db.MustDefs()
	target, gemDef := defs.Item(item.Type, item.TypeID), defs.Item(gemItemType, gem.TypeID)
	switch {
	case gem.Type != gemItemType || gem.Count == 0 || gemDef == nil || target == nil || item.Count != 1:
		fail("not a gem or not a single item")
		return
	case !lapisiaFits(gemDef, realType(item.Type)) || !gemFits(&item, target, gemDef):
		fail("gem does not fit the item")
		return
	}
	pos := -1
	for i, g := range item.Gems {
		if g == 0 {
			pos = i
			break
		}
	}
	if pos < 0 {
		fail("no free socket")
		return
	}
	cost := lapisiaMoney(gemDef.ReqIg)
	if ch.Money < cost {
		fail("not enough gold")
		return
	}
	ch.Money -= cost
	chance := int(math.RoundToEven(float64(ch.Level)*float64(float32(0.05)) + float64(289-8*int(gemDef.ReqIg))))
	chance = max(chance, 1)
	if hBag != 0 && isValidItemPosition(hBag, hSlot) {
		if hi := findItem(s.items, hBag, hSlot); hi >= 0 {
			if hd := defs.Item(s.items[hi].Type, s.items[hi].TypeID); hd != nil && realType(hd.Type) == 26 && hd.Special == specialGemSuccessUp {
				chance = int(math.RoundToEven(float64(hd.ReqVg) / 100 * float64(chance)))
				s.adjustCount(s.items[hi].UID, s.items[hi].Count-1)
			}
		}
	}
	chance = min(max(chance, 1), 50)
	before := s.maxPools()
	uidI := item.UID
	s.adjustCount(gem.UID, gem.Count-1)
	if rand.IntN(100) >= chance {
		broke := false
		if gemDef.ReqVg != 0 {
			if ci := s.findSpecialItem(specialGemProtect); ci >= 0 {
				s.adjustCount(s.items[ci].UID, s.items[ci].Count-1)
			} else if i := findItemByUID(s.items, uidI); i >= 0 {
				broke = true
				s.adjustCount(uidI, 0)
				s.send(buildRemoveItem(&item, true), "0x0206 item destroyed by a failed gem")
			}
		}
		s.persist()
		s.log.Printf("Rapture World gem add failed gem=%d chance=%d item_destroyed=%t", gem.TypeID, chance, broke)
		s.send(buildGemAddResult(0, gBag, gSlot, gem.Count-1, iBag, iSlot, 0, 0, ch.Money), "0x0801 gem add result")
		if broke && iBag == 0 {
			s.equipmentChanged([]uint8{iSlot})
			s.sendChangedPools(before, "gem add")
		}
		if s.player != nil {
			s.player.UpdateItems(s.items)
		}
		return
	}
	if i := findItemByUID(s.items, uidI); i >= 0 {
		s.items[i].Gems[pos] = gem.TypeID
		db.SaveCharacterItem(ch.ID, &s.items[i])
	}
	s.persist()
	s.log.Printf("Rapture World gem add gem=%d item=%d/%d position=%d chance=%d", gem.TypeID, item.Type, item.TypeID, pos, chance)
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
	// CUser::ItemGemRemoveAll (0x00461fc0): the item (count 1, not real
	// type 24) pays every gem's GetGemMoneyByGrade; the chance is the first
	// gem's (level x 0.05 + 289 - 8 x ReqIg, 1-50). A failure loses all the
	// gems, and the item too when one of them had ReqVg set.
	item := s.items[ii]
	defs := db.MustDefs()
	var cost uint32
	count, risky, first := 0, false, (*db.ItemDef)(nil)
	for _, g := range item.Gems {
		if d := defs.Item(gemItemType, g); g != 0 && d != nil {
			cost += lapisiaMoney(d.ReqIg)
			count++
			risky = risky || d.ReqVg != 0
			if first == nil {
				first = d
			}
		}
	}
	result := func(ok bool) {
		w := packet.New(packet.GemRemove)
		w.Bool(ok)
		w.U8(iBag)
		w.U8(iSlot)
		w.Bytes(removed[:])
		w.Bytes(ids[:])
		w.Bytes(bags[:])
		w.Bytes(slots[:])
		w.U32(ch.Money)
		s.send(w.MustFinish(), "0x0802 gem remove result")
	}
	if count == 0 || item.Count != 1 || realType(item.Type) == 24 || ch.Money < cost || len(freeInventorySlots(s.items)) < count {
		s.log.Printf("Rapture World rejected gem remove: gems=%d cost=%d money=%d", count, cost, ch.Money)
		result(false)
		return
	}
	ch.Money -= cost
	chance := min(max(int(math.RoundToEven(float64(ch.Level)*float64(float32(0.05))+float64(289-8*int(first.ReqIg)))), 1), 50)
	if rand.IntN(100) >= chance {
		i := findItemByUID(s.items, uid)
		s.items[i].Gems = [maxGemSlots]uint8{}
		if risky {
			s.adjustCount(uid, 0)
			s.send(buildRemoveItem(&item, true), "0x0206 item destroyed by a failed gem removal")
		} else {
			db.SaveCharacterItem(ch.ID, &s.items[i])
		}
		s.persist()
		s.log.Printf("Rapture World gem remove failed chance=%d item_destroyed=%t", chance, risky)
		result(false)
		if iBag == 0 {
			if risky {
				s.equipmentChanged([]uint8{iSlot})
			}
			s.sendChangedPools(before, "gem remove")
		}
		if s.player != nil {
			s.player.UpdateItems(s.items)
		}
		return
	}
	s.persist()
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
	result(extracted)
	if extracted && iBag == 0 {
		s.sendChangedPools(before, "gem remove")
	}
	if s.player != nil {
		s.player.UpdateItems(s.items)
	}
}

// --- recreation rune (0x0806) ---

var jewelryItemTypes = []uint8{22, 23, 40}

// rollsStatsOnly is CItem::ReGenerationCraftExpansion's split: weapons
// (real types 1-15) and real types 22, 23 and 25 roll the six stats only.
func rollsStatsOnly(t uint8) bool {
	rt := realType(t)
	return (rt >= 1 && rt <= 15) || rt == 22 || rt == 23 || rt == 25
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
