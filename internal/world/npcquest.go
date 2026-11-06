package world

import (
	"encoding/binary"
	"errors"
	"math"
	"os"
	"path/filepath"
	"sync"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// NpcQuest.SData holds the NPC definitions (DBInitializeConstNpcQuest
// 0x004caf80; format recovered by Eternity's npcs.cpp): 13 lists, one
// per NPC type, indexed by row (type id - 1). Type 1 NPCs are shops with
// their item list; type 2 are gatekeepers with three destinations.

const (
	npcTypeShop       = 1
	npcTypeGatekeeper = 2
	npcTypeCount      = 13
	gatekeeperReach   = 30.0
)

type npcGate struct {
	MapID   uint16
	X, Y, Z float32
	Name    string
	Money   uint32
}

type npcDef struct {
	Type      uint8
	TypeID    uint16
	Country   uint8 // 0 light, 1 fury, 2 both
	Name      string
	ShopType  uint8
	ShopItems [][2]uint8
	Gates     [3]npcGate
}

var npcDefs [npcTypeCount][]npcDef

type sdataReader struct {
	b   []byte
	pos int
}

var errShortSData = errors.New("truncated NpcQuest.SData")

func (r *sdataReader) take(n int) []byte {
	if n < 0 || r.pos+n > len(r.b) {
		panic(errShortSData)
	}
	out := r.b[r.pos : r.pos+n]
	r.pos += n
	return out
}

func (r *sdataReader) u32() uint32 { return binary.LittleEndian.Uint32(r.take(4)) }
func (r *sdataReader) u16() uint16 { return binary.LittleEndian.Uint16(r.take(2)) }
func (r *sdataReader) u8() uint8   { return r.take(1)[0] }
func (r *sdataReader) f32() float32 {
	return math.Float32frombits(r.u32())
}
func (r *sdataReader) str() string {
	b := r.take(int(r.u32()))
	for i, c := range b {
		if c == 0 {
			b = b[:i]
			break
		}
	}
	return string(b)
}
func (r *sdataReader) words() {
	n := r.u32()
	r.take(int(n) * 2)
}

func loadNpcQuest(log *logx.Logger) (err error) {
	raw, err := os.ReadFile(filepath.Join(gameDataDir(), "NpcQuest.SData"))
	if err != nil {
		return err
	}
	defer func() {
		if p := recover(); p != nil {
			err = errShortSData
		}
	}()
	r := &sdataReader{b: raw}
	shops, gates := 0, 0
	for group := 0; group < npcTypeCount; group++ {
		count := r.u32()
		if count > 65536 {
			return errors.New("invalid NPC list count in NpcQuest.SData")
		}
		list := make([]npcDef, count)
		for i := range list {
			n := &list[i]
			n.Type = r.u8()
			n.TypeID = r.u16()
			if group == 0 {
				n.ShopType = r.u8()
			}
			r.take(12) // model, move distance, move speed
			switch r.u32() {
			case 1:
				n.Country = 0
			case 2:
				n.Country = 1
			default:
				n.Country = 2
			}
			n.Name = r.str()
			r.str() // description
			switch group {
			case 0:
				items := r.u32()
				n.ShopItems = make([][2]uint8, items)
				for k := range n.ShopItems {
					n.ShopItems[k] = [2]uint8{r.u8(), r.u8()}
				}
				shops++
			case 1:
				for k := range n.Gates {
					g := &n.Gates[k]
					g.MapID = r.u16()
					g.X = r.f32()
					g.Y = float32(float64(r.f32()) + float64(float32(0.9)))
					g.Z = r.f32()
					g.Name = r.str()
					g.Money = r.u32()
				}
				gates++
			}
			r.words() // start quests
			r.words() // end quests
		}
		npcDefs[group] = list
	}
	// 256x256 item links: the quests each item starts and ends.
	for t := 0; t < 256; t++ {
		for id := 0; id < 256; id++ {
			starts, ends := r.wordList(), r.wordList()
			if len(starts) > 0 || len(ends) > 0 {
				questItemLinks[[2]uint8{uint8(t), uint8(id)}] = questItemLink{Start: starts, End: ends}
			}
		}
	}
	count := r.u32()
	if count > 65535 {
		return errors.New("quest count exceeds WORD identifiers in NpcQuest.SData")
	}
	quests := make([]questDef, count)
	for i := range quests {
		readQuest(r, &quests[i])
	}
	questDefs = quests
	buildMobQuestLinks()
	log.Printf("Rapture World loaded NpcQuest.SData shops=%d gatekeepers=%d item_links=%d quests=%d trailing=%d",
		shops, gates, len(questItemLinks), len(quests), len(raw)-r.pos)
	return nil
}

func (r *sdataReader) wordList() []uint16 {
	n := r.u32()
	if n > 65536 {
		panic(errShortSData)
	}
	out := make([]uint16, n)
	for i := range out {
		out[i] = r.u16()
	}
	return out
}

// findNpcDef is GetNpc (0x0041d640): by type and row.
func findNpcDef(npcType uint8, typeID uint16) *npcDef {
	if npcType < 1 || npcType > npcTypeCount || typeID < 1 || int(typeID) > len(npcDefs[npcType-1]) {
		return nil
	}
	return &npcDefs[npcType-1][typeID-1]
}

func npcOnMap(mapID uint16, id uint32) (MapNpc, bool) {
	for _, n := range EntitiesOnMap(mapID).NPCs {
		if n.ID == id {
			return n, true
		}
	}
	return MapNpc{}, false
}

func buildShopBuyResult(result, bag, slot, itemType, typeID, count uint8, money uint32) []byte {
	w := packet.New(packet.ShopBuy)
	w.Bytes([]byte{result, bag, slot, itemType, typeID, count})
	w.U32(money)
	return w.MustFinish()
}

// handleShopBuy (0x0702): npc id, item index, count; CUser::PacketShop
// (0x0046af10). Result 0 bought, 1 not enough gold, 2 no room. A stack
// must take the whole purchase; otherwise the first free slot is used.
func (s *Session) handleShopBuy(p []byte) {
	if len(p) < 8 || s.char == nil {
		return
	}
	ch := s.char
	npcID, _ := packet.U32(p[2:])
	index, count := p[6], p[7]
	npc, ok := npcOnMap(ch.MapID, npcID)
	if !ok || npc.Type != npcTypeShop {
		return
	}
	def := findNpcDef(npc.Type, npc.TypeID)
	if def == nil || int(index) >= len(def.ShopItems) {
		return
	}
	entry := def.ShopItems[index]
	item := db.MustDefs().Item(entry[0], entry[1])
	if item == nil || count == 0 || count > item.MaxStack() {
		return
	}
	price := uint64(count) * uint64(max(item.Buy, 0))
	if uint64(ch.Money) < price {
		s.send(buildShopBuyResult(1, 0, 0, item.Type, item.TypeID, count, 0), "0x0702 shop buy (no gold)")
		return
	}
	var placed *db.Item
	if item.MaxStack() > 1 {
		for i := range s.items {
			it := &s.items[i]
			if it.Bag >= 1 && it.Bag <= InventoryLastBag && it.Type == item.Type && it.TypeID == item.TypeID &&
				uint16(it.Count)+uint16(count) <= uint16(item.MaxStack()) {
				it.Count += count
				if err := db.SaveCharacterItem(ch.ID, it); err != nil {
					s.log.Printf("Rapture World shop stack failed: %v", err)
					return
				}
				placed = it
				break
			}
		}
	}
	if placed == nil {
		free := freeInventorySlots(s.items)
		if len(free) == 0 {
			s.send(buildShopBuyResult(2, 0, 0, item.Type, item.TypeID, 0, 0), "0x0702 shop buy (no room)")
			return
		}
		n := uint8(1)
		if item.MaxStack() > 1 {
			n = count
		}
		it := db.Item{Bag: free[0][0], Slot: free[0][1], Type: item.Type, TypeID: item.TypeID, Quality: item.Quality,
			Count: n, MakeType: "B"}
		if err := db.AddCharacterItem(ch.ID, &it); err != nil {
			s.log.Printf("Rapture World shop item failed: %v", err)
			return
		}
		s.items = append(s.items, it)
		placed = &s.items[len(s.items)-1]
	}
	ch.Money -= uint32(price)
	s.persist()
	s.log.Printf("Rapture World bought %d/%d x%d for %d at npc %d", item.Type, item.TypeID, count, price, npc.TypeID)
	s.send(buildShopBuyResult(0, placed.Bag, placed.Slot, placed.Type, placed.TypeID, placed.Count, ch.Money), "0x0702 shop buy")
}

func buildGatekeeperResult(ok bool, money uint32) []byte {
	w := packet.New(packet.Gatekeeper)
	w.Bool(ok)
	w.U32(money)
	return w.MustFinish()
}

// handleGatekeeper (0x020C): npc id, destination 0-2. A gatekeeper of the
// character's faction (or neutral) within 30 units moves it for the
// destination's fee.
func (s *Session) handleGatekeeper(p []byte) {
	if len(p) < 7 || s.char == nil {
		return
	}
	ch := s.char
	npcID, _ := packet.U32(p[2:])
	gate := p[6]
	fail := func(why string) {
		s.log.Printf("Rapture World gatekeeper refused: %s", why)
		s.send(buildGatekeeperResult(false, ch.Money), "0x020C gatekeeper (refused)")
	}
	npc, ok := npcOnMap(ch.MapID, npcID)
	if gate > 2 || !ok || npc.Type != npcTypeGatekeeper || DistanceXZ(npc.PosX, npc.PosZ, ch.PosX, ch.PosZ) >= gatekeeperReach {
		fail("no gatekeeper in reach")
		return
	}
	def := findNpcDef(npc.Type, npc.TypeID)
	if def == nil || (def.Country != 2 && def.Country != s.faction) {
		fail("wrong faction")
		return
	}
	g := def.Gates[gate]
	if g.MapID == 0 && g.X == 0 && g.Z == 0 {
		fail("empty destination")
		return
	}
	if ch.Money < g.Money || !hasGrid(g.MapID) {
		fail("no gold or unknown map")
		return
	}
	ch.Money -= g.Money
	s.persist()
	s.send(buildGatekeeperResult(true, ch.Money), "0x020C gatekeeper")
	s.log.Printf("Rapture World gatekeeper to %q map %d for %d", g.Name, g.MapID, g.Money)
	s.applyTeleport(g.MapID, g.X, g.Y, g.Z, "gatekeeper")
}

// NPC usage (CUser::PacketNpc 0x00467510): 0x0E04 records the NPC a
// player is talking to; an NPC in use stops patrolling.
var (
	npcUseMu sync.Mutex
	npcUsers = map[uint32]int{}
)

func (s *Session) useNpc(id uint32) {
	if s.char != nil && id != 0 {
		if _, ok := npcOnMap(s.char.MapID, id); !ok {
			return
		}
	}
	if id == s.usingNpc {
		return
	}
	npcUseMu.Lock()
	if s.usingNpc != 0 {
		if npcUsers[s.usingNpc]--; npcUsers[s.usingNpc] <= 0 {
			delete(npcUsers, s.usingNpc)
		}
	}
	if id != 0 {
		npcUsers[id]++
	}
	npcUseMu.Unlock()
	s.usingNpc = id
}

func npcInUse(id uint32) bool {
	npcUseMu.Lock()
	defer npcUseMu.Unlock()
	return npcUsers[id] > 0
}
