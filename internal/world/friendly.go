package world

import (
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// Heals and buffs on users follow CUser::ChkTargetUser 0x0044e140,
// CUser::SkillHeal / SkillBuff and their Party / NParty variants, HealUser
// 0x00450670 and BuffUser: TargetType 3 (and 7) is one chosen user, who
// must be friendly (CUser::IsTeamUsable 0x0044bf30: the caster, or alive,
// same country or a WarType P map), 4 and 5 the caster's party members on
// the map within the skill's ApplyRange (a raid only its group), anything
// else the caster. A single target answers 0x0511 (result, caster, target,
// skill, level, HP/SP/MP healed) to the view; a party skill announces
// 0x0511 result 4 and sends each member's 0x0513.

const (
	targetTypeOther    = 3
	targetTypeParty    = 4
	targetTypeNParty   = 5
	targetTypeOtherAlt = 7

	detailHeal     = 41
	detailCure     = 42
	skillResultPty = 4
)

// pendingFriendly is a heal, cure or buff to apply in a target's session.
type pendingFriendly struct {
	SkillID      uint16
	Level        uint8
	HP, MP, SP   uint32
	Cure, IsBuff bool
}

func (p *ConnectedPlayer) QueueFriendly(f pendingFriendly) {
	p.mailMu.Lock()
	p.friendly = append(p.friendly, f)
	p.mailMu.Unlock()
}

func (p *ConnectedPlayer) TakeFriendly() []pendingFriendly {
	p.mailMu.Lock()
	defer p.mailMu.Unlock()
	f := p.friendly
	p.friendly = nil
	return f
}

// teamUsable is IsTeamUsable without duels (none exist yet).
func (s *Session) teamUsable(p *ConnectedPlayer) bool {
	if p == s.player {
		return true
	}
	if z, ok := mapConfig.Zones[realMap(s.char.MapID)]; ok && z.WarType == "P" {
		return true
	}
	return p.Faction() == s.faction
}

// friendlyTargets resolves the users a heal or buff reaches; ok false is
// ChkTargetUser's refusal (0x0511 result 11).
func (s *Session) friendlyTargets(def *db.SkillDef, target uint32) ([]*ConnectedPlayer, bool, bool) {
	me := s.player
	switch def.TargetType {
	case targetTypeOther, targetTypeOtherAlt:
		if target == 0 || target == s.char.ID {
			return []*ConnectedPlayer{me}, false, true
		}
		p := FindPlayerByID(target)
		if p == nil || p.MapID() != s.char.MapID || !p.IsAlive() || !s.teamUsable(p) {
			return nil, false, false
		}
		x, z := p.PositionXZ()
		if DistanceXZ(s.char.PosX, s.char.PosZ, x, z) > float32(def.AttackRange)+2.3+2 {
			return nil, false, false
		}
		return []*ConnectedPlayer{p}, false, true
	case targetTypeParty, targetTypeNParty:
		partyMu.Lock()
		pt := partyOf(me)
		var members []*ConnectedPlayer
		if pt != nil {
			group := -1
			if pt.kind != raidKindNormal {
				for i, m := range pt.members {
					if m == me && i < len(pt.slots) {
						group = int(pt.slots[i]) / 5
					}
				}
			}
			for i, m := range pt.members {
				if group >= 0 && (i >= len(pt.slots) || int(pt.slots[i])/5 != group) {
					continue
				}
				members = append(members, m)
			}
		}
		partyMu.Unlock()
		if len(members) == 0 {
			return []*ConnectedPlayer{me}, true, true
		}
		var out []*ConnectedPlayer
		for _, m := range members {
			if m != me && (m.MapID() != s.char.MapID || !m.IsAlive() || !s.teamUsable(m)) {
				continue
			}
			x, z := m.PositionXZ()
			if DistanceXZ(s.char.PosX, s.char.PosZ, x, z) > float32(def.ApplyRange) {
				continue
			}
			out = append(out, m)
		}
		return out, true, true
	}
	return []*ConnectedPlayer{me}, false, true
}

// friendlySkill casts a heal (TypeEffect 2) or buff (4) on its targets.
func (s *Session) friendlySkill(k db.CharacterSkill, def *db.SkillDef, target uint32, stats CharacterStats) {
	ch := s.char
	targets, party, ok := s.friendlyTargets(def, target)
	if !ok {
		s.send(buildSkillUseResult(packet.SkillUseNoTarget, skillResultNotTarget, ch.ID, 0, k.SkillID, k.Level, 0, 0, 0),
			"0x0511 skill target refused")
		return
	}
	s.startCooldown(k, def)
	sp, mp := s.skillCosts(def, &stats)
	s.spendDelta(mp, sp)
	s.persist()
	s.send(buildSkillUsedSpMp(sp, mp), "0x050C skill used sp/mp")
	heal := pendingFriendly{SkillID: k.SkillID, Level: k.Level, IsBuff: def.TypeEffect == typeEffectBuff}
	if !heal.IsBuff {
		switch def.TypeDetail {
		case detailCure:
			heal.Cure = true
		default:
			// HealUser: a non-zero HealHP adds twice the caster's WIS.
			if def.HealHP > 0 {
				heal.HP = satAdd(uint32(def.HealHP), satMul(stats.Wis, 2))
			}
			heal.MP, heal.SP = uint32(max(def.HealMP, 0)), uint32(max(def.HealSP, 0))
		}
	}
	if party {
		s.broadcastNear(buildSkillUseResult(packet.SkillUseNoTarget, skillResultPty, ch.ID, 0, k.SkillID, k.Level, 0, 0, 0),
			"0x0511 party skill", true)
	}
	for _, p := range targets {
		op := packet.SkillUseNoTarget
		if party {
			op = packet.SkillPartyMember
		}
		frame := buildSkillUseResult(op, skillResultNormal, ch.ID, p.CharacterID, k.SkillID, k.Level,
			uint16(min(heal.HP, 65535)), uint16(min(heal.SP, 65535)), uint16(min(heal.MP, 65535)))
		x, z := p.PositionXZ()
		sendNear(p.MapID(), x, z, BroadcastRadius, frame, "skill on user")
		if p == s.player {
			s.applyFriendly(heal)
		} else {
			p.QueueFriendly(heal)
		}
	}
	s.log.Printf("Rapture World friendly skill skill_id=%d level=%d targets=%d party=%v", k.SkillID, k.Level, len(targets), party)
}

// applyFriendly is HealUser / BuffUser on this user.
func (s *Session) applyFriendly(f pendingFriendly) {
	if s.char == nil || s.dead {
		return
	}
	switch {
	case f.IsBuff:
		if def := db.MustDefs().Skill(f.SkillID, f.Level); def != nil {
			s.addEffect(def, 0)
		}
	case f.Cure:
		s.removeEffects(func(b *ActiveBuff) bool { return b.Effect == typeEffectDebuff }, "cure")
	default:
		s.healDelta(f.HP, f.MP, f.SP)
		s.persist()
		st := s.stats()
		s.send(buildCurrentHitpoints(&st), "0x0521 healed")
	}
}

func (s *Session) tickFriendly() {
	if s.player == nil {
		return
	}
	for _, f := range s.player.TakeFriendly() {
		s.applyFriendly(f)
	}
}

// cancelBuff is 0x0511 with 0xFF: CUser::RemApplySkill of an own buff.
func (s *Session) cancelBuff(skillID uint32) {
	s.removeEffects(func(b *ActiveBuff) bool {
		return uint32(b.SkillID) == skillID && b.Effect == typeEffectBuff
	}, "cancelled by the user")
}
