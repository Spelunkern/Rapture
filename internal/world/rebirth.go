package world

import (
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
)

// Resurrection follows ps_game.exe: the 0x0553 request (CUser::PacketPC),
// CUser::KillSoul 0x004599e0, KillSoulByItem 0x00459740,
// KillSoulByItemNoMove 0x004598a0, KillSoulParty 0x004596a0,
// UpdateRebirth 0x0048d240, FindItemRebirth 0x0045f740 and Rebirth
// 0x00459ec0.

// 0x0553 request byte.
const (
	reviveAtBind     = 2
	reviveWithParty  = 3
	reviveRuneAtBind = 4
	reviveRuneHere   = 5
)

// CUser::Rebirth kind byte of 0x0551.
const (
	rebirthKindHere  = 1
	rebirthKindBind  = 4
	rebirthKindParty = 5
)

const (
	// rebirthDeadline is EnterDead's tick + 200000: the dead user is
	// revived at the bind point when it has not chosen.
	rebirthDeadline = 200 * time.Second
	// partyRebirthWait is 0x0553 mode 3's tick + 30000.
	partyRebirthWait = 30 * time.Second
	// abilityUltimateRebirth (46, CUser +0x58d8) revives an ultimate-mode
	// character without the rune.
	abilityUltimateRebirth = 46
	// Resurrection runes: real type 26, Special 37.
	specialRebirth = 37
)

// startRebirthTimer is EnterDead's deadline (code 0).
func (s *Session) startRebirthTimer() {
	s.rebirthAt, s.rebirthParty = time.Now().Add(rebirthDeadline), false
}

// handleResurrect (0x0553): mode byte.
func (s *Session) handleResurrect(p []byte) {
	if len(p) < 3 || s.char == nil || !s.dead {
		return
	}
	switch p[2] {
	case reviveAtBind:
		s.killSoul()
	case reviveWithParty:
		if !s.inParty() {
			s.killSoul()
			return
		}
		s.rebirthAt, s.rebirthParty = time.Now().Add(partyRebirthWait), true
	case reviveRuneAtBind:
		s.killSoulByItem(false)
	case reviveRuneHere:
		s.killSoulByItem(true)
	}
}

// tickRebirth is UpdateRebirth's death timer.
func (s *Session) tickRebirth(now time.Time) {
	if !s.dead || s.rebirthAt.IsZero() || now.Before(s.rebirthAt) {
		return
	}
	if s.rebirthParty && s.inParty() {
		s.killSoulParty()
		return
	}
	s.killSoul()
}

// killSoul is CUser::KillSoul. Outside ultimate mode: the death's exp
// loss, then rebirth at the bind point with full pools. In ultimate mode
// the character needs ability 46 or a resurrection rune (no exp loss, half
// pools); without either the original ends the character.
func (s *Session) killSoul() {
	ch := s.char
	if ch.Mode != growUltimate || s.deathLoss < 0 {
		lost := expDrop(ch, s.deathLoss)
		m, x, y, z := bindPosition(ch.MapID, s.faction, ch.PosX, ch.PosY, ch.PosZ)
		s.rebirth(m, x, y, z, rebirthKindBind, true, lost)
		return
	}
	if s.hasBenefit(abilityUltimateRebirth) {
		s.halfPools()
		m, x, y, z := bindPosition(ch.MapID, s.faction, ch.PosX, ch.PosY, ch.PosZ)
		s.rebirth(m, x, y, z, rebirthKindBind, false, 0)
		return
	}
	if i := s.findRebirthItem(false); i >= 0 {
		s.useRebirthItem(i)
		s.halfPools()
		m, x, y, z := bindPosition(ch.MapID, s.faction, ch.PosX, ch.PosY, ch.PosZ)
		s.rebirth(m, x, y, z, rebirthKindBind, false, 0)
		return
	}
	s.ultimateDeath()
}

// killSoulByItem is KillSoulByItem (here false: bind point, preferring
// runes with ReqVg set) and KillSoulByItemNoMove (here true: where the
// character died, preferring runes with ReqVg 0). No exp loss, half pools;
// without a rune it is KillSoul.
func (s *Session) killSoulByItem(here bool) {
	ch := s.char
	i := -1
	if s.deathLoss >= 0 {
		i = s.findRebirthItem(here)
	}
	if i < 0 {
		s.killSoul()
		return
	}
	s.useRebirthItem(i)
	s.halfPools()
	if here {
		s.rebirth(ch.MapID, ch.PosX, ch.PosY, ch.PosZ, rebirthKindHere, false, 0)
		return
	}
	m, x, y, z := bindPosition(ch.MapID, s.faction, ch.PosX, ch.PosY, ch.PosZ)
	s.rebirth(m, x, y, z, rebirthKindBind, false, 0)
}

// killSoulParty is KillSoulParty: at the party leader's position with
// full pools (after the exp loss) when the leader is alive on the same
// map; otherwise, and always in ultimate mode, KillSoul.
func (s *Session) killSoulParty() {
	ch := s.char
	if ch.Mode == growUltimate {
		s.killSoul()
		return
	}
	l := s.partyLeader()
	if l == nil || l == s.player || !l.IsAlive() || l.MapID() != ch.MapID {
		s.killSoul()
		return
	}
	lost := expDrop(ch, s.deathLoss)
	pos := l.Snapshot()
	s.rebirth(ch.MapID, pos.PosX, pos.PosY, pos.PosZ, rebirthKindParty, true, lost)
}

func (s *Session) inParty() bool {
	if s.player == nil {
		return false
	}
	partyMu.Lock()
	defer partyMu.Unlock()
	return partyOf(s.player) != nil
}

