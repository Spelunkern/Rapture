package world

import (
	"math/rand/v2"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// TypeEffect 6 skills are CUser::SkillOnce 0x00453610 (reached through
// UseSkillUser / AttackUser), switched on the skill's Type (TypeDetail).
const (
	typeEffectOnce = 6

	detailLifeSkill = 121 // resurrection: revive a dead user (LifeSkill)
	detailKillSoul  = 122 // send a dead non-ultimate user to its bind point
	detailDetect    = 123 // reveal an enemy's defence element, level, mode
	detailCheck     = 124 // success roll only
	detailReturn    = 125 // move the caster to its bind point

	skillResultFailed    = 3
	skillResultNotTarget = 11
)

// pendingLifeSkill asks a dead user's session to revive at the caster.
type pendingLifeSkill struct {
	CasterID uint32
	X, Y, Z  float32
}

func (p *ConnectedPlayer) QueueLifeSkill(l pendingLifeSkill) {
	p.mailMu.Lock()
	p.lifeSkill = &l
	p.mailMu.Unlock()
}

func (p *ConnectedPlayer) TakeLifeSkill() *pendingLifeSkill {
	p.mailMu.Lock()
	defer p.mailMu.Unlock()
	l := p.lifeSkill
	p.lifeSkill = nil
	return l
}

// QueueSoulKill asks a dead user's session to run KillSoul.
func (p *ConnectedPlayer) QueueSoulKill() {
	p.mailMu.Lock()
	p.soulKill = true
	p.mailMu.Unlock()
}

func (p *ConnectedPlayer) TakeSoulKill() bool {
	p.mailMu.Lock()
	defer p.mailMu.Unlock()
	k := p.soulKill
	p.soulKill = false
	return k
}

// equipmentDefenseAttr is the user's armour element (CUser +0x1284).
func (p *ConnectedPlayer) equipmentDefenseAttr() uint8 {
	p.mu.Lock()
	items := append([]db.Item(nil), p.items...)
	p.mu.Unlock()
	return GetEquipmentStats(items).DefenseAttr
}

// buildDetectInfo (0x0558): target defence element u32, level u16, mode u8.
func buildDetectInfo(attr uint32, level uint16, mode uint8) []byte {
	w := packet.New(packet.SkillDetectInfo)
	w.U32(attr)
	w.U16(level)
	w.U8(mode)
	return w.MustFinish()
}

// userSkillTarget resolves a TypeEffect 6 target: the caster itself for
// self skills (TargetType 2), else a user on the caster's map.
func (s *Session) userSkillTarget(def *db.SkillDef, target uint32) (*ConnectedPlayer, bool) {
	if def.TargetType == targetTypeSelf || target == s.char.ID {
		return s.player, s.player != nil
	}
	p := FindPlayerByID(target)
	if p == nil || p.MapID() != s.char.MapID {
		return nil, false
	}
	return p, true
}

// inUserSkillRange is CUser::EnableAttackRange against a user: AttackRange
// plus the 2.3 unit tolerance and the movement allowance.
func (s *Session) inUserSkillRange(p *ConnectedPlayer, def *db.SkillDef) bool {
	if p == s.player {
		return true
	}
	x, z := p.PositionXZ()
	return DistanceXZ(s.char.PosX, s.char.PosZ, x, z) <= float32(def.AttackRange)+2.3+2
}

// skillOnceSucceeds is CUser::AttackSuccess 0x00456b10 for these skills:
// a block (ChkBlock; checked when the caster is the target) fails it, then
// SuccessType 1 rolls SuccessValue%. Types 2 and 3 add 100 x
// caster/(caster+target) LUC or WIS in integer division, which is 0 unless
// the target's stat is 0, so they roll SuccessValue% too. The level-based
// default and the state resistances are not modelled: every EP4.5
// TypeEffect 6 skill is SuccessType 1.
func (s *Session) skillOnceSucceeds(def *db.SkillDef, self bool) bool {
	if self && s.blocksAttack(int(def.Kind)) {
		return false
	}
	switch def.SuccessType {
	case 1, 2, 3:
		return rand.IntN(100) < int(def.SuccessValue)
	}
	return true
}

// skillOnce runs a TypeEffect 6 skill after its cast: SP/MP and the reset
// time are spent whatever the outcome (AttackUser 0x00451d1d).
func (s *Session) skillOnce(k db.CharacterSkill, def *db.SkillDef, target uint32, stats CharacterStats) {
	ch := s.char
	p, ok := s.userSkillTarget(def, target)
	if !ok || !s.inUserSkillRange(p, def) {
		s.send(buildSkillUseResult(packet.SkillUseNoTarget, 5, ch.ID, 0, k.SkillID, k.Level, 0, 0, 0), "0x0511 skill out of range")
		return
	}
	s.startCooldown(k, def)
	sp, mp := s.skillCosts(def, &stats)
	s.spendDelta(mp, sp)
	s.persist()
	s.send(buildSkillUsedSpMp(sp, mp), "0x050C skill used sp/mp")
	result := func(code uint8, targetID uint32) {
		s.broadcastNear(buildSkillUseResult(packet.SkillUseNoTarget, code, ch.ID, targetID, k.SkillID, k.Level, 0, 0, 0),
			"0x0511 skill once", true)
	}
	self := p == s.player
	switch def.TypeDetail {
	case detailLifeSkill:
		if p.IsAlive() {
			s.send(buildSkillUseResult(packet.SkillUseNoTarget, skillResultNotTarget, ch.ID, 0, k.SkillID, k.Level, 0, 0, 0),
				"0x0511 resurrection target alive")
			return
		}
		if !s.skillOnceSucceeds(def, self) {
			result(skillResultFailed, p.CharacterID)
			return
		}
		result(skillResultNormal, p.CharacterID)
		p.QueueLifeSkill(pendingLifeSkill{ch.ID, ch.PosX, ch.PosY, ch.PosZ})
	case detailKillSoul:
		if p.Snapshot().Mode != growUltimate && s.skillOnceSucceeds(def, self) {
			result(skillResultNormal, p.CharacterID)
			p.QueueSoulKill()
			return
		}
		result(skillResultNormal, p.CharacterID)
	case detailDetect:
		if !s.skillOnceSucceeds(def, self) {
			result(skillResultFailed, p.CharacterID)
			return
		}
		if p.Faction() == s.faction {
			result(skillResultNotTarget, p.CharacterID)
			return
		}
		result(skillResultNormal, p.CharacterID)
		t := p.Snapshot()
		s.send(buildDetectInfo(uint32(p.equipmentDefenseAttr()), t.Level, t.Mode), "0x0558 detect info")
	case detailCheck:
		if !s.skillOnceSucceeds(def, self) {
			result(skillResultFailed, p.CharacterID)
			return
		}
		result(skillResultNormal, p.CharacterID)
	case detailReturn:
		if s.dead {
			return
		}
		result(skillResultNormal, 0)
		if m, x, y, z, ok := bindPositionFound(ch.MapID, s.faction, ch.PosX, ch.PosY, ch.PosZ); ok {
			// SkillOnce sets the move position (type 1) due now; the next
			// update (UpdateResetPosition) moves the user.
			s.player.QueueTeleport(m, x, y, z)
		}
	default:
		s.log.Printf("Rapture World skill once with unknown type %d skill_id=%d", def.TypeDetail, k.SkillID)
	}
	s.log.Printf("Rapture World skill once skill_id=%d type=%d target=%d", k.SkillID, def.TypeDetail, target)
}

// tickSkillOnce applies queued skill effects on this (dead) user.
func (s *Session) tickSkillOnce() {
	p := s.player
	if p == nil {
		return
	}
	if l := p.TakeLifeSkill(); l != nil && s.dead {
		s.lifeSkill(l)
	}
	if p.TakeSoulKill() && s.dead {
		s.killSoul()
	}
}

// lifeSkill is CUser::LifeSkill 0x00459d20: the corpse leaves the view,
// 0x0551 kind 4 with the user's exp and the caster's position goes to the
// view, the pools are full, no exp is lost.
func (s *Session) lifeSkill(l *pendingLifeSkill) {
	ch := s.char
	s.broadcastNear(buildCharacterReviveInit(ch.ID), "0x0406 corpse leave", true)
	s.setPools(false)
	s.dead, s.rebirthAt, s.rebirthParty = false, time.Time{}, false
	ch.PosX, ch.PosY, ch.PosZ = l.X, l.Y, l.Z
	if s.player != nil {
		s.player.SetAlive(true)
		s.player.UpdateCharacter(*ch)
	}
	s.persist()
	s.placeView()
	s.broadcastNear(buildCharacterResurrect(ch.ID, rebirthKindBind, ch.Exp, l.X, l.Y, l.Z), "0x0551 life skill", true)
	st := s.stats()
	s.send(buildCharacterReviveHitpoints(ch.ID, st.CurrentHP, st.CurrentMP, st.CurrentSP), "0x0505 life skill pools")
	s.log.Printf("Rapture World revived by skill character_id=%d caster=%d", ch.ID, l.CasterID)
}
