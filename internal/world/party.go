package world

import (
	"strings"
	"sync"
	"time"

	"github.com/Spelunkern/Rapture/internal/packet"
)

// Normal parties as the original server runs them (party_runtime*,
// recovered by Eternity), with the 5.4 client's packet widths:
//
//	0x0B02 invite by id / 0x0B09 by name, 0x0B03 answer (0 accepts) and
//	error replies, 0x0B01 roster, 0x0B04 member joined, 0x0B05 left,
//	0x0B06 expelled, 0x0B07 new leader, 0x0520 party badge, 0x0C01/0x0C03
//	member HP and pools, 0x0C09 member level, 0x1105 party chat.
//
// One mutex guards every party; members are ConnectedPlayers, so any
// session can address them.

const (
	partyMax          = 7
	partyInviteWindow = 30 * time.Second
	partyRewardRange  = 100

	partyErrDeclined = 0
	partyErrBusy     = 1
	partyErrNotLead  = 2
	partyErrInParty  = 3
	partyErrNoTarget = 4
	partyErrSame     = 5
)

// partyExpFactor is MobDeathExperience's party table by member count.
var partyExpFactor = [8]float64{1, 1, 1, 0.73, 0.645, 0.56, 0.475, 0.39}

type party struct {
	id       uint32
	members  []*ConnectedPlayer
	leader   int
	maxLevel uint16

	// Raid state (kind 1), see raid.go.
	kind         uint8
	subLeader    int
	slots        []uint16 // wire slot per member
	autoJoin     bool
	itemDivision int32
}

var (
	partyMu     sync.Mutex
	nextPartyID uint32
)

// partyOf returns the player's party (under partyMu).
func partyOf(p *ConnectedPlayer) *party { return p.party }

func (pt *party) leaderPlayer() *ConnectedPlayer {
	if pt.leader < len(pt.members) {
		return pt.members[pt.leader]
	}
	return nil
}

// refreshLevel is PartyRuntime::check_experience_level.
func (pt *party) refreshLevel() {
	if len(pt.members) <= 1 {
		return
	}
	pt.maxLevel = 0
	for _, m := range pt.members {
		pt.maxLevel = max(pt.maxLevel, m.Snapshot().Level)
	}
}

func (pt *party) send(frame []byte, label string) {
	for _, m := range pt.members {
		m.Send(frame, label)
	}
}

// writePartyMember is the roster/join record: id, name, level, job,
// HP/max, MP/max, SP/max (u32 in the 5.4 client), instance, position and
// active effects (none sent).
func writePartyMember(w *packet.Writer, p *ConnectedPlayer) {
	ch := p.Snapshot()
	p.mu.Lock()
	st := StatsFor(&ch, GetEquipmentStats(p.items))
	p.mu.Unlock()
	w.U32(ch.ID)
	w.FixedString(ch.Name, 21)
	w.U16(ch.Level)
	w.U8(ch.Class)
	for _, v := range []uint32{st.CurrentHP, st.MaxHP, st.CurrentMP, st.MaxMP, st.CurrentSP, st.MaxSP} {
		w.U32(v)
	}
	w.U16(0) // instance
	w.F32(ch.PosX)
	w.F32(ch.PosY)
	w.F32(ch.PosZ)
	w.U8(0) // effects
}

func partyIDPacket(op packet.Opcode, id uint32) []byte {
	w := packet.New(op)
	w.U32(id)
	return w.MustFinish()
}

func partyError(to *ConnectedPlayer, code uint8, id uint32) {
	w := packet.New(packet.PartyAnswer)
	w.U8(code)
	w.U32(id)
	to.Send(w.MustFinish(), "0x0B03 party error")
}

// partyBadge (0x0520) tells viewers whether the character is in a party
// (1) or leads one (2).
func partyBadge(p *ConnectedPlayer) {
	kind := uint8(0)
	if pt := partyOf(p); pt != nil {
		kind = 1
		if pt.leaderPlayer() == p {
			kind = 2
		}
	}
	w := packet.New(packet.PartyBadge)
	w.U32(p.CharacterID)
	w.U8(kind)
	frame := w.MustFinish()
	x, z := p.PositionXZ()
	sendNear(p.MapID(), x, z, ViewportRadius, frame, "0x0520 party badge")
}

