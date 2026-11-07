package world

import (
	"fmt"
	"math"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

const inventoryChunk = 50

func buildInventoryPackets(items []db.Item) [][]byte {
	if len(items) == 0 {
		return [][]byte{buildZeroCount(packet.CharacterItems)}
	}
	var out [][]byte
	for start := 0; start < len(items); start += inventoryChunk {
		chunk := items[start:min(start+inventoryChunk, len(items))]
		w := packet.New(packet.CharacterItems)
		w.U8(uint8(len(chunk)))
		for i := range chunk {
			it := &chunk[i]
			w.U8(it.Bag)
			w.U8(it.Slot)
			w.U8(it.Type)
			w.U8(it.TypeID)
			w.U16(it.Quality)
			w.Bytes(it.Gems[:])
			w.U8(it.Count)
			w.FixedString(it.CraftName, 21)
		}
		out = append(out, w.MustFinish())
	}
	return out
}

func isValidItemPosition(bag, slot uint8) bool { return bag <= 5 && slot < 24 }

func findItem(items []db.Item, bag, slot uint8) int {
	for i := range items {
		if items[i].Bag == bag && items[i].Slot == slot {
			return i
		}
	}
	return -1
}

func findItemByUID(items []db.Item, uid int64) int {
	for i := range items {
		if items[i].UID == uid {
			return i
		}
	}
	return -1
}

func writeSlot(w *packet.Writer, bag, slot uint8, it *db.Item) {
	w.U8(bag)
	w.U8(slot)
	if it == nil {
		w.Bytes(make([]byte, 3))
		w.U16(0)
		w.Bytes(make([]byte, 6))
		w.FixedString("", 21)
		return
	}
	w.U8(it.Type)
	w.U8(it.TypeID)
	w.U8(it.Count)
	w.U16(it.Quality)
	w.Bytes(it.Gems[:])
	w.FixedString(it.CraftName, 21)
}

func buildBagToBag(srcBag, srcSlot uint8, src *db.Item, destBag, destSlot uint8, dest *db.Item) []byte {
	w := packet.New(packet.ItemBagToBag)
	writeSlot(w, srcBag, srcSlot, src)
	writeSlot(w, destBag, destSlot, dest)
	return w.MustFinish()
}

func buildAddItem(it *db.Item) []byte {
	w := packet.New(packet.AddItem)
	writeSlot(w, it.Bag, it.Slot, it)
	return w.MustFinish()
}

func buildSetMoney(money uint32) []byte {
	w := packet.New(packet.SetMoney)
	w.U32(money)
	return w.MustFinish()
}

func buildAddItemError(code uint8) []byte {
	w := packet.New(packet.AddItem)
	w.U8(0)
	w.U8(code)
	return w.MustFinish()
}

func buildUseItemResult(characterID uint32, it *db.Item) []byte {
	w := packet.New(packet.UseItem)
	w.U32(characterID)
	if it == nil {
		w.Bytes(make([]byte, 5))
	} else {
		w.Bytes([]byte{it.Bag, it.Slot, it.Type, it.TypeID, it.Count})
	}
	return w.MustFinish()
}

func buildRemoveItem(it *db.Item, full bool) []byte {
	w := packet.New(packet.RemoveItem)
	w.U8(it.Bag)
	w.U8(it.Slot)
	if full {
		w.Bytes(make([]byte, 3))
	} else {
		w.Bytes([]byte{it.Type, it.TypeID, it.Count})
	}
	return w.MustFinish()
}

func freeInventorySlots(items []db.Item) [][2]uint8 {
	var out [][2]uint8
	for bag := uint8(InventoryFirstBag); bag <= InventoryLastBag; bag++ {
		for slot := uint8(0); slot < InventorySlotCount; slot++ {
			if findItem(items, bag, slot) < 0 {
				out = append(out, [2]uint8{bag, slot})
			}
		}
	}
	return out
}

func itemsMatch(a, b *db.Item) bool {
	return a.Type == b.Type && a.TypeID == b.TypeID && a.Quality == b.Quality && a.Gems == b.Gems && a.CraftName == b.CraftName
}

// itemsCanStack: same item, and the destination is not equipped.
func itemsCanStack(src, dest *db.Item) bool { return itemsMatch(src, dest) && dest.Bag != 0 }

func maxStackOf(t, id uint8) uint8 {
	if d := db.MustDefs().Item(t, id); d != nil {
		return d.MaxStack()
	}
	return 1
}

func findStackable(items []db.Item, tpl *db.Item, maxStack uint8) int {
	for i := range items {
		if itemsCanStack(tpl, &items[i]) && items[i].Count < maxStack {
			return i
		}
	}
	return -1
}

// wouldFit simulates giveItem on a copy of the inventory.
func wouldFit(items []db.Item, tpl *db.Item, count, maxStack uint8) bool {
	sim := append([]db.Item(nil), items...)
	for count > 0 {
		if i := findStackable(sim, tpl, maxStack); i >= 0 {
			moved := min(count, maxStack-sim[i].Count)
			sim[i].Count += moved
			count -= moved
			continue
		}
		free := freeInventorySlots(sim)
		if len(free) == 0 {
			return false
		}
		n := min(count, maxStack)
		it := *tpl
		it.Bag, it.Slot, it.Count = free[0][0], free[0][1], n
		sim = append(sim, it)
		count -= n
	}
	return true
}

// giveItem stacks onto existing items, then fills free slots, persisting
// each change. Returns the 0x0205 packets and what did not fit.
func (s *Session) giveItem(tpl *db.Item, count, maxStack uint8) ([][]byte, uint8, error) {
	return giveItemTo(s.char.ID, &s.items, tpl, count, maxStack)
}

func giveItemTo(charID uint32, items *[]db.Item, tpl *db.Item, count, maxStack uint8) ([][]byte, uint8, error) {
	var packets [][]byte
	for count > 0 {
		if i := findStackable(*items, tpl, maxStack); i >= 0 {
			it := &(*items)[i]
			moved := min(count, maxStack-it.Count)
			it.Count += moved
			if err := db.SaveCharacterItem(charID, it); err != nil {
				return packets, count, err
			}
			packets = append(packets, buildAddItem(it))
			count -= moved
			continue
		}
		free := freeInventorySlots(*items)
		if len(free) == 0 {
			break
		}
		n := min(count, maxStack)
		it := db.Item{Bag: free[0][0], Slot: free[0][1], Type: tpl.Type, TypeID: tpl.TypeID, Quality: tpl.Quality,
			Gems: tpl.Gems, Count: n, CraftName: tpl.CraftName, MakeType: tpl.MakeType}
		if err := db.AddCharacterItem(charID, &it); err != nil {
			return packets, count, err
		}
		*items = append(*items, it)
		packets = append(packets, buildAddItem(&it))
		count -= n
	}
	return packets, count, nil
}

// consumeOne decrements the item at index i by one, persisting it.
func (s *Session) consumeOne(i int) (db.Item, error) {
	it := s.items[i]
	it.Count--
	var err error
	if it.Count == 0 {
		err = db.DeleteCharacterItems(s.char.ID, it.UID)
		s.items = append(s.items[:i], s.items[i+1:]...)
	} else {
		err = db.SaveCharacterItem(s.char.ID, &it)
		s.items[i].Count = it.Count
	}
	return it, err
}

func cooldownForReqIg(reqIg uint8) time.Duration {
	ms := map[uint8]int{1: 15000, 2: 20000, 3: 25000, 4: 30000, 5: 60000, 6: 120000, 10: 600000, 11: 2000}[reqIg]
	return time.Duration(ms) * time.Millisecond
}

// handleUseItem (0x050A): bag, slot.
func (s *Session) handleUseItem(p []byte) {
	if len(p) < 4 || s.char == nil {
		return
	}
	ch := s.char
	bag, slot := p[2], p[3]
	if !isValidItemPosition(bag, slot) {
		return
	}
	reject := func(why string) {
		s.log.Printf("Rapture World rejected use item bag=%d slot=%d: %s", bag, slot, why)
		s.send(buildUseItemResult(ch.ID, nil), "0x050A use item rejected")
	}
	i := findItem(s.items, bag, slot)
	if i < 0 || s.items[i].Count == 0 {
		reject("empty slot")
		return
	}
	it := s.items[i]
	def := db.MustDefs().Item(it.Type, it.TypeID)
	if def == nil {
		reject("no item definition")
		return
	}
	if !isConsumableType(it.Type) {
		reject("not a consumable")
		return
	}
	switch def.Special {
	case specialChangeName:
		if err := db.EnableRename(ch.ID); err != nil {
			s.log.Printf("Rapture World failed to enable rename: %v", err)
			return
		}
		ch.RenamePending = true
		res, err := s.consumeOne(i)
		if err != nil {
			s.log.Printf("Rapture World failed to consume item: %v", err)
		}
		s.log.Printf("Rapture World name-change stone used character_id=%d", ch.ID)
		s.send(buildUseItemResult(ch.ID, &res), "0x050A use item")
		return
	case specialResetStats:
		resetStats(ch)
		res, err := s.consumeOne(i)
		if err != nil {
			s.log.Printf("Rapture World failed to consume item: %v", err)
		}
		s.persist()
		stats := s.stats()
		s.log.Printf("Rapture World reset stats character_id=%d stat_point=%d", ch.ID, ch.StatPoint)
		s.send(buildUseItemResult(ch.ID, &res), "0x050A use item")
		s.send(buildStatsReset(ch.StatPoint, ch.Str, ch.Rec, ch.Int, ch.Wis, ch.Dex, ch.Luc), "0x0214 stats reset")
		s.send(buildMaxEnergy(ch, 0, stats.MaxHP), "0x050B max hp (reset)")
		s.send(buildMaxEnergy(ch, 1, stats.MaxSP), "0x050B max sp (reset)")
		s.send(buildMaxEnergy(ch, 2, stats.MaxMP), "0x050B max mp (reset)")
		s.send(buildCurrentHitpoints(&stats), "0x0521 current hp/mp/sp (reset)")
		return
	}
	// CUser::ItemUse -> UseItemSkill: Range/AttackTime name a skill
	// (candies, remedies, charms) that the item casts on its user.
	if def.Special == specialNone && def.Range != 0 && def.AttackTime != 0 {
		if sk := db.MustDefs().Skill(uint16(def.Range), def.AttackTime); sk != nil && sk.TypeEffect == typeEffectBuff {
			cd := cooldownForReqIg(def.ReqIg)
			if last, ok := s.itemCooldowns[def.ReqIg]; ok && cd > 0 && time.Since(last) < cd {
				reject("cooldown")
				return
			}
			res, err := s.consumeOne(i)
			if err != nil {
				s.log.Printf("Rapture World failed to consume item: %v", err)
			}
			if cd > 0 {
				s.itemCooldowns[def.ReqIg] = time.Now()
			}
			s.persist()
			s.send(buildUseItemResult(ch.ID, &res), "0x050A use item")
			s.send(buildRemoveItem(&res, res.Count == 0), "0x0206 remove item")
			if _, ok := s.addEffect(sk, 0); ok {
				s.log.Printf("Rapture World item skill item=%d/%d skill_id=%d level=%d keep=%d", def.Type, def.TypeID, sk.SkillID, sk.SkillLevel, sk.KeepSeconds)
			}
			return
		}
	}
	hp, mp, sp := pos16(def.ConstHP), pos16(def.ConstMP), pos16(def.ConstSP)
	if (def.Special != specialNone && def.Special != specialPercentHealingPotion) || hp+mp+sp == 0 {
		reject(fmt.Sprintf("unsupported special=%d", def.Special))
		return
	}
	cd := cooldownForReqIg(def.ReqIg)
	if last, ok := s.itemCooldowns[def.ReqIg]; ok && cd > 0 && time.Since(last) < cd {
		reject("cooldown")
		return
	}
	before := s.stats()
	s.healDelta(recoveryAmount(hp, before.MaxHP, def.Special), recoveryAmount(mp, before.MaxMP, def.Special),
		recoveryAmount(sp, before.MaxSP, def.Special))
	res, err := s.consumeOne(i)
	if err != nil {
		s.log.Printf("Rapture World failed to consume item: %v", err)
	}
	if cd > 0 {
		s.itemCooldowns[def.ReqIg] = time.Now()
	}
	s.persist()
	after := s.stats()
	s.log.Printf("Rapture World used consumable %d/%d remaining=%d hp=%d mp=%d sp=%d", res.Type, res.TypeID, res.Count, after.CurrentHP, after.CurrentMP, after.CurrentSP)
	s.send(buildUseItemResult(ch.ID, &res), "0x050A use item")
	s.send(buildRemoveItem(&res, res.Count == 0), "0x0206 remove item")
	s.send(buildCurrentHitpoints(&after), "0x0521 current hp/mp/sp")
}

// healDelta raises effective HP/MP/SP by up to the given amounts, applied
// to the base fields as deltas (see CharacterStats). Returns the actual
// amounts gained.
func (s *Session) healDelta(hp, mp, sp uint32) (uint32, uint32, uint32) {
	ch := s.char
	st := s.stats()
	dh := min(satAdd(st.CurrentHP, hp), st.MaxHP) - st.CurrentHP
	dm := min(satAdd(st.CurrentMP, mp), st.MaxMP) - st.CurrentMP
	ds := min(satAdd(st.CurrentSP, sp), st.MaxSP) - st.CurrentSP
	ch.CurrentHP, ch.CurrentMP, ch.CurrentSP = satAdd(ch.CurrentHP, dh), satAdd(ch.CurrentMP, dm), satAdd(ch.CurrentSP, ds)
	return dh, dm, ds
}

// spendDelta lowers effective MP/SP by the given amounts (as deltas).
func (s *Session) spendDelta(mp, sp uint32) {
	ch := s.char
	st := s.stats()
	ch.CurrentMP = satSub(ch.CurrentMP, st.CurrentMP-satSub(st.CurrentMP, mp))
	ch.CurrentSP = satSub(ch.CurrentSP, st.CurrentSP-satSub(st.CurrentSP, sp))
}

// handleChangeAppearance (0x0226): bag, slot, hair, face, height, sex.
func (s *Session) handleChangeAppearance(p []byte) {
	if len(p) < 8 || s.char == nil {
		return
	}
	ch := s.char
	bag, slot, hair, face, height, sex := p[2], p[3], p[4], p[5], p[6], p[7]
	if !isValidItemPosition(bag, slot) {
		return
	}
	i := findItem(s.items, bag, slot)
	if i < 0 || s.items[i].Count == 0 || !isConsumableType(s.items[i].Type) {
		return
	}
	def := db.MustDefs().Item(s.items[i].Type, s.items[i].TypeID)
	if def == nil || (def.Special != specialChangeAppearance && def.Special != specialChangeSex) {
		return
	}
	newSex := ch.Gender
	if def.Special == specialChangeSex {
		if sex > 1 {
			return
		}
		newSex = sex
	}
	ch.Hair, ch.Face, ch.Height, ch.Gender = hair, face, height, newSex
	if err := db.SaveAppearance(ch.ID, hair, face, height, newSex); err != nil {
		s.log.Printf("Rapture World failed to save appearance: %v", err)
		return
	}
	res, err := s.consumeOne(i)
	if err != nil {
		s.log.Printf("Rapture World failed to consume item: %v", err)
	}
	s.syncPlayer()
	s.log.Printf("Rapture World change appearance character_id=%d hair=%d face=%d height=%d sex=%d", ch.ID, hair, face, height, newSex)
	s.send(buildUseItemResult(ch.ID, &res), "0x050A use item (change appearance)")
	frame := buildChangeAppearance(ch.ID, hair, face, height, newSex)
	s.broadcastNear(frame, "0x0226 change appearance", true)
}

// broadcastNear sends to viewers within the viewport that may see this
// character; includeSelf also echoes it to this session.
func (s *Session) broadcastNear(frame []byte, label string, includeSelf bool) {
	ch := s.char
	_ = ch
	s.sendToViewers(frame, label)
	if includeSelf {
		s.send(frame, label)
	}
}

// handleDropItem (0x0206): bag, slot, count.
func (s *Session) handleDropItem(p []byte) {
	if len(p) < 5 || s.char == nil {
		return
	}
	ch := s.char
	bag, slot, req := p[2], p[3], p[4]
	if !isValidItemPosition(bag, slot) {
		return
	}
	i := findItem(s.items, bag, slot)
	if i < 0 {
		return
	}
	it := s.items[i]
	n := req
	if n == 0 || n > it.Count {
		n = it.Count
	}
	if n == 0 {
		return
	}
	dropped := it
	dropped.Count, dropped.Bag, dropped.Slot, dropped.UID = n, 0, 0, 0
	// CUser::ItemDrop: the dropper owns it (no party share), at
	// CUser::GetDropPosRand: within half a unit, at the character's height.
	x, z := dropPosition(ch.PosX, ch.PosZ)
	mi := MapItem{ID: nextMapItemID(), Item: dropped, OwnerID: ch.ID, NoParty: true, DroppedAt: time.Now(), PosX: x, PosY: ch.PosY, PosZ: z}
	res := it
	res.Count = it.Count - n
	var err error
	if res.Count == 0 {
		err = db.DeleteCharacterItems(ch.ID, it.UID)
		s.items = append(s.items[:i], s.items[i+1:]...)
	} else {
		err = db.SaveCharacterItem(ch.ID, &res)
		s.items[i].Count = res.Count
	}
	if err != nil {
		s.log.Printf("Rapture World failed to drop item: %v", err)
		return
	}
	addMapItem(ch.MapID, mi)
	s.log.Printf("Rapture World dropped item uid=%d map_item_id=%d type=%d/%d count=%d remaining=%d", it.UID, mi.ID, it.Type, it.TypeID, n, res.Count)
	s.send(buildRemoveItem(&res, res.Count == 0), "0x0206 drop remove item")
	dispatchItemEnter(ch.MapID, &mi, "0x0401 drop map add item")
	if bag == 0 {
		if slot < 8 {
			s.equipmentChanged([]uint8{slot})
		}
		s.sendStatPackets()
	}
}

// equipmentChanged pushes 0x0507 for the slots to self and viewers.
func (s *Session) equipmentChanged(slots []uint8) {
	ch := s.char
	if s.player != nil {
		s.player.UpdateItems(s.items)
	}
	for _, slot := range slots {
		frame := buildCharacterEquipment(ch, s.items, slot)
		s.send(frame, fmt.Sprintf("0x0507 character equipment slot %d", slot))
		s.sendToViewers(frame, "0x0507 character equipment (broadcast)")
	}
}

// mobGoldBonus is ItemGet's mob-gold rate: ability 44's percent, +20 with
// the White Tiger charm, +50 with the Red Phoenix.
func (s *Session) mobGoldBonus(amount uint32) uint32 {
	rate, charm := int32(0), 0
	for _, b := range s.buffs {
		for _, a := range b.Abilities {
			switch {
			case a.Type == 44:
				rate = max(a.Value, 0)
			case a.Type >= 40 && a.Type <= 42 && a.Value > 0:
				charm = max(charm, int(a.Type)-39)
			}
		}
	}
	switch charm {
	case 2:
		rate += 20
	case 3:
		rate += 50
	}
	if rate <= 0 {
		return amount
	}
	return amount + uint32(math.Trunc(float64(float32(rate))/100*float64(float32(amount))))
}

// handlePickupItem (0x0205): map item id.
func (s *Session) handlePickupItem(p []byte) {
	if len(p) < 6 || s.char == nil {
		return
	}
	ch := s.char
	id, _ := packet.U32(p[2:])
	// CZone::GetPickItem: 3 missing, 0 reserved (CItem::PickEnable: the
	// owner, or its party for mob loot, for the first 30 s), 4 farther than
	// 10 units, 2 a quest item no active quest needs.
	mi, ok := findMapItem(ch.MapID, id)
	if !ok {
		s.send(buildAddItemError(3), "0x0205 pickup error")
		return
	}
	if mi.OwnerID != 0 && mi.OwnerID != ch.ID && time.Since(mi.DroppedAt) < mapItemOwnerWindow &&
		(mi.NoParty || !samePartyIDs(mi.OwnerID, ch.ID)) {
		s.send(buildAddItemError(0), "0x0205 pickup reserved")
		return
	}
	if DistanceXZ(mi.PosX, mi.PosZ, ch.PosX, ch.PosZ) > 10 {
		s.send(buildAddItemError(4), "0x0205 pickup too far")
		return
	}
	if mi.Money == 0 && realType(mi.Item.Type) == 27 && !s.questNeeds(mi.Item.Type, mi.Item.TypeID) {
		s.send(buildAddItemError(2), "0x0205 pickup quest item not needed")
		return
	}
	if mi.Money > 0 {
		removeMapItem(ch.MapID, mi.ID)
		amount := mi.Money
		if mi.FromMob {
			amount = s.mobGoldBonus(amount)
		}
		ch.Money = min(uint32(min(uint64(ch.Money)+uint64(amount), db.MoneyCap)), db.MoneyCap)
		s.persist()
		gold := db.Item{Bag: 1, Type: moneyItemType, Quality: uint16(min(mi.Money, 65535)), Count: 1}
		s.log.Printf("Rapture World picked up money amount=%d new_money=%d", mi.Money, ch.Money)
		s.send(buildAddItem(&gold), "0x0205 pickup money")
		s.send(buildSetMoney(ch.Money), "0x0213 pickup money update")
		dispatchEntityLeave(ch.MapID, viewItem, mi.ID, "0x0402 pickup map remove item")
		return
	}
	adds, left, err := s.giveItem(&mi.Item, mi.Item.Count, maxStackOf(mi.Item.Type, mi.Item.TypeID))
	if err != nil {
		s.log.Printf("Rapture World pickup failed: %v", err)
	}
	if len(adds) == 0 {
		s.send(buildAddItemError(1), "0x0205 pickup inventory full")
		return
	}
	for _, a := range adds {
		s.send(a, "0x0205 pickup add item")
	}
	dispatchEntityLeave(ch.MapID, viewItem, mi.ID, "0x0402 pickup map remove item")
	if left == 0 {
		removeMapItem(ch.MapID, mi.ID)
	} else if upd, ok := setMapItemCount(ch.MapID, mi.ID, left); ok {
		dispatchItemEnter(ch.MapID, &upd, "0x0401 pickup map add remaining item")
	}
	s.log.Printf("Rapture World picked up map_item_id=%d type=%d/%d remaining=%d", mi.ID, mi.Item.Type, mi.Item.TypeID, left)
	s.questPickup(mi.Item.Type, mi.Item.TypeID)
}

// moveItem performs an inventory move/swap/merge and returns the 0x0204.
func (s *Session) moveItem(srcBag, srcSlot, destBag, destSlot uint8) ([]byte, bool) {
	ch := s.char
	if !isValidItemPosition(srcBag, srcSlot) || !isValidItemPosition(destBag, destSlot) {
		return nil, false
	}
	si, di := findItem(s.items, srcBag, srcSlot), findItem(s.items, destBag, destSlot)
	at := func(i int) *db.Item {
		if i < 0 {
			return nil
		}
		return &s.items[i]
	}
	if si < 0 {
		return buildBagToBag(srcBag, srcSlot, nil, destBag, destSlot, at(di)), true
	}
	if srcBag == destBag && srcSlot == destSlot {
		return buildBagToBag(srcBag, srcSlot, at(si), destBag, destSlot, at(si)), true
	}
	if di >= 0 {
		src, dest := s.items[si], s.items[di]
		if itemsCanStack(&src, &dest) {
			if ms := maxStackOf(src.Type, src.TypeID); ms > 1 && dest.Count < ms {
				moved := min(src.Count, ms-dest.Count)
				s.items[di].Count += moved
				db.SaveCharacterItem(ch.ID, &s.items[di])
				s.items[si].Count -= moved
				db.SaveCharacterItem(ch.ID, &s.items[si])
				destUID := dest.UID
				var srcAfter *db.Item
				if s.items[si].Count == 0 {
					s.items = append(s.items[:si], s.items[si+1:]...)
				} else {
					c := s.items[si]
					srcAfter = &c
				}
				destAfter := s.items[findItemByUID(s.items, destUID)]
				s.log.Printf("Rapture World merged stack moved=%d", moved)
				return buildBagToBag(srcBag, srcSlot, srcAfter, destBag, destSlot, &destAfter), true
			}
		}
		s.items[si].Bag, s.items[si].Slot = destBag, destSlot
		s.items[di].Bag, s.items[di].Slot = srcBag, srcSlot
		db.SaveCharacterItem(ch.ID, &s.items[si])
		db.SaveCharacterItem(ch.ID, &s.items[di])
		s.log.Printf("Rapture World swapped items uid=%d <-> uid=%d", src.UID, dest.UID)
		return buildBagToBag(srcBag, srcSlot, &s.items[di], destBag, destSlot, &s.items[si]), true
	}
	s.items[si].Bag, s.items[si].Slot = destBag, destSlot
	if err := db.SaveCharacterItem(ch.ID, &s.items[si]); err != nil {
		s.log.Printf("Rapture World failed to move item: %v", err)
	}
	return buildBagToBag(srcBag, srcSlot, nil, destBag, destSlot, &s.items[si]), true
}

// handleItemBagToBag (0x0204): src bag/slot, dest bag/slot.
func (s *Session) handleItemBagToBag(p []byte) {
	if len(p) < 6 || s.char == nil {
		return
	}
	srcBag, srcSlot, destBag, destSlot := p[2], p[3], p[4], p[5]
	if srcBag == WarehouseBag || destBag == WarehouseBag {
		s.handleWarehouseMove(srcBag, srcSlot, destBag, destSlot)
		return
	}
	if srcBag == guildBankBag || destBag == guildBankBag {
		s.handleGuildBankMove(srcBag, srcSlot, destBag, destSlot)
		return
	}
	// Anything entering an equipment slot must pass CUser::EnableEquipment;
	// a refused move echoes the unchanged slots.
	if !s.equipAllowed(srcBag, srcSlot, destBag, destSlot) {
		var src, dest *db.Item
		if i := findItem(s.items, srcBag, srcSlot); i >= 0 {
			src = &s.items[i]
		}
		if i := findItem(s.items, destBag, destSlot); i >= 0 {
			dest = &s.items[i]
		}
		s.log.Printf("Rapture World refused equip (%d,%d)->(%d,%d)", srcBag, srcSlot, destBag, destSlot)
		s.send(buildBagToBag(srcBag, srcSlot, src, destBag, destSlot, dest), "0x0204 item move refused")
		return
	}
	frame, ok := s.moveItem(srcBag, srcSlot, destBag, destSlot)
	if !ok {
		return
	}
	s.send(frame, "0x0204 item bag-to-bag")
	var slots []uint8
	if srcBag == 0 && srcSlot < 8 {
		slots = append(slots, srcSlot)
	}
	if destBag == 0 && destSlot < 8 && !(srcBag == 0 && srcSlot == destSlot) {
		slots = append(slots, destSlot)
	}
	if len(slots) > 0 {
		s.equipmentChanged(slots)
	}
	if srcBag == 0 || destBag == 0 {
		s.sendStatPackets()
	}
}

// equipAllowed checks both directions of a move that touches the
// equipment bag.
func (s *Session) equipAllowed(srcBag, srcSlot, destBag, destSlot uint8) bool {
	defs := db.MustDefs()
	check := func(bag, slot uint8, item int) bool {
		if bag != 0 || item < 0 {
			return true
		}
		it := s.items[item]
		others := make([]db.Item, 0, len(s.items))
		for i, o := range s.items {
			if i != item && !(o.Bag == bag && o.Slot == slot) {
				others = append(others, o)
			}
		}
		return canEquip(s.char, defs.Item(it.Type, it.TypeID), slot, others)
	}
	if srcBag == 0 && destBag == 0 && srcSlot == destSlot {
		return true
	}
	return check(destBag, destSlot, findItem(s.items, srcBag, srcSlot)) &&
		check(srcBag, srcSlot, findItem(s.items, destBag, destSlot))
}
