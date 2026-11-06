package world

import (
	"sync"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// Player-to-player trade (0x0A01-0x0A0A). The session that completes the
// trade moves the items in the database; the partner's session reloads
// through its trade-refresh mailbox.

const tradeSlots = 8

type offeredItem struct {
	Item  db.Item
	Count uint8
}

type tradeOffer struct {
	items     [tradeSlots]*offeredItem
	money     uint32
	ready     bool
	confirmed bool
}

type tradeSession struct {
	mu     sync.Mutex
	a, b   uint32
	offerA tradeOffer
	offerB tradeOffer
}

func (t *tradeSession) offer(id uint32) *tradeOffer {
	switch id {
	case t.a:
		return &t.offerA
	case t.b:
		return &t.offerB
	}
	return nil
}

func (t *tradeSession) partner(id uint32) uint32 {
	if id == t.a {
		return t.b
	}
	return t.a
}

func (t *tradeSession) unready() {
	t.offerA.ready, t.offerA.confirmed, t.offerB.ready, t.offerB.confirmed = false, false, false, false
}

var (
	tradeMu       sync.Mutex
	tradeRequests = map[uint32]uint32{} // requester -> target
	tradeSessions = map[uint32]*tradeSession{}
)

func tradeFor(id uint32) *tradeSession {
	tradeMu.Lock()
	defer tradeMu.Unlock()
	return tradeSessions[id]
}

func endTrade(a, b uint32) {
	tradeMu.Lock()
	delete(tradeSessions, a)
	delete(tradeSessions, b)
	tradeMu.Unlock()
}

func tradePacket(op packet.Opcode, b ...byte) []byte {
	w := packet.New(op)
	w.Bytes(b)
	return w.MustFinish()
}

func buildTradeReady(kind uint8, canceled bool) []byte {
	return tradePacket(packet.ExchangeReady, kind, boolByte(canceled))
}

func buildTradeConfirm(kind uint8, confirmed bool) []byte {
	return tradePacket(packet.ExchangeConfirm, kind, boolByte(confirmed))
}

func boolByte(v bool) byte {
	if v {
		return 1
	}
	return 0
}

func sendTradeCancel(p *ConnectedPlayer) {
	p.Send(buildTradeReady(3, true), "0x0A05 exchange cancel")
	p.Send(buildTradeConfirm(1, false), "0x0A0A exchange cancel confirm (self)")
	p.Send(buildTradeConfirm(2, false), "0x0A0A exchange cancel confirm (partner)")
	p.Send(tradePacket(packet.ExchangeEnd, 1), "0x0A04 exchange end (canceled)")
}

func broadcastUnready(players ...*ConnectedPlayer) {
	for _, p := range players {
		if p == nil {
			continue
		}
		p.Send(buildTradeReady(3, true), "0x0A05 exchange offer changed")
		p.Send(buildTradeConfirm(1, false), "0x0A0A exchange unconfirm 1")
		p.Send(buildTradeConfirm(2, false), "0x0A0A exchange unconfirm 2")
	}
}

// tradeCancelForDisconnect drops pending requests and cancels any trade.
func tradeCancelForDisconnect(id uint32, log *logx.Logger) {
	tradeMu.Lock()
	delete(tradeRequests, id)
	for r, t := range tradeRequests {
		if t == id {
			delete(tradeRequests, r)
		}
	}
	t := tradeSessions[id]
	tradeMu.Unlock()
	if t == nil {
		return
	}
	partner := t.partner(id)
	endTrade(id, partner)
	if p := FindPlayerByID(partner); p != nil {
		sendTradeCancel(p)
	}
}

func buildTradeStart(partner uint32) []byte {
	w := packet.New(packet.ExchangeStart)
	w.U32(partner)
	w.U8(0)
	return w.MustFinish()
}

func startTrade(a, b *ConnectedPlayer) {
	t := &tradeSession{a: a.CharacterID, b: b.CharacterID}
	tradeMu.Lock()
	tradeSessions[a.CharacterID], tradeSessions[b.CharacterID] = t, t
	tradeMu.Unlock()
	accepted := tradePacket(packet.ExchangeAnswer, 0)
	a.Send(accepted, "0x0A02 exchange accepted")
	b.Send(accepted, "0x0A02 exchange accepted")
	a.Send(buildTradeStart(b.CharacterID), "0x0A03 exchange start")
	b.Send(buildTradeStart(a.CharacterID), "0x0A03 exchange start")
}

// handleTradeRequest (0x0A01): target character id. Two crossing requests
// start the trade directly.
func (s *Session) handleTradeRequest(target uint32) {
	me := s.player
	if me == nil || target == me.CharacterID || tradeFor(me.CharacterID) != nil {
		return
	}
	// CUser::PacketExchange 0x0A01: a user of the same zone and country
	// that is not trading.
	other := FindPlayerByID(target)
	if other == nil || other.MapID() != me.MapID() || other.Faction() != me.Faction() || tradeFor(target) != nil {
		return
	}
	tradeMu.Lock()
	reciprocal := tradeRequests[target] == me.CharacterID
	if reciprocal {
		delete(tradeRequests, target)
		delete(tradeRequests, me.CharacterID)
	} else {
		tradeRequests[me.CharacterID] = target
	}
	tradeMu.Unlock()
	if reciprocal {
		startTrade(me, other)
		return
	}
	w := packet.New(packet.ExchangeRequest)
	w.U32(me.CharacterID)
	other.Send(w.MustFinish(), "0x0A01 exchange request")
}

// handleTradeAnswer (0x0A02): the byte is "declined" (0 accepts).
func (s *Session) handleTradeAnswer(accepted bool) {
	me := s.player
	if me == nil {
		return
	}
	tradeMu.Lock()
	var requester uint32
	for r, t := range tradeRequests {
		if t == me.CharacterID {
			requester = r
		}
	}
	delete(tradeRequests, requester)
	tradeMu.Unlock()
	if requester == 0 {
		return
	}
	other := FindPlayerByID(requester)
	if other == nil {
		return
	}
	if !accepted {
		other.Send(tradePacket(packet.ExchangeAnswer, 1), "0x0A02 exchange rejected")
		return
	}
	startTrade(other, me)
}

func writeOffered(w *packet.Writer, o *offeredItem) {
	if o == nil {
		w.Bytes(make([]byte, 3))
		w.U16(0)
		w.Bytes(make([]byte, 6))
		w.FixedString("", 21)
		return
	}
	w.U8(o.Item.Type)
	w.U8(o.Item.TypeID)
	w.U8(o.Count)
	w.U16(o.Item.Quality)
	w.Bytes(o.Item.Gems[:])
	w.FixedString(o.Item.CraftName, 21)
}

func buildTradeAddDest(slot uint8, o *offeredItem) []byte {
	w := packet.New(packet.ExchangeAddDest)
	w.U8(slot)
	writeOffered(w, o)
	return w.MustFinish()
}

// mutateOffer applies fn to this player's offer, unreadies both sides and
// returns the partner (nil if offline).
func (s *Session) mutateOffer(fn func(*tradeOffer)) (*tradeSession, *ConnectedPlayer, bool) {
	me := s.player
	if me == nil {
		return nil, nil, false
	}
	t := tradeFor(me.CharacterID)
	if t == nil {
		return nil, nil, false
	}
	t.mu.Lock()
	t.unready()
	fn(t.offer(me.CharacterID))
	partnerID := t.partner(me.CharacterID)
	t.mu.Unlock()
	partner := FindPlayerByID(partnerID)
	broadcastUnready(me, partner)
	return t, partner, true
}

// handleTradeAdd (0x0A06): bag, slot, count, trade slot.
func (s *Session) handleTradeAdd(bag, slot, count, dest uint8) {
	if dest >= tradeSlots || bag == 0 || !isValidItemPosition(bag, slot) {
		return
	}
	i := findItem(s.items, bag, slot)
	if i < 0 {
		return
	}
	// 0x0A06: count 1..stack, not a quest item (real type 27), not offered
	// twice.
	it := s.items[i]
	if count == 0 || count > it.Count || realType(it.Type) == 27 {
		return
	}
	if t := tradeFor(s.char.ID); t != nil {
		if off := t.offer(s.char.ID); off != nil {
			for _, x := range off.items {
				if x != nil && x.Item.UID == it.UID {
					return
				}
			}
		}
	}
	n := count
	o := &offeredItem{Item: it, Count: n}
	_, partner, ok := s.mutateOffer(func(off *tradeOffer) { off.items[dest] = o })
	if !ok {
		return
	}
	s.send(tradePacket(packet.ExchangeAdd, bag, slot, n, dest), "0x0A06 exchange add (self)")
	if partner != nil {
		partner.Send(buildTradeAddDest(dest, o), "0x0A09 exchange add dest")
	}
}

func (s *Session) handleTradeRemove(dest uint8) {
	if dest >= tradeSlots {
		return
	}
	_, partner, ok := s.mutateOffer(func(off *tradeOffer) { off.items[dest] = nil })
	if !ok {
		return
	}
	s.send(buildTradeAddDest(dest, nil), "0x0A09 exchange remove (self)")
	if partner != nil {
		partner.Send(buildTradeAddDest(dest, nil), "0x0A09 exchange remove (partner)")
	}
}

func buildTradeMoney(kind uint8, money uint32) []byte {
	w := packet.New(packet.ExchangeAddMoney)
	w.U8(kind)
	w.U32(money)
	return w.MustFinish()
}

func (s *Session) handleTradeMoney(requested uint32) {
	if s.char == nil {
		return
	}
	offered := min(requested, s.char.Money)
	_, partner, ok := s.mutateOffer(func(off *tradeOffer) { off.money = offered })
	if !ok {
		return
	}
	s.send(buildTradeMoney(1, offered), "0x0A08 exchange add money (self)")
	if partner != nil {
		partner.Send(buildTradeMoney(2, offered), "0x0A08 exchange add money (partner)")
	}
}

// handleTradeReady (0x0A05): 0 ready, 1 unready, 2+ cancel.
func (s *Session) handleTradeReady(v uint8) {
	me := s.player
	if me == nil {
		return
	}
	t := tradeFor(me.CharacterID)
	if t == nil {
		return
	}
	partnerID := t.partner(me.CharacterID)
	partner := FindPlayerByID(partnerID)
	if v >= 2 {
		endTrade(me.CharacterID, partnerID)
		sendTradeCancel(me)
		if partner != nil {
			sendTradeCancel(partner)
		}
		return
	}
	t.mu.Lock()
	t.offer(me.CharacterID).ready = v == 0
	done := t.offerA.ready && t.offerB.ready && t.offerA.confirmed && t.offerB.confirmed
	t.mu.Unlock()
	s.send(buildTradeReady(1, v == 1), "0x0A05 exchange ready (self)")
	if partner != nil {
		partner.Send(buildTradeReady(2, v == 1), "0x0A05 exchange ready (partner)")
	}
	if done && partner != nil {
		s.finalizeTrade(t, partner)
	}
}

// handleTradeConfirm (0x0A0A): confirmed flag.
func (s *Session) handleTradeConfirm(confirmed bool) {
	me := s.player
	if me == nil {
		return
	}
	t := tradeFor(me.CharacterID)
	if t == nil {
		return
	}
	partner := FindPlayerByID(t.partner(me.CharacterID))
	t.mu.Lock()
	t.offer(me.CharacterID).confirmed = confirmed
	done := t.offerA.ready && t.offerB.ready && t.offerA.confirmed && t.offerB.confirmed
	t.mu.Unlock()
	s.send(buildTradeConfirm(1, confirmed), "0x0A0A exchange confirm (self)")
	if partner != nil {
		partner.Send(buildTradeConfirm(2, confirmed), "0x0A0A exchange confirm (partner)")
	}
	if done && partner != nil {
		s.finalizeTrade(t, partner)
	}
}

func offerValid(items []db.Item, o *tradeOffer, money uint32) bool {
	if o.money > money {
		return false
	}
	totals := map[int64]int{}
	for _, it := range o.items {
		if it != nil {
			totals[it.Item.UID] += int(it.Count)
		}
	}
	for uid, n := range totals {
		i := findItemByUID(items, uid)
		if i < 0 || int(items[i].Count) < n {
			return false
		}
	}
	return true
}

func offerFits(receiver []db.Item, o *tradeOffer) bool {
	sim := append([]db.Item(nil), receiver...)
	for _, it := range o.items {
		if it == nil {
			continue
		}
		ms := maxStackOf(it.Item.Type, it.Item.TypeID)
		if !wouldFit(sim, &it.Item, it.Count, ms) {
			return false
		}
		// wouldFit works on its own copy; account for the placement here.
		tpl := it.Item
		tpl.UID = 0
		for left := it.Count; left > 0; {
			if i := findStackable(sim, &tpl, ms); i >= 0 {
				m := min(left, ms-sim[i].Count)
				sim[i].Count += m
				left -= m
				continue
			}
			free := freeInventorySlots(sim)
			n := min(left, ms)
			c := tpl
			c.Bag, c.Slot, c.Count = free[0][0], free[0][1], n
			sim = append(sim, c)
			left -= n
		}
	}
	return true
}

// transferOffer moves an offer from one inventory to another, returning
// the packets for each side.
func transferOffer(fromID, toID uint32, o *tradeOffer, from, to *[]db.Item) (fromPk, toPk [][]byte) {
	for _, it := range o.items {
		if it == nil {
			continue
		}
		i := findItemByUID(*from, it.Item.UID)
		if i < 0 {
			continue
		}
		grant := min(it.Count, (*from)[i].Count)
		(*from)[i].Count -= grant
		db.SaveCharacterItem(fromID, &(*from)[i])
		fromPk = append(fromPk, buildRemoveItem(&(*from)[i], (*from)[i].Count == 0))
		if (*from)[i].Count == 0 {
			*from = append((*from)[:i], (*from)[i+1:]...)
		}
		if grant == 0 {
			continue
		}
		tpl := it.Item
		tpl.UID = 0
		ms := maxStackOf(tpl.Type, tpl.TypeID)
		adds, left, _ := giveItemTo(toID, to, &tpl, grant, ms)
		toPk = append(toPk, adds...)
		if left > 0 {
			back, _, _ := giveItemTo(fromID, from, &tpl, left, ms)
			fromPk = append(fromPk, back...)
		}
	}
	return fromPk, toPk
}

func (s *Session) finalizeTrade(t *tradeSession, partner *ConnectedPlayer) {
	me := s.player
	endTrade(me.CharacterID, partner.CharacterID)
	t.mu.Lock()
	mine, theirs := *t.offer(me.CharacterID), *t.offer(partner.CharacterID)
	t.mu.Unlock()
	partnerItems, err := db.CharacterItems(partner.CharacterID)
	partnerMoney := partner.Snapshot().Money
	if err != nil || !offerValid(s.items, &mine, s.char.Money) || !offerValid(partnerItems, &theirs, partnerMoney) ||
		!offerFits(partnerItems, &mine) || !offerFits(s.items, &theirs) {
		s.log.Printf("Rapture World trade failed validation with character_id=%d", partner.CharacterID)
		sendTradeCancel(me)
		sendTradeCancel(partner)
		return
	}
	myOut, theirIn := transferOffer(me.CharacterID, partner.CharacterID, &mine, &s.items, &partnerItems)
	theirOut, myIn := transferOffer(partner.CharacterID, me.CharacterID, &theirs, &partnerItems, &s.items)
	delta := int64(theirs.money) - int64(mine.money)
	s.char.Money = uint32(min(max(int64(s.char.Money)+delta, 0), db.MoneyCap))
	s.persist()
	me.Send(tradePacket(packet.ExchangeEnd, 0), "0x0A04 exchange end (self)")
	partner.Send(tradePacket(packet.ExchangeEnd, 0), "0x0A04 exchange end (partner)")
	partner.QueueTradeRefresh(append(theirOut, theirIn...), -delta)
	for _, p := range append(myOut, myIn...) {
		s.send(p, "0x0206/0x0205 trade result item")
	}
	s.send(buildSetMoney(s.char.Money), "0x0213 trade result money")
	if s.player != nil {
		s.player.UpdateItems(s.items)
	}
	s.log.Printf("Rapture World trade completed with character_id=%d money_delta=%d", partner.CharacterID, delta)
}