// enterParty is enter_normal_party (partyMu held): announce the newcomer,
// send it the roster of the members already there.
func enterParty(pt *party, p *ConnectedPlayer) bool {
	if pt.isRaid() {
		return enterRaid(pt, p)
	}
	if len(pt.members) >= partyMax {
		return false
	}
	if len(pt.members) > 0 {
		w := packet.New(packet.PartyJoined)
		writePartyMember(w, p)
		pt.send(w.MustFinish(), "0x0B04 party member joined")
	}
	w := packet.New(packet.PartyRoster)
	w.U8(uint8(pt.leader))
	w.U8(uint8(len(pt.members)))
	for _, m := range pt.members {
		writePartyMember(w, m)
	}
	p.Send(w.MustFinish(), "0x0B01 party roster")
	pt.members = append(pt.members, p)
	p.party, p.inviterID = pt, 0
	pt.refreshLevel()
	partyBadge(p)
	return true
}

// leaveParty is leave_normal_party (partyMu held). expelled sends 0x0B06
// instead of 0x0B05. A party left with one member dissolves.
func leaveParty(pt *party, p *ConnectedPlayer, expelled bool) {
	if pt.isRaid() {
		removeRaid(pt, p, expelled)
		return
	}
	index := -1
	for i, m := range pt.members {
		if m == p {
			index = i
		}
	}
	if index < 0 {
		return
	}
	op, label := packet.PartyLeft, "0x0B05 party member left"
	if expelled {
		op, label = packet.PartyExpelled, "0x0B06 party member expelled"
	}
	pt.send(partyIDPacket(op, p.CharacterID), label)
	p.party = nil
	pt.members = append(pt.members[:index], pt.members[index+1:]...)
	leaderChanged := false
	switch {
	case len(pt.members) == 1:
		leaveParty(pt, pt.members[0], false)
	case len(pt.members) > 1 && index == pt.leader:
		pt.leader, leaderChanged = 0, true
	case index < pt.leader:
		pt.leader--
	}
	pt.refreshLevel()
	partyBadge(p)
	if leaderChanged {
		l := pt.leaderPlayer()
		pt.send(partyIDPacket(packet.PartyLeader, l.CharacterID), "0x0B07 party leader")
		partyBadge(l)
	}
}

// partyLeaveOnDisconnect drops a leaving player from its party and from
// any pending invitation.
func partyLeaveOnDisconnect(p *ConnectedPlayer) {
	partyMu.Lock()
	defer partyMu.Unlock()
	if pt := partyOf(p); pt != nil {
		leaveParty(pt, p, false)
	}
	p.inviterID = 0
}

// handlePartyInvite (0x0B02 id / 0x0B09 name).
func (s *Session) handlePartyInvite(op packet.Opcode, p []byte) {
	me := s.player
	if me == nil {
		return
	}
	var target *ConnectedPlayer
	var targetID uint32
	if op == packet.PartyInvite {
		if len(p) < 6 {
			return
		}
		targetID, _ = packet.U32(p[2:])
		if targetID == me.CharacterID {
			partyError(me, partyErrNoTarget, targetID)
			return
		}
		if t := FindPlayerByID(targetID); t != nil && t.MapID() == me.MapID() {
			target = t
		}
	} else {
		if len(p) < 23 {
			return
		}
		if t := FindPlayerByName(strings.TrimRight(packet.FixedASCII(p[2:23]), " ")); t != nil && t != me && t.MapID() == me.MapID() {
			target, targetID = t, t.CharacterID
		}
	}
	partyMu.Lock()
	defer partyMu.Unlock()
	if pt := partyOf(me); pt != nil && (pt.leaderPlayer() != me || len(pt.members) >= partyMax) {
		s.log.Printf("Rapture World party invite refused: not the leader or party full")
		partyError(me, partyErrNotLead, targetID)
		return
	}
	if target == nil || target.faction != me.faction {
		if t := FindPlayerByID(targetID); t != nil {
			s.log.Printf("Rapture World party invite refused: target %d map %d faction %d vs map %d faction %d", targetID, t.MapID(), t.faction, me.MapID(), me.faction)
		} else {
			s.log.Printf("Rapture World party invite refused: target %d not online", targetID)
		}
		partyError(me, partyErrNoTarget, targetID)
		return
	}
	if partyOf(target) != nil || (target.inviterID != 0 && time.Now().Before(target.inviteDeadline)) {
		code := uint8(partyErrInParty)
		if partyOf(target) != nil && partyOf(target) == partyOf(me) {
			code = partyErrSame
		}
		s.log.Printf("Rapture World party invite refused: target already in a party or invited")
		partyError(me, code, targetID)
		return
	}
	s.log.Printf("Rapture World party invite %d -> %d", me.CharacterID, targetID)
	target.inviterID, target.inviteDeadline = me.CharacterID, time.Now().Add(partyInviteWindow)
	w := packet.New(op)
	if op == packet.PartyInvite {
		w.U32(me.CharacterID)
	} else {
		w.FixedString(me.CharacterName, 21)
	}
	target.Send(w.MustFinish(), "party invite")
}

