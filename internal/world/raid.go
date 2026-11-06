package world

import (
	"strings"
	"time"

	"github.com/Spelunkern/Rapture/internal/packet"
)

// Raids are parties converted with 0x0B0E (party_runtime_raid*, recovered
// by Eternity): up to 30 members in numbered wire slots, a leader and a
// sub-leader, an item-division mode and an auto-join flag. Packets:
//
//	0x0B0E convert, 0x0B0F item division, 0x0B10 auto-join, 0x0B16
//	disband, 0x0B14/0x0B18 invite (id/name), 0x0B15 answer, 0x0B19 error,
//	0x0B11 auto-join by name, 0x0B12 swap slots, 0x0B13 sub-leader,
//	0x0B17 leader, 0x0B1A expel, 0x0B0D leave, 0x0B0B roster (in chunks
//	of six), 0x0B0C member joined, 0x0C0C member pools.

const (
	raidMax           = 30
	raidKindNormal    = 0
	raidKindRaid      = 1
	raidRosterChunk   = 6
	raidErrNoRaid     = 12
	raidErrNoAutoJoin = 13
	raidErrFull       = 14
)

// raidExpFactor is MobDeathExperience's raid table by member count.
var raidExpFactor = [31]float64{1, 1, 0.73, 0.645, 0.56, 0.475, 0.39, 0.305, 0.27, 0.251, 0.235, 0.219, 0.21,
	0.17, 0.14, 0.1, 0.06, 0.03, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01, 0.01}

func (pt *party) isRaid() bool { return pt.kind == raidKindRaid }

func (pt *party) subLeaderPlayer() *ConnectedPlayer {
	if pt.isRaid() && pt.subLeader < len(pt.members) {
		return pt.members[pt.subLeader]
	}
	return nil
}

// emptyRaidSlot is the lowest wire slot not in use.
func (pt *party) emptyRaidSlot() int {
	for c := 0; c < raidMax; c++ {
		used := false
		for _, s := range pt.slots {
			used = used || int(s) == c
		}
		if !used {
			return c
		}
	}
	return -1
}

func raidError(to *ConnectedPlayer, reason, id uint32) {
	w := packet.New(packet.RaidError)
	w.U32(reason)
	w.U32(id)
	to.Send(w.MustFinish(), "0x0B19 raid error")
}

// enterRaid is enter_raid_party (partyMu held).
func enterRaid(pt *party, p *ConnectedPlayer) bool {
	if len(pt.members) >= raidMax {
		return false
	}
	slot := pt.emptyRaidSlot()
	if slot < 0 {
		return false
	}
	pt.members = append(pt.members, p)
	pt.slots = append(pt.slots, uint16(slot))
	p.party, p.inviterID = pt, 0
	if len(pt.members) > 1 {
		w := packet.New(packet.RaidJoined)
		w.U16(uint16(slot))
		writePartyMember(w, p)
		pt.send(w.MustFinish(), "0x0B0C raid member joined")
	}
	pt.sendRaidRoster(p)
	pt.refreshLevel()
	partyBadge(p)
	return true
}

// sendRaidRoster sends the roster in 0x0B0B chunks of six; the first byte
// flags whether more chunks follow.
func (pt *party) sendRaidRoster(to *ConnectedPlayer) {
	for start := 0; start < len(pt.members); start += raidRosterChunk {
		end := min(start+raidRosterChunk, len(pt.members))
		w := packet.New(packet.RaidRoster)
		w.Bool(end < len(pt.members))
		w.U8(uint8(pt.slots[pt.leader]))
		w.U8(uint8(pt.slots[pt.subLeader]))
		w.U16(uint16(pt.itemDivision))
		w.U8(pt.kind)
		w.Bool(pt.autoJoin)
		w.U8(uint8(end - start))
		for i := start; i < end; i++ {
			w.U16(pt.slots[i])
			writePartyMember(w, pt.members[i])
		}
		to.Send(w.MustFinish(), "0x0B0B raid roster")
	}
}