func (s *Session) partyLeader() *ConnectedPlayer {
	if s.player == nil {
		return nil
	}
	partyMu.Lock()
	defer partyMu.Unlock()
	if pt := partyOf(s.player); pt != nil {
		return pt.leaderPlayer()
	}
	return nil
}

// hasBenefit reports a positive ability of a buff still active (death
// clears all but the KeepOnDeath ones first).
func (s *Session) hasBenefit(kind uint8) bool {
	for _, b := range s.buffs {
		for _, a := range b.Abilities {
			if a.Type == kind && a.Value > 0 {
				return true
			}
		}
	}
	return false
}

// findRebirthItem is FindItemRebirth: bags 1.. in order, slots 0-23, a
// rune (real type 26, Special 37), first one matching the preference
// (noMove: ReqVg 0, otherwise ReqVg set), then any rune. -1 when none.
func (s *Session) findRebirthItem(noMove bool) int {
	defs := db.MustDefs()
	best := func(prefer bool) int {
		found := -1
		for i, it := range s.items {
			if it.Bag == 0 || it.Count == 0 {
				continue
			}
			d := defs.Item(it.Type, it.TypeID)
			if d == nil || realType(d.Type) != 26 || d.Special != specialRebirth {
				continue
			}
			if prefer && (d.ReqVg == 0) != noMove {
				continue
			}
			if found < 0 || it.Bag < s.items[found].Bag || (it.Bag == s.items[found].Bag && it.Slot < s.items[found].Slot) {
				found = i
			}
		}
		return found
	}
	if i := best(true); i >= 0 {
		return i
	}
	return best(false)
}

// useRebirthItem is ItemUseNSend: one rune consumed, the client told.
func (s *Session) useRebirthItem(i int) {
	res, err := s.consumeOne(i)
	if err != nil {
		s.log.Printf("Rapture World failed to consume resurrection rune: %v", err)
	}
	s.send(buildUseItemResult(s.char.ID, &res), "0x050A resurrection rune")
}

// setPools sets current HP/MP/SP to the effective maxima (half when half).
func (s *Session) setPools(half bool) {
	ch := s.char
	ch.CurrentHP, ch.CurrentMP, ch.CurrentSP = ch.MaxHP, ch.MaxMP, ch.MaxSP
	full := StatsFor(ch, s.equipment())
	target := func(m uint32) uint32 {
		if half {
			return m / 2
		}
		return m
	}
	ch.CurrentHP = satSub(ch.CurrentHP, full.CurrentHP-target(full.MaxHP))
	ch.CurrentMP = satSub(ch.CurrentMP, full.CurrentMP-target(full.MaxMP))
	ch.CurrentSP = satSub(ch.CurrentSP, full.CurrentSP-target(full.MaxSP))
}

func (s *Session) halfPools() { s.setPools(true) }

// rebirth is CUser::Rebirth: full pools when restore, the corpse leaves
// the view (CZone::LeaveDead 0x0406), then on the same map the user moves
// and 0x0551 (id, kind, exp, position) goes to the view; on another map
// 0x0551 goes to the user before the zone change. 0x0505 sends the pools.
func (s *Session) rebirth(mapID uint16, x, y, z float32, kind uint8, restore bool, lost uint32) {
	ch := s.char
	if restore {
		s.setPools(false)
	}
	s.broadcastNear(buildCharacterReviveInit(ch.ID), "0x0406 corpse leave", true)
	s.dead, s.rebirthAt, s.rebirthParty = false, time.Time{}, false
	if s.player != nil {
		s.player.SetAlive(true)
	}
	s.persist()
	stats := s.stats()
	resurrect := buildCharacterResurrect(ch.ID, kind, ch.Exp, x, y, z)
	pools := buildCharacterReviveHitpoints(ch.ID, stats.CurrentHP, stats.CurrentMP, stats.CurrentSP)
	if mapID == ch.MapID {
		if x != ch.PosX || z != ch.PosZ {
			s.clearView(true)
			ch.PosX, ch.PosY, ch.PosZ = x, y, z
			if s.player != nil {
				s.player.UpdateCharacter(*ch)
				s.player.UpdateMotion(0, 0)
			}
			s.savePosition()
			s.placeView()
		} else if s.player != nil {
			s.player.UpdateCharacter(*ch)
		}
		s.broadcastNear(resurrect, "0x0551 rebirth", true)
		s.send(pools, "0x0505 rebirth pools")
	} else {
		s.send(resurrect, "0x0551 rebirth")
		s.send(pools, "0x0505 rebirth pools")
		s.applyTeleport(mapID, x, y, z, "rebirth")
	}
	s.log.Printf("Rapture World rebirth character_id=%d kind=%d map=%d pos=(%.2f, %.2f, %.2f) exp_lost=%d",
		ch.ID, kind, mapID, x, y, z, lost)
}

// ultimateDeath is KillSoul's ultimate-mode end without ability 46 or a
// rune: ExpDrop, the DB agent's 0x0406 (account, character), logout and
// LeaveDead.
func (s *Session) ultimateDeath() {
	ch := s.char
	expDrop(ch, s.deathLoss)
	s.persist()
	s.log.Printf("Rapture World ultimate death character_id=%d (no resurrection rune)", ch.ID)
	s.endUltimateCharacter()
}

// endUltimateCharacter is the rest of that branch: the character leaves the
// world, is deleted (usp_Delete_Char_E, the DB agent's 0x0406 effect; the
// usual restore window applies) and the connection closes (LogoutUser).
func (s *Session) endUltimateCharacter() {
	charID := s.char.ID
	s.leaveWorld()
	if ok, err := db.DeleteCharacter(s.accountID, charID); err != nil || !ok {
		s.log.Printf("Rapture World ultimate death delete failed character_id=%d ok=%v: %v", charID, ok, err)
	}
	if s.conn != nil {
		s.conn.Close()
	}
}
