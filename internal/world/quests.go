package world

import (
	"math/rand/v2"
	"sort"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// Quests from NpcQuest.SData (EP4/5 layout, documented by Parsec and
// recovered from ps_game.exe by Eternity: CUser::QuestEnableStart
// 0x0047f080, QuestStartRequest 0x0047f220, QuestFindItem 0x0047f8e0,
// the end/reward/progress routines). Packets: 0x0901 active list, 0x0906
// finished list, 0x0902 start (npc id, quest id), 0x0903 end / result,
// 0x0905 counter, 0x0907 reward choice, 0x0908 abandon.

type questItem struct{ Type, TypeID, Count uint8 }

type questResult struct {
	NeedMobID                 uint16 // counter selector for mob quests
	NeedMobCount              uint8
	NeedItemID, NeedItemCount uint8 // counter selector for item quests
	NeedTime                  uint32
	NeedHG                    uint16
	NeedVG                    int16
	NeedOG                    uint8
	Exp, Money                uint32
	ItemType, ItemTypeID      uint8
	NextQuest                 uint16
}

type questDef struct {
	ID                  uint16
	Name                string
	MinLevel, MaxLevel  uint16
	Faction, Mode       uint8
	Male, Female        bool
	Jobs                [6]uint8
	HG                  uint16 // also an alternative prerequisite quest
	VG                  int16  // likewise
	CG, OG, IG          uint8
	PrevQuest           uint16
	RequireParty        bool
	PartyJobs           [6]uint8
	MinMinutes          uint32
	MaxMinutes          uint32
	TickStart, TickKeep uint32
	TickReceive         uint32 // nonzero: repeatable
	StartType           uint8
	StartNpcType        uint8
	StartNpcID          uint16
	StartItemType       uint8
	StartItemID         uint8
	StartItems          [3]questItem // given on start
	EndType             uint8        // 0 npc, 1 items, 2 mobs, 3 player kills
	EndNpcType          uint8
	EndNpcID            uint16
	FarmItems           [3]questItem
	PvpKills            uint8
	MobIDs              [2]uint16
	MobCounts           [2]uint8
	ResultType          uint8
	UserSelect          uint8
	Results             [3]questResult
}

type questItemLink struct{ Start, End []uint16 }

var (
	questDefs      []questDef
	questItemLinks = map[[2]uint8]questItemLink{}
	mobQuestLinks  = map[uint16][]uint16{}
)

func readQuestItem(r *sdataReader) questItem {
	return questItem{r.u8(), r.u8(), r.u8()}
}

func readQuest(r *sdataReader, q *questDef) {
	q.ID = r.u16()
	q.Name = r.str()
	r.str() // summary
	q.MinLevel, q.MaxLevel = r.u16(), r.u16()
	q.Faction, q.Mode = r.u8(), r.u8()
	q.Male, q.Female = r.u8() != 0, r.u8() != 0
	for i := range q.Jobs {
		q.Jobs[i] = r.u8()
	}
	q.HG, q.VG = r.u16(), int16(r.u16())
	q.CG, q.OG, q.IG = r.u8(), r.u8(), r.u8()
	q.PrevQuest = r.u16()
	q.RequireParty = r.u8() != 0
	for i := range q.PartyJobs {
		q.PartyJobs[i] = r.u8()
	}
	q.MinMinutes, q.MaxMinutes = r.u32(), r.u32()
	q.TickStart, q.TickKeep, q.TickReceive = r.u32(), r.u32(), r.u32()
	q.StartType, q.StartNpcType, q.StartNpcID = r.u8(), r.u8(), r.u16()
	q.StartItemType, q.StartItemID = r.u8(), r.u8()
	for i := range q.StartItems {
		q.StartItems[i] = readQuestItem(r)
	}
	q.EndType, q.EndNpcType, q.EndNpcID = r.u8(), r.u8(), r.u16()
	for i := range q.FarmItems {
		q.FarmItems[i] = readQuestItem(r)
	}
	q.PvpKills = r.u8()
	q.MobIDs[0], q.MobCounts[0] = r.u16(), r.u8()
	q.MobIDs[1], q.MobCounts[1] = r.u16(), r.u8()
	q.ResultType, q.UserSelect = r.u8(), r.u8()
	for i := range q.Results {
		res := &q.Results[i]
		res.NeedMobID, res.NeedMobCount = r.u16(), r.u8()
		res.NeedItemID, res.NeedItemCount = r.u8(), r.u8()
		res.NeedTime = r.u32()
		res.NeedHG, res.NeedVG, res.NeedOG = r.u16(), int16(r.u16()), r.u8()
		res.Exp, res.Money = r.u32(), r.u32()
		res.ItemType, res.ItemTypeID = r.u8(), r.u8()
		res.NextQuest = r.u16()
	}
	for range 4 { // descriptions
		r.str()
	}
	for range 3 { // completion messages
		r.str()
	}
}

// findQuestDef is GetQuest: by row (1-based), not the embedded id.
func findQuestDef(id uint16) *questDef {
	if id == 0 || int(id) > len(questDefs) {
		return nil
	}
	return &questDefs[id-1]
}

// buildMobQuestLinks maps mobs to the mob-kill quests that count them,
// ascending by minimum level (MobQuestLinks::build).
func buildMobQuestLinks() {
	links := map[uint16][]uint16{}
	for i := range questDefs {
		q := &questDefs[i]
		if q.EndType != 2 || q.MobIDs[0] == 0 {
			continue
		}
		for _, mob := range q.MobIDs {
			if mob == 0 {
				continue
			}
			id := uint16(i + 1)
			list := links[mob]
			dup := false
			for _, e := range list {
				dup = dup || e == id
			}
			if !dup {
				links[mob] = append(list, id)
			}
		}
	}
	for mob, list := range links {
		sort.SliceStable(list, func(a, b int) bool {
			return findQuestDef(list[a]).MinLevel < findQuestDef(list[b]).MinLevel
		})
		links[mob] = list
	}
	mobQuestLinks = links
}

// activeQuest is a quest in progress.
type activeQuest struct {
	ID        uint16
	def       *questDef
	counters  [3]uint8
	startAt   time.Time
	endAt     time.Time // timed quests
	successAt time.Time // MinMinutes elapsed
	success   uint8     // 1: waiting for the reward choice
}

func (q *activeQuest) delayMinutes() int16 {
	if q.def.MaxMinutes != 0 {
		return int16(max(time.Until(q.endAt), 0) / time.Minute)
	}
	return int16(min(time.Since(q.startAt)/time.Minute, 32767))
}

func (q *activeQuest) row() db.CharQuest {
	return db.CharQuest{QuestID: q.ID, Delay: q.delayMinutes(), Counts: q.counters}
}

// loadQuests restores the character's quests as PacketUserDBSetInfo does.
func (s *Session) loadQuests() {
	s.quests, s.finishedQuests = nil, nil
	rows, err := db.CharacterQuests(s.char.ID)
	s.logErr("load quests", err)
	now := time.Now()
	for _, r := range rows {
		def := findQuestDef(r.QuestID)
		if def == nil {
			continue
		}
		q := &activeQuest{ID: r.QuestID, def: def, counters: r.Counts}
		delay := time.Duration(max(r.Delay, 0)) * time.Minute
		if def.MaxMinutes != 0 {
			q.endAt = now.Add(delay)
			q.startAt = q.endAt.Add(-time.Duration(def.MaxMinutes) * time.Minute)
		} else {
			q.startAt = now.Add(-delay)
		}
		if def.MinMinutes != 0 {
			q.successAt = q.startAt.Add(time.Duration(def.MinMinutes) * time.Minute)
		}
		s.quests = append(s.quests, q)
	}
	fin, err := db.FinishedQuests(s.char.ID)
	s.logErr("load finished quests", err)
	s.finishedQuests = fin
}

// questListPackets are 0x0901 (id, minutes left, counters) and 0x0906
// (id, success), up to 255 entries each.
func (s *Session) questListPackets() [][]byte {
	var out [][]byte
	for start := 0; start == 0 || start < len(s.quests); start += 255 {
		end := min(start+255, len(s.quests))
		w := packet.New(packet.QuestList)
		w.U8(uint8(end - start))
		for _, q := range s.quests[start:end] {
			w.U16(q.ID)
			left := uint16(0)
			if q.def.MaxMinutes != 0 {
				left = uint16(max(time.Until(q.endAt), 0) / time.Minute)
			}
			w.U16(left)
			w.Bytes(q.counters[:])
		}
		out = append(out, w.MustFinish())
	}
	for start := 0; start == 0 || start < len(s.finishedQuests); start += 255 {
		end := min(start+255, len(s.finishedQuests))
		w := packet.New(packet.QuestFinishedList)
		w.U8(uint8(end - start))
		for _, f := range s.finishedQuests[start:end] {
			w.U16(f.QuestID)
			w.U8(f.Success)
		}
		out = append(out, w.MustFinish())
	}
	return out
}

// saveQuests writes every active quest's delay and counters.
func (s *Session) saveQuests() {
	for _, q := range s.quests {
		s.logErr("save quest", db.SaveCharacterQuest(s.char.ID, q.row()))
	}
}

func (s *Session) findQuest(id uint16) *activeQuest {
	for _, q := range s.quests {
		if q.ID == id {
			return q
		}
	}
	return nil
}

func (s *Session) questFinished(id uint16, onlySuccess bool) bool {
	for _, f := range s.finishedQuests {
		if f.QuestID == id && (!onlySuccess || f.Success != 0) {
			return true
		}
	}
	return false
}

// questPrerequisites: PrevQuest, HG or VG finished successfully (any).
func (s *Session) questPrerequisites(q *questDef) bool {
	ids := []uint16{q.PrevQuest, q.HG, uint16(q.VG)}
	if ids[0] == 0 && ids[1] == 0 && ids[2] == 0 {
		return true
	}
	for _, id := range ids {
		if id != 0 && s.questFinished(id, true) {
			return true
		}
	}
	return false
}

// questEnableStart is CUser::QuestEnableStart: level range, race
// (Faction), job, mode and prerequisites.
func (s *Session) questEnableStart(q *questDef) bool {
	ch := s.char
	if ch.Level < q.MinLevel || ch.Level > q.MaxLevel || ch.Class >= 6 || q.Jobs[ch.Class] == 0 {
		return false
	}
	if !raceAllowed(q.Faction, ch.Race) {
		return false
	}
	switch ch.Mode {
	case growEasy:
		if q.Mode != 0 {
			return false
		}
	case growNormal:
		if q.Mode > 1 {
			return false
		}
	case growHard:
		if q.Mode == 3 {
			return false
		}
	}
	return s.questPrerequisites(q)
}

// raceAllowed is the Country/Faction race rule shared with items.
func raceAllowed(country, race uint8) bool {
	switch country {
	case 0, 1:
		return race == country
	case 2:
		return race == 0 || race == 1
	case 3:
		return race == 3
	case 4:
		return race == 2
	case 5:
		return race == 2 || race == 3
	}
	return true
}

func (s *Session) npcMatches(npcID uint32, npcType uint8, typeID uint16) (MapNpc, bool) {
	npc, ok := npcOnMap(s.char.MapID, npcID)
	return npc, ok && npc.Type == npcType && npc.TypeID == typeID
}

// findInventoryItem finds (and optionally consumes one of) an item.
func (s *Session) findInventoryItem(t, id uint8, consume bool) bool {
	for i := range s.items {
		it := s.items[i]
		if it.Bag >= InventoryFirstBag && it.Bag <= InventoryLastBag && it.Type == t && it.TypeID == id {
			if consume {
				res, err := s.consumeOne(i)
				s.logErr("consume quest item", err)
				s.send(buildRemoveItem(&res, res.Count == 0), "0x0206 quest item used")
			}
			return true
		}
	}
	return false
}

// handleQuestStart (0x0902): npc id, quest id (CUser::QuestStartRequest).
func (s *Session) handleQuestStart(p []byte) {
	if len(p) < 8 || s.char == nil {
		return
	}
	npcID, _ := packet.U32(p[2:])
	id, _ := packet.U16(p[6:])
	q := findQuestDef(id)
	if q == nil || q.StartType > 4 || q.StartType == 4 {
		return
	}
	if q.StartType <= 2 {
		npc, ok := npcOnMap(s.char.MapID, npcID)
		if !ok {
			return
		}
		if (q.StartType != 0 || q.StartNpcType != 0) && (npc.Type != q.StartNpcType || npc.TypeID != q.StartNpcID) {
			return
		}
		if q.StartType == 0 && !s.questPrerequisites(q) {
			return
		}
	} else if npcID != uint32(q.StartItemID) {
		return
	}
	if s.findQuest(id) != nil || s.questFinished(id, false) || !s.questEnableStart(q) {
		s.log.Printf("Rapture World quest %d refused (active, finished or requirements)", id)
		return
	}
	if (q.StartType == 2 || q.StartType == 3) && !s.findInventoryItem(q.StartItemType, q.StartItemID, q.StartType == 2) {
		return
	}
	s.startQuest(q, id, npcID)
}

// startQuest adds the quest, saves it, tells the client and hands out
// the start items.
func (s *Session) startQuest(q *questDef, id uint16, npcID uint32) {
	now := time.Now()
	aq := &activeQuest{ID: id, def: q, startAt: now}
	if q.MaxMinutes != 0 {
		aq.endAt = now.Add(time.Duration(q.MaxMinutes) * time.Minute)
	}
	if q.MinMinutes != 0 {
		aq.successAt = now.Add(time.Duration(q.MinMinutes) * time.Minute)
	}
	if err := db.AddCharacterQuest(s.char.ID, db.CharQuest{QuestID: id, Delay: int16(min(q.MaxMinutes, 32767))}); err != nil {
		s.log.Printf("Rapture World quest %d start failed: %v", id, err)
		return
	}
	s.quests = append(s.quests, aq)
	w := packet.New(packet.QuestStart)
	w.U32(npcID)
	w.U16(id)
	s.send(w.MustFinish(), "0x0902 quest start")
	for _, it := range q.StartItems {
		if it.Type != 0 {
			s.giveQuestItem(it.Type, it.TypeID, max(it.Count, 1))
		}
	}
	s.log.Printf("Rapture World quest %d %q started", id, q.Name)
}

// giveQuestItem adds an item to the inventory and tells the client; it
// returns the item's placement, if any.
func (s *Session) giveQuestItem(t, id, count uint8) *db.Item {
	def := db.MustDefs().Item(t, id)
	if def == nil {
		return nil
	}
	tpl := db.Item{Type: t, TypeID: id, Quality: def.Quality, MakeType: "Q"}
	adds, _, err := s.giveItem(&tpl, count, def.MaxStack())
	s.logErr("give quest item", err)
	for _, a := range adds {
		s.send(a, "0x0205 quest item")
	}
	for i := len(s.items) - 1; i >= 0; i-- {
		if s.items[i].Type == t && s.items[i].TypeID == id {
			return &s.items[i]
		}
	}
	return nil
}

// questFindItems is QuestFindItem (0x0047f8e0): the farm items must all be
// held; counters take the held totals; consuming removes what the chosen
// reward variant needs (whole stacks for quest-type items).
func (s *Session) questFindItems(q *activeQuest, consume bool) bool {
	d := q.def
	var totals [3]int
	for _, it := range s.items {
		if it.Bag < InventoryFirstBag || it.Bag > InventoryLastBag {
			continue
		}
		for i, f := range d.FarmItems {
			if f.Type != 0 && it.Type == f.Type && it.TypeID == f.TypeID {
				totals[i] += int(it.Count)
				break
			}
		}
	}
	var remaining [3]int
	for i, f := range d.FarmItems {
		if totals[i] < int(f.Count) {
			return false
		}
		if totals[i] > int(q.counters[i]) {
			q.counters[i] = f.Count
		} else {
			q.counters[i] = uint8(min(totals[i], 255))
		}
		remaining[i] = int(f.Count)
	}
	if !consume {
		return true
	}
	if d.ResultType != 1 && d.UserSelect == 0 {
		for v := 2; v >= 0; v-- {
			sel, req := d.Results[v].NeedItemID, d.Results[v].NeedItemCount
			if d.EndType == 1 && req != 0 && sel >= 1 && sel <= 3 {
				q.counters[v] = uint8(min(totals[v], 255))
				if int(q.counters[sel-1]) >= int(req) {
					remaining[sel-1] = int(req)
					break
				}
			}
		}
	}
	for i := len(s.items) - 1; i >= 0; i-- {
		it := s.items[i]
		if it.Bag < InventoryFirstBag || it.Bag > InventoryLastBag {
			continue
		}
		for k, f := range d.FarmItems {
			if f.Type == 0 || it.Type != f.Type || it.TypeID != f.TypeID {
				continue
			}
			take := remaining[k]
			if isQuestItemType(it.Type) {
				take = int(it.Count)
			}
			take = min(take, int(it.Count))
			if take <= 0 {
				break
			}
			remaining[k] -= take
			res := it
			res.Count -= uint8(take)
			s.items[i].Count = res.Count
			s.logErr("consume quest item", db.SaveCharacterItem(s.char.ID, &s.items[i]))
			s.send(buildRemoveItem(&res, res.Count == 0), "0x0206 quest item handed in")
			if res.Count == 0 {
				s.items = append(s.items[:i], s.items[i+1:]...)
			}
			break
		}
	}
	return true
}

// handleQuestEnd (0x0903): npc id, quest id (quest_end_request).
func (s *Session) handleQuestEnd(p []byte) {
	if len(p) < 8 || s.char == nil {
		return
	}
	npcID, _ := packet.U32(p[2:])
	id, _ := packet.U16(p[6:])
	q := s.findQuest(id)
	if q == nil {
		return
	}
	d := q.def
	if d.EndType > 3 || (d.EndType == 0 && d.EndNpcType == 0) {
		return
	}
	if d.EndType == 0 || npcID != 0 {
		if _, ok := s.npcMatches(npcID, d.EndNpcType, d.EndNpcID); !ok {
			return
		}
	}
	switch d.EndType {
	case 1:
		if !s.questFindItems(q, d.UserSelect == 0 || d.EndNpcType == 0) {
			return
		}
	case 2:
		if q.counters[0] < d.MobCounts[0] || q.counters[1] < d.MobCounts[1] {
			return
		}
	case 3:
		if q.counters[0] < d.PvpKills {
			return
		}
	}
	ctx := npcID
	if d.EndType >= 2 {
		ctx = 0
	}
	s.completeQuest(q, ctx, true)
}

// completeQuest is quest_requests_complete.
func (s *Session) completeQuest(q *activeQuest, ctx uint32, success bool) {
	d := q.def
	if !success {
		w := packet.New(packet.QuestEnd)
		w.U32(ctx)
		w.U16(q.ID)
		w.Bytes(make([]byte, 14))
		s.send(w.MustFinish(), "0x0903 quest failed")
		s.removeQuest(q, 0)
		s.log.Printf("Rapture World quest %d failed", q.ID)
		return
	}
	if d.MinMinutes != 0 && time.Now().Before(q.successAt) {
		return
	}
	if d.ResultType == 1 {
		s.awardQuest(q, 0, ctx)
		return
	}
	if d.UserSelect != 0 {
		q.success = 1
		w := packet.New(packet.QuestChoose)
		w.U16(q.ID)
		s.send(w.MustFinish(), "0x0907 quest reward choice")
		return
	}
	for v := 2; v >= 0; v-- {
		var sel, req uint16
		switch d.EndType {
		case 1:
			sel, req = uint16(d.Results[v].NeedItemID), uint16(d.Results[v].NeedItemCount)
			if sel > 3 {
				sel = 0
			}
		case 2:
			sel, req = d.Results[v].NeedMobID, uint16(d.Results[v].NeedMobCount)
			if sel > 2 {
				sel = 0
			}
		}
		if v == 0 || (sel != 0 && req != 0 && uint16(q.counters[sel-1]) >= req) {
			s.awardQuest(q, uint8(v), ctx)
			return
		}
	}
}

// handleQuestChoose (0x0907): quest id, reward index.
func (s *Session) handleQuestChoose(p []byte) {
	if len(p) < 5 || s.char == nil {
		return
	}
	id, _ := packet.U16(p[2:])
	choice := p[4]
	q := s.findQuest(id)
	if q == nil || q.success == 0 || choice >= q.def.UserSelect || choice >= 3 {
		return
	}
	if q.def.EndNpcType != 0 && !s.questFindItems(q, true) {
		return
	}
	s.awardQuest(q, choice, 0)
}

// handleQuestAbandon (0x0908): quest id.
func (s *Session) handleQuestAbandon(p []byte) {
	if len(p) < 4 || s.char == nil {
		return
	}
	id, _ := packet.U16(p[2:])
	if q := s.findQuest(id); q != nil {
		s.completeQuest(q, 0, false)
	}
}

// awardQuest is award_quest_reward: exp (doubled unless the quest is
// ultimate-only), gold and an item; 0x0903 reports them.
func (s *Session) awardQuest(q *activeQuest, variant uint8, ctx uint32) {
	res := q.def.Results[variant]
	exp := res.Exp
	if q.def.Mode <= 2 {
		exp += exp
	}
	ch := s.char
	if res.Money != 0 {
		ch.Money = uint32(min(uint64(ch.Money)+uint64(res.Money), db.MoneyCap))
		s.persist()
		s.send(buildSetMoney(ch.Money), "0x0213 quest gold")
	}
	w := packet.New(packet.QuestEnd)
	w.U32(ctx)
	w.U16(q.ID)
	w.U8(1)
	w.U8(variant)
	w.U32(exp)
	w.U32(res.Money)
	var it *db.Item
	if res.ItemType != 0 {
		it = s.giveQuestItem(res.ItemType, res.ItemTypeID, 1)
	}
	if it != nil {
		w.Bytes([]byte{it.Bag, it.Slot, it.Type, it.TypeID})
	} else {
		w.Bytes(make([]byte, 4))
	}
	s.send(w.MustFinish(), "0x0903 quest completed")
	if exp > 0 {
		s.awardExp(exp)
		s.send(buildExpGain(exp), "0x0207 quest exp")
	}
	s.removeQuest(q, 1)
	s.log.Printf("Rapture World quest %d completed variant %d exp %d money %d", q.ID, variant, exp, res.Money)
}

// removeQuest is QuestRuntime::remove: repeatable quests (TickReceive)
// are deleted, others finish with their success flag.
func (s *Session) removeQuest(q *activeQuest, success uint8) {
	for i, a := range s.quests {
		if a == q {
			s.quests = append(s.quests[:i], s.quests[i+1:]...)
			break
		}
	}
	if q.def.TickReceive != 0 {
		s.logErr("delete quest", db.DeleteCharacterQuest(s.char.ID, q.ID))
		return
	}
	s.logErr("finish quest", db.FinishCharacterQuest(s.char.ID, q.ID, success))
	s.finishedQuests = append(s.finishedQuests, db.FinishedQuest{QuestID: q.ID, Success: success})
}

func (s *Session) bumpQuestCounter(q *activeQuest, slot int) {
	if q.counters[slot] == 255 {
		return
	}
	q.counters[slot]++
	s.logErr("save quest counter", db.SaveCharacterQuest(s.char.ID, q.row()))
	w := packet.New(packet.QuestCounter)
	w.U16(q.ID)
	w.U8(uint8(slot))
	w.U8(q.counters[slot])
	s.send(w.MustFinish(), "0x0905 quest counter")
}

// questKill is update_mob_kill_quests for one kill of mobID.
func (s *Session) questKill(mobID uint16) {
	if s.char == nil {
		return
	}
	now := time.Now()
	for _, id := range mobQuestLinks[mobID] {
		q := s.findQuest(id)
		if q == nil || q.def.EndType != 2 || (q.def.MinMinutes != 0 && now.After(q.successAt)) {
			continue
		}
		for slot := 0; slot < 2; slot++ {
			if q.def.MobIDs[slot] != mobID {
				continue
			}
			s.bumpQuestCounter(q, slot)
			if q.def.EndNpcType == 0 && q.counters[0] >= q.def.MobCounts[0] && q.counters[1] >= q.def.MobCounts[1] {
				s.completeQuest(q, 0, true)
				break
			}
		}
	}
}

// questNeeds: an item that starts a quest, or one an active quest ends with.
func (s *Session) questNeeds(t, id uint8) bool {
	link, ok := questItemLinks[[2]uint8{t, id}]
	if !ok {
		return false
	}
	if len(link.Start) > 0 {
		return true
	}
	for _, qid := range link.End {
		if s.findQuest(qid) != nil {
			return true
		}
	}
	return false
}

// questPickup is quest_requests_progress_pick_item.
func (s *Session) questPickup(t, id uint8) {
	link, ok := questItemLinks[[2]uint8{t, id}]
	if !ok || s.char == nil {
		return
	}
	now := time.Now()
	for _, qid := range link.End {
		q := s.findQuest(qid)
		if q == nil || q.def.EndType != 1 || (q.def.MinMinutes != 0 && now.After(q.successAt)) {
			continue
		}
		for slot, f := range q.def.FarmItems {
			if f.Type != t || f.TypeID != id {
				continue
			}
			s.bumpQuestCounter(q, slot)
			if q.def.EndNpcType == 0 && s.questFindItems(q, true) {
				s.completeQuest(q, 0, true)
			}
			break
		}
	}
}

// questTimers fails timed quests whose time ran out.
func (s *Session) questTimers() {
	now := time.Now()
	for _, q := range append([]*activeQuest(nil), s.quests...) {
		if q.def.MaxMinutes != 0 && !now.Before(q.endAt) {
			s.completeQuest(q, 0, false)
		}
	}
}

// questItemDrop is mob_loot's quest item: Mobs.QuestItemID drops (chance =
// the item's Quality per cent) when it starts a quest or any recipient has
// a quest it ends.
func questItemDrop(mob *MapMob, recipients []*ConnectedPlayer) *db.ItemDef {
	if mob.def == nil || mob.def.QuestItemID <= 0 {
		return nil
	}
	t, id := uint8(mob.def.QuestItemID/1000), uint8(mob.def.QuestItemID%1000)
	def := db.MustDefs().Item(t, id)
	if def == nil {
		return nil
	}
	link := questItemLinks[[2]uint8{t, id}]
	eligible := len(link.Start) > 0
	for _, p := range recipients {
		for _, qid := range link.End {
			eligible = eligible || p.hasQuest(qid)
		}
	}
	if eligible && rand.IntN(100) < int(def.Quality) {
		return def
	}
	return nil
}

// syncQuestIDs publishes the active quest ids for other sessions' loot.
func (s *Session) syncQuestIDs() {
	if s.player == nil {
		return
	}
	ids := make([]uint16, 0, len(s.quests))
	for _, q := range s.quests {
		ids = append(ids, q.ID)
	}
	s.player.setQuests(ids)
}