// removeRaid is remove_raid_party (partyMu held): 0x0B0D or 0x0B1A, the
// sub-leader takes over a departed leader, a raid of one dissolves.
func removeRaid(pt *party, p *ConnectedPlayer, expelled bool) {
	index := -1
	for i, m := range pt.members {
		if m == p {
			index = i
		}
	}
	if index < 0 {
		return
	}
	leaderChanged := index == pt.leader
	subChanged := !leaderChanged && index == pt.subLeader
	op, label := packet.PartyLeaveAlt, "0x0B0D raid member left"
	if expelled {
		op, label = packet.RaidExpel, "0x0B1A raid member expelled"
	}
	pt.send(partyIDPacket(op, p.CharacterID), label)
	p.party = nil
	pt.members = append(pt.members[:index], pt.members[index+1:]...)
	pt.slots = append(pt.slots[:index], pt.slots[index+1:]...)
	if index < pt.leader {
		pt.leader--
	}
	if index < pt.subLeader {
		pt.subLeader--
	}
	switch {
	case len(pt.members) == 1:
		removeRaid(pt, pt.members[0], false)
		partyBadge(p)
		return
	case len(pt.members) > 1 && (leaderChanged || subChanged):
		if leaderChanged {
			pt.leader = pt.subLeader
		}
		for i := range pt.members {
			if i != pt.leader {
				pt.subLeader, subChanged = i, true
				break
			}
		}
	}
	pt.refreshLevel()
	partyBadge(p)
	if leaderChanged && len(pt.members) > 0 {
		l := pt.leaderPlayer()
		pt.send(partyIDPacket(packet.RaidLeader, l.CharacterID), "0x0B17 raid leader")
		partyBadge(l)
	}
	if subChanged && len(pt.members) > 1 {
		pt.send(partyIDPacket(packet.RaidSubLeader, pt.members[pt.subLeader].CharacterID), "0x0B13 raid sub-leader")
	}
}

// handleRaidSettings covers 0x0B0E (convert), 0x0B0F (item division),
// 0x0B10 (auto-join) and 0x0B16 (disband), all leader-only.
func (s *Session) handleRaidSettings(op packet.Opcode, p []byte) {
	me := s.player
	if me == nil {
		return
	}
	partyMu.Lock()
	defer partyMu.Unlock()
	pt := partyOf(me)
	if pt == nil || pt.leaderPlayer() != me {
		return
	}
	switch op {
	case packet.RaidConvert:
		if len(p) < 8 || p[2] == 0 {
			return
		}
		pt.kind = raidKindRaid
		pt.slots = pt.slots[:0]
		for i := range pt.members {
			pt.slots = append(pt.slots, uint16(i))
		}
		for i := range pt.members {
			if i != pt.leader {
				pt.subLeader = i
				break
			}
		}
		pt.kind, pt.autoJoin = p[2], p[3] != 0
		div, _ := packet.U32(p[4:])
		pt.itemDivision = int32(div)
		w := packet.New(op)
		w.U8(pt.kind)
		w.Bool(pt.autoJoin)
		w.U32(div)
		w.U32(uint32(pt.slots[pt.leader]))
		w.U32(uint32(pt.slots[pt.subLeader]))
		pt.send(w.MustFinish(), "0x0B0E raid converted")
		return
	}
	if pt.kind == raidKindNormal {
		return
	}
	w := packet.New(op)
	switch op {
	case packet.RaidDivision:
		if len(p) < 6 {
			return
		}
		div, _ := packet.U32(p[2:])
		pt.itemDivision = int32(div)
		w.U32(div)
		pt.send(w.MustFinish(), "0x0B0F raid item division")
	case packet.RaidAutoJoin:
		if len(p) < 3 {
			return
		}
		pt.autoJoin = p[2] != 0
		w.Bool(pt.autoJoin)
		pt.send(w.MustFinish(), "0x0B10 raid auto-join")
	case packet.RaidDisband:
		pt.send(w.MustFinish(), "0x0B16 raid disbanded")
		for len(pt.members) > 0 {
			removeRaid(pt, pt.members[len(pt.members)-1], false)
		}
	}
}

func (pt *party) mayInvite(p *ConnectedPlayer) bool {
	return len(pt.members) < raidMax && (pt.leaderPlayer() == p || pt.subLeaderPlayer() == p)
}

// handleRaidInvite (0x0B14 id / 0x0B18 name): the leader or sub-leader
// invites; outside a raid it starts a new party as raid-capable.
func (s *Session) handleRaidInvite(op packet.Opcode, p []byte) {
	me := s.player
	if me == nil {
		return
	}
	var target *ConnectedPlayer
	var id uint32
	if op == packet.RaidInvite {
		if len(p) < 6 {
			return
		}
		id, _ = packet.U32(p[2:])
		if id == me.CharacterID {
			raidError(me, partyErrNoTarget, id)
			return
		}
		if t := FindPlayerByID(id); t != nil && t.MapID() == me.MapID() {
			target = t
		}
	} else {
		if len(p) < 23 {
			return
		}
		if t := FindPlayerByName(strings.TrimRight(packet.FixedASCII(p[2:23]), " ")); t != nil && t != me && t.MapID() == me.MapID() {
			target, id = t, t.CharacterID
		}
	}
	partyMu.Lock()
	defer partyMu.Unlock()
	if pt := partyOf(me); pt != nil && !pt.mayInvite(me) {
		raidError(me, partyErrNotLead, id)
		return
	}
	if target == nil || target.faction != me.faction {
		raidError(me, partyErrNoTarget, id)
		return
	}
	if partyOf(target) != nil || (target.inviterID != 0 && time.Now().Before(target.inviteDeadline)) {
		code := uint32(partyErrInParty)
		if partyOf(target) != nil && partyOf(target) == partyOf(me) {
			code = partyErrSame
		}
		raidError(me, code, id)
		return
	}
	target.inviterID, target.inviteDeadline = me.CharacterID, time.Now().Add(partyInviteWindow)
	w := packet.New(op)
	if op == packet.RaidInvite {
		w.U32(me.CharacterID)
	} else {
		w.FixedString(me.CharacterName, 21)
	}
	target.Send(w.MustFinish(), "raid invite")
}

