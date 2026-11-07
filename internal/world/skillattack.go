package world

import (
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// Skill attacks on mobs as CUser::SkillAttackSelect / SkillAttackRange /
// SkillAttackRangeSelect and CUser::AttackMob(skill) (0x0044fd90) run
// them. Packets: 0x0517 first hit, 0x0518 drain (detail 27, three heal
// words appended), 0x0519 later and area hits, 0x0511 result 4 announces
// an area skill, 0x051A the skill's AddDamage. Result byte: 0 normal,
// 1 critical, 3 miss.

const (
	opSkillDrain   packet.Opcode = 0x0518
	opSkillOther   packet.Opcode = 0x0519
	resultAnnounce               = 4

	detailAttack     = 21
	detailMulti      = 22
	detailSacrifice  = 23
	detailNoElement  = 24
	detailDrain      = 27
	typeEffectAttack = 1
	typeEffectHeal   = 2
	typeEffectDebuff = 3
	typeEffectBuff   = 4
)

// skillCost is CUser::UseSkill's cost: the reduction is a percentage of
// the cost; a cost of 1 spends the whole pool.
func skillCost(cost uint16, reduction int32, pool uint32) uint32 {
	if cost == 1 {
		return pool
	}
	discount := int32(float64(float32(cost)) / 100 * float64(float32(reduction)))
	return uint32(max(int32(cost)-discount, 0))
}

func (s *Session) skillCosts(def *db.SkillDef, st *CharacterStats) (sp, mp uint32) {
	return skillCost(def.SPCost, st.SPCostCut, st.CurrentSP), skillCost(def.MPCost, st.MPCostCut, st.CurrentMP)
}

// skillAttack runs an attack skill (TypeEffect 1) or a debuff (3) on mobs.
func (s *Session) skillAttack(k db.CharacterSkill, def *db.SkillDef, target uint32, castPos *[2]float32, stats CharacterStats) {
	ch := s.char
	if def.TypeDetail < detailAttack || def.TypeDetail > detailDrain {
		if def.TypeEffect == typeEffectAttack {
			s.log.Printf("Rapture World rejected skill use: attack detail %d not modeled skill_id=%d", def.TypeDetail, k.SkillID)
			return
		}
	}
	if stats.Disabled[def.Kind] > 0 {
		s.log.Printf("Rapture World rejected skill use: category %d disabled skill_id=%d", def.Kind, k.SkillID)
		return
	}
	var center *MapMob
	if def.TargetType != targetTypeEnemiesNearCaster {
		mob, ok := MobByID(ch.MapID, target)
		if !ok || mob.CurrentHP == 0 {
			return
		}
		if castPos != nil {
			if DistanceXZ(castPos[0], castPos[1], mob.PosX, mob.PosZ) > skillTargetEscapeRange {
				s.log.Printf("Rapture World rejected skill use: target escaped during cast skill_id=%d", k.SkillID)
				return
			}
		} else if !s.inSkillRange(&mob, def, &stats) {
			s.log.Printf("Rapture World rejected skill use: out of range skill_id=%d", k.SkillID)
			return
		}
		center = &mob
	}
	sp, mp := s.skillCosts(def, &stats)
	s.startCooldown(k, def)
	s.spendDelta(mp, sp)
	s.persist()
	s.send(buildSkillUsedSpMp(sp, mp), "0x050C skill used sp/mp")
	s.log.Printf("Rapture World skill use skill_id=%d level=%d effect=%d detail=%d target_type=%d kind=%d",
		k.SkillID, k.Level, def.TypeEffect, def.TypeDetail, def.TargetType, def.Kind)

	switch def.TargetType {
	case targetTypeEnemiesNearCaster, targetTypeEnemiesNearTarget:
		x, z, announced := ch.PosX, ch.PosZ, uint32(0)
		if center != nil {
			x, z, announced = center.PosX, center.PosZ, center.ID
		}
		sendNear(ch.MapID, ch.PosX, ch.PosZ, CombatRadius,
			buildSkillUseResult(packet.SkillUseNoTarget, resultAnnounce, ch.ID, announced, k.SkillID, k.Level, 0, 0, 0), "0x0511 area skill")
		// Area skills skip drain; within ApplyRange inclusive.
		if def.TypeDetail == detailDrain {
			return
		}
		for _, id := range mobsWithin(ch.MapID, x, z, float32(def.ApplyRange)) {
			s.skillStrike(id, def, k, false)
		}
	default:
		if def.TypeDetail == detailMulti {
			sendNear(ch.MapID, center.PosX, center.PosZ, CombatRadius,
				buildSkillUseResult(packet.SkillUse, resultAnnounce, ch.ID, center.ID, k.SkillID, k.Level, 0, 0, 0), "0x0517 multi-hit skill")
		}
		s.skillStrike(center.ID, def, k, true)
	}
}

// skillStrike is one target of a skill: detail 22 hits MultiAttack times
// (stopping when the mob or the caster dies), 23 then sacrifices the
// caster's remaining HP.
func (s *Session) skillStrike(target uint32, def *db.SkillDef, k db.CharacterSkill, first bool) {
	if def.TypeDetail == detailMulti {
		for i := uint8(0); i < def.MultiAttack; i++ {
			if !s.skillHit(target, def, k, false) || s.char.CurrentHP == 0 {
				return
			}
		}
		return
	}
	s.skillHit(target, def, k, first)
	if def.TypeDetail == detailSacrifice && first {
		s.sacrifice()
	}
}

// sacrifice is detail 23's CUser::Damage of the caster's whole HP.
func (s *Session) sacrifice() {
	ch := s.char
	st := s.stats()
	if st.CurrentHP == 0 {
		return
	}
	ch.CurrentHP = 0
	ch.Deaths++
	s.persist()
	refreshed := s.stats()
	s.send(buildCurrentHitpoints(&refreshed), "0x0521 current hp after sacrifice")
	state, death := buildCharacterDeathState(ch.ID), buildCharacterDeath(ch.ID, deathTypeMob, ch.ID)
	for _, p := range PlayersNear(ch.MapID, ch.PosX, ch.PosZ, CombatRadius) {
		p.Send(state, "0x051C character death state")
		p.Send(death, "0x0504 character death")
	}
	if s.player != nil {
		s.player.SetAlive(false)
	}
	s.dead, s.autoAttack, s.cast = true, false, nil
	s.clearBuffsOnDeath()
	s.log.Printf("Rapture World character sacrificed itself character_id=%d", ch.ID)
}

func mobsWithin(mapID uint16, x, z, r float32) []uint32 {
	var out []uint32
	for _, m := range EntitiesOnMap(mapID).Mobs {
		if m.CurrentHP > 0 && DistanceXZ(x, z, m.PosX, m.PosZ) <= r {
			out = append(out, m.ID)
		}
	}
	return out
}

// skillHit is CUser::AttackMob with a skill. It returns whether the mob is
// still alive.
func (s *Session) skillHit(target uint32, def *db.SkillDef, k db.CharacterSkill, first bool) bool {
	ch := s.char
	mob, ok := MobByID(ch.MapID, target)
	if !ok || mob.CurrentHP == 0 {
		return false
	}
	md := db.MustDefs().Mobs[mob.MobID]
	if md == nil {
		return false
	}
	stats := s.stats()
	mc, _ := MobCombatByID(ch.MapID, target)
	drain := first && def.TypeDetail == detailDrain
	op := opSkillOther
	switch {
	case drain:
		op = opSkillDrain
	case first:
		op = packet.SkillUse
	}
	SetMobAggro(ch.MapID, target, ch.ID)
	s.lastCombatAt = time.Now()
	if !playerSkillHits(&stats, &mc, md, def) {
		frame := buildSkillUseResult(op, attackResultMiss, ch.ID, mob.ID, k.SkillID, k.Level, 0, 0, 0)
		if drain {
			frame = buildSkillDrainResult(attackResultMiss, ch.ID, mob.ID, k.SkillID, k.Level, [3]uint16{}, [3]uint16{})
		}
		sendNear(ch.MapID, mob.PosX, mob.PosZ, CombatRadius, frame, "skill use (miss)")
		return true
	}
	s.wearWeapon()
	t := skillTarget{MaxHP: mob.MaxHP, HP: mob.CurrentHP, MaxMP: md.MaxMP, MaxSP: md.MaxSP, MP: md.MaxMP, SP: md.MaxSP}
	caster := [3]uint32{stats.CurrentHP, stats.CurrentSP, stats.CurrentMP}
	close := DistanceXZ(ch.PosX, ch.PosZ, mob.PosX, mob.PosZ) <= closeRangeDistance
	damage, crit := playerSkillDamage(&stats, &mc, def, t, caster, close)
	r, ok := DamageMob(ch.MapID, target, uint32(max(damage[0], 0)))
	if !ok {
		return false
	}
	AddMobAggro(ch.MapID, target, ch.ID, int64(r.AppliedHP)+int64(def.Aggro))
	code := uint8(attackSuccessNormal)
	if crit {
		code = attackResultCritical
	}
	dealt := [3]uint16{r.DamageHP, uint16(min(max(damage[1], 0), 65535)), uint16(min(max(damage[2], 0), 65535))}
	frame := buildSkillUseResult(op, code, ch.ID, r.Mob.ID, k.SkillID, k.Level, dealt[0], dealt[1], dealt[2])
	if drain {
		// The caster heals what it drained (CUser::Heal), HP/SP/MP.
		hp, mp, sp := s.healDelta(uint32(dealt[0]), uint32(dealt[2]), uint32(dealt[1]))
		frame = buildSkillDrainResult(code, ch.ID, r.Mob.ID, k.SkillID, k.Level, dealt,
			[3]uint16{uint16(min(hp, 65535)), uint16(min(sp, 65535)), uint16(min(mp, 65535))})
		s.persist()
	}
	sendNear(ch.MapID, r.Mob.PosX, r.Mob.PosZ, CombatRadius, frame, "skill use")
	if drain {
		st := s.stats()
		s.send(buildCurrentHitpoints(&st), "0x0521 current hp after drain")
	}
	alive := s.resolveMobDamage(&r)
	if alive && !drain && (def.AddDamageHP != 0 || def.AddDamageSP != 0 || def.AddDamageMP != 0) {
		if x, ok := DamageMob(ch.MapID, target, uint32(uint16(def.AddDamageHP))); ok {
			w := packet.New(packet.SkillExtraDamage)
			w.U32(ch.ID)
			w.U32(x.Mob.ID)
			w.U16(k.SkillID)
			w.U8(k.Level)
			w.U16(x.DamageHP)
			w.U16(uint16(def.AddDamageSP))
			w.U16(uint16(def.AddDamageMP))
			sendNear(ch.MapID, x.Mob.PosX, x.Mob.PosZ, CombatRadius, w.MustFinish(), "0x051A skill extra damage")
			alive = s.resolveMobDamage(&x)
		}
	}
	// TypeEffect 3 leaves the skill on the mob (CMob::AddApplySkill).
	if alive && def.TypeEffect == typeEffectDebuff {
		if speed, ok := ApplyMobEffect(ch.MapID, target, ch.ID, def); ok && speed != nil {
			sendNear(ch.MapID, r.Mob.PosX, r.Mob.PosZ, BroadcastRadius, speed, "0x0609 mob speed")
		}
	}
	return alive
}

// buildSkillDrainResult (0x0518): the 0x0517 layout plus the healed
// HP/SP/MP.
func buildSkillDrainResult(result uint8, sender, target uint32, skillID uint16, level uint8, damage, healed [3]uint16) []byte {
	w := packet.New(opSkillDrain)
	w.U8(result)
	w.U32(sender)
	w.U32(target)
	w.U16(skillID)
	w.U8(level)
	for _, v := range damage {
		w.U16(v)
	}
	for _, v := range healed {
		w.U16(v)
	}
	return w.MustFinish()
}

// inSkillRange is CUser::EnableAttackSkillRange against a mob: the skill's
// AttackRange plus the mob's size and the 2.3 unit tolerance (and the
// movement allowance, at least 2).
func (s *Session) inSkillRange(m *MapMob, def *db.SkillDef, st *CharacterStats) bool {
	size := float32(0)
	if md := db.MustDefs().Mobs[m.MobID]; md != nil {
		size = float32(md.Size)
	}
	reach := float32(def.AttackRange) + size + 2.3 + 2
	return DistanceXZ(s.char.PosX, s.char.PosZ, m.PosX, m.PosZ) <= reach
}

// clearBuffsOnDeath is CUser::ClearApplySkillByDeath: every active skill
// ends except those with FixRange set.
func (s *Session) clearBuffsOnDeath() {
	s.deathLoss = s.deathExpLoss()
	s.startRebirthTimer()
	kept := s.buffs[:0]
	var removed []uint32
	for _, b := range s.buffs {
		if d := db.MustDefs().Skill(b.SkillID, b.SkillLevel); d != nil && d.KeepOnDeath {
			kept = append(kept, b)
			continue
		}
		removed = append(removed, b.ID)
	}
	s.buffs = kept
	for _, id := range removed {
		w := packet.New(packet.BuffRemove)
		w.U32(id)
		s.send(w.MustFinish(), "0x050E buff removed by death")
	}
	if len(removed) > 0 {
		s.sendBuffStats("after death")
	}
}