// handlePartyAnswer (0x0B03): 0 accepts. The inviter's party is created
// on the first acceptance.
func (s *Session) handlePartyAnswer(p []byte) {
	me := s.player
	if me == nil || len(p) < 3 {
		return
	}
	partyMu.Lock()
	defer partyMu.Unlock()
	inviterID := me.inviterID
	me.inviterID = 0
	if inviterID == 0 || partyOf(me) != nil {
		return
	}
	inviter := FindPlayerByID(inviterID)
	if inviter == nil || inviter.MapID() != me.MapID() {
		return
	}
	if p[2] != 0 {
		partyError(inviter, partyErrDeclined, me.CharacterID)
		return
	}
	pt := partyOf(inviter)
	if pt == nil {
		nextPartyID++
		pt = &party{id: nextPartyID, itemDivision: 1}
		enterParty(pt, inviter)
	} else if pt.leaderPlayer() != inviter {
		partyError(inviter, partyErrBusy, me.CharacterID)
		return
	}
	if !enterParty(pt, me) {
		partyError(inviter, partyErrBusy, me.CharacterID)
	}
}

func (s *Session) handlePartyLeave() {
	if s.player == nil {
		return
	}
	partyMu.Lock()
	defer partyMu.Unlock()
	if pt := partyOf(s.player); pt != nil {
		leaveParty(pt, s.player, false)
	}
}

// handlePartyExpel (0x0B06) and handlePartyLeader (0x0B07): leader only.
func (s *Session) handlePartyExpel(p []byte) {
	if s.player == nil || len(p) < 6 {
		return
	}
	id, _ := packet.U32(p[2:])
	partyMu.Lock()
	defer partyMu.Unlock()
	pt := partyOf(s.player)
	if pt == nil || pt.leaderPlayer() != s.player {
		return
	}
	for _, m := range pt.members {
		if m.CharacterID == id && m != s.player {
			leaveParty(pt, m, true)
			return
		}
	}
}

func (s *Session) handlePartyLeader(p []byte) {
	if s.player == nil || len(p) < 6 {
		return
	}
	id, _ := packet.U32(p[2:])
	partyMu.Lock()
	defer partyMu.Unlock()
	pt := partyOf(s.player)
	if pt != nil && pt.leaderPlayer() == s.player {
		for i, m := range pt.members {
			if m.CharacterID == id && i != pt.leader {
				old := pt.leaderPlayer()
				pt.leader = i
				pt.send(partyIDPacket(packet.PartyLeader, id), "0x0B07 party leader")
				partyBadge(m)
				partyBadge(old)
				return
			}
		}
	}
	s.player.Send(partyIDPacket(packet.PartyLeader, 0), "0x0B07 party leader refused")
}

// partyPools (0x0C03) shares the character's HP/SP/MP with its party.
func (s *Session) partyPools() {
	if s.player == nil || s.char == nil {
		return
	}
	partyMu.Lock()
	pt := partyOf(s.player)
	var members []*ConnectedPlayer
	op := packet.PartyPools
	if pt != nil {
		members = append(members, pt.members...)
		if pt.isRaid() {
			op = packet.RaidPools
		}
	}
	partyMu.Unlock()
	if len(members) == 0 {
		return
	}
	st := s.stats()
	w := packet.New(op)
	w.U32(s.char.ID)
	w.U32(st.CurrentHP)
	w.U32(st.CurrentSP)
	w.U32(st.CurrentMP)
	broadcast(members, w.MustFinish(), "0x0C03 party member pools", s.logger)
}