// handleRaidAnswer (0x0B15): 0 accepts; joins the inviter's raid (or
// creates its party).
func (s *Session) handleRaidAnswer(p []byte) {
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
		raidError(inviter, partyErrDeclined, me.CharacterID)
		return
	}
	pt := partyOf(inviter)
	switch {
	case pt == nil:
		nextPartyID++
		pt = &party{id: nextPartyID, itemDivision: 1}
		enterParty(pt, inviter)
	case !pt.mayInvite(inviter):
		raidError(inviter, partyErrBusy, me.CharacterID)
		return
	}
	if !enterParty(pt, me) {
		raidError(inviter, partyErrBusy, me.CharacterID)
	}
}

// handleRaidAutoJoin (0x0B11): join the raid of the named member when it
// allows auto-join.
func (s *Session) handleRaidAutoJoin(p []byte) {
	me := s.player
	if me == nil || len(p) < 23 {
		return
	}
	partyMu.Lock()
	defer partyMu.Unlock()
	if partyOf(me) != nil {
		return
	}
	found := FindPlayerByName(strings.TrimRight(packet.FixedASCII(p[2:23]), " "))
	if found == nil || partyOf(found) == nil || !partyOf(found).isRaid() || found.faction != me.faction {
		raidError(me, raidErrNoRaid, 0)
		return
	}
	pt := partyOf(found)
	switch {
	case !pt.autoJoin:
		raidError(me, raidErrNoAutoJoin, 0)
	case len(pt.members) >= raidMax:
		raidError(me, raidErrFull, 0)
	default:
		enterRaid(pt, me)
	}
}

// handleRaidRoles covers 0x0B12 (swap wire slots; leader or sub-leader),
// 0x0B13 (sub-leader) and 0x0B17 (leader).
func (s *Session) handleRaidRoles(op packet.Opcode, p []byte) {
	me := s.player
	if me == nil || len(p) < 6 {
		return
	}
	partyMu.Lock()
	defer partyMu.Unlock()
	pt := partyOf(me)
	if pt == nil || !pt.isRaid() {
		return
	}
	leader := pt.leaderPlayer() == me
	if !leader && (op != packet.RaidSwap || pt.subLeaderPlayer() != me) {
		return
	}
	id, _ := packet.U32(p[2:])
	switch op {
	case packet.RaidSubLeader:
		for i, m := range pt.members {
			if m.CharacterID == id && i != pt.leader {
				pt.subLeader = i
			}
		}
		pt.send(partyIDPacket(packet.RaidSubLeader, pt.members[pt.subLeader].CharacterID), "0x0B13 raid sub-leader")
	case packet.RaidLeader:
		for i, m := range pt.members {
			if m.CharacterID != id || i == pt.leader {
				continue
			}
			old := pt.leaderPlayer()
			pt.leader = i
			pt.send(partyIDPacket(packet.RaidLeader, id), "0x0B17 raid leader")
			if i == pt.subLeader {
				for j := range pt.members {
					if j != pt.leader {
						pt.subLeader = j
						break
					}
				}
				pt.send(partyIDPacket(packet.RaidSubLeader, pt.members[pt.subLeader].CharacterID), "0x0B13 raid sub-leader")
			}
			partyBadge(m)
			partyBadge(old)
			return
		}
	case packet.RaidSwap:
		if len(p) < 10 {
			return
		}
		first, second := uint16(id), uint16(0)
		v, _ := packet.U32(p[6:])
		second = uint16(v)
		for i, sl := range pt.slots {
			switch sl {
			case first:
				pt.slots[i] = second
			case second:
				pt.slots[i] = first
			}
		}
		w := packet.New(op)
		w.U32(uint32(first))
		w.U32(uint32(second))
		w.U32(uint32(pt.slots[pt.leader]))
		w.U32(uint32(pt.slots[pt.subLeader]))
		pt.send(w.MustFinish(), "0x0B12 raid slots swapped")
	}
}

// handleRaidExpel (0x0B1A): leader only.
func (s *Session) handleRaidExpel(p []byte) {
	me := s.player
	if me == nil || len(p) < 6 {
		return
	}
	id, _ := packet.U32(p[2:])
	partyMu.Lock()
	defer partyMu.Unlock()
	pt := partyOf(me)
	if pt == nil || !pt.isRaid() || pt.leaderPlayer() != me {
		return
	}
	for _, m := range pt.members {
		if m.CharacterID == id && m != me {
			removeRaid(pt, m, true)
			return
		}
	}
}