// partyLevel (0x0C09) tells the party about a level-up.
func (s *Session) partyLevel() {
	partyMu.Lock()
	defer partyMu.Unlock()
	if pt := partyOf(s.player); pt != nil && !pt.isRaid() {
		w := packet.New(packet.PartyLevel)
		w.U32(s.char.ID)
		w.U16(s.char.Level)
		pt.send(w.MustFinish(), "0x0C09 party member level")
		pt.refreshLevel()
	}
}

// partyChat (0x1105) relays a message to the party.
func (s *Session) partyChat(message string) {
	partyMu.Lock()
	pt := partyOf(s.player)
	var members []*ConnectedPlayer
	if pt != nil {
		members = append(members, pt.members...)
	}
	partyMu.Unlock()
	if len(members) == 0 {
		return
	}
	broadcast(members, buildChatByID(packet.ChatParty, s.char.ID, message), "0x1105 party chat", s.logger)
}

// partyExpLevelShare is the level gap factor against the party's highest
// level.
func partyExpLevelShare(maxLevel, level uint16) float64 {
	switch d := int32(maxLevel) - int32(level); {
	case d < 8:
		return 1
	case d < 16:
		return 0.8
	case d < 32:
		return 0.6
	case d < 48:
		return 0.4
	}
	return 0.2
}

// killRewards splits a kill's exp per mob_death: solo kills get 1.2x;
// party members within 100 units on the map get the count factor and the
// level share. Returns each recipient and amount.
func killRewards(killer *ConnectedPlayer, mapID uint16, x, z float32, mobLevel, mobExp uint16) map[*ConnectedPlayer]uint32 {
	out := map[*ConnectedPlayer]uint32{}
	partyMu.Lock()
	pt := partyOf(killer)
	var members []*ConnectedPlayer
	var maxLevel uint16
	raid := false
	if pt != nil {
		members, maxLevel, raid = append(members, pt.members...), pt.maxLevel, pt.isRaid()
	}
	partyMu.Unlock()
	if len(members) == 0 {
		lvl := killer.Snapshot().Level
		out[killer] = uint32(float64(mobExperience(mobLevel, mobExp, lvl)) * 1.2)
		return out
	}
	factor := partyExpFactor[min(len(members), len(partyExpFactor)-1)]
	if raid {
		factor = raidExpFactor[min(len(members), len(raidExpFactor)-1)]
	}
	for _, m := range members {
		mx, mz := m.PositionXZ()
		if m.MapID() != mapID || !m.IsAlive() || DistanceXZ(mx, mz, x, z) > partyRewardRange {
			continue
		}
		lvl := m.Snapshot().Level
		base := float64(mobExperience(mobLevel, mobExp, lvl)) * factor
		out[m] = uint32(base * partyExpLevelShare(maxLevel, lvl))
	}
	return out
}

// partyMembersNear lists the killer's party members on the map within
// the reward range (nil without a party).
func partyMembersNear(killer *ConnectedPlayer, mapID uint16, x, z float32) []*ConnectedPlayer {
	partyMu.Lock()
	pt := partyOf(killer)
	var members []*ConnectedPlayer
	if pt != nil {
		members = append(members, pt.members...)
	}
	partyMu.Unlock()
	var out []*ConnectedPlayer
	for _, m := range members {
		mx, mz := m.PositionXZ()
		if m.MapID() == mapID && DistanceXZ(mx, mz, x, z) <= partyRewardRange {
			out = append(out, m)
		}
	}
	return out
}

// samePartyIDs reports whether two characters share a party.
func samePartyIDs(a, b uint32) bool {
	pa, pb := FindPlayerByID(a), FindPlayerByID(b)
	if pa == nil || pb == nil {
		return false
	}
	partyMu.Lock()
	defer partyMu.Unlock()
	return partyOf(pa) != nil && partyOf(pa) == partyOf(pb)
}
