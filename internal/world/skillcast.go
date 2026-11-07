package world

import (
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

const (
	skillResultNormal      = 0
	skillTargetEscapeRange = 100.0

	targetTypeSelf              = 2
	targetTypeEnemiesNearCaster = 6
	targetTypeEnemiesNearTarget = 7

	periodicHealTick = 3 * time.Second
)

// ActiveBuff is a self-buff on the session's character: stat bonuses
// and/or a heal-over-time.
type ActiveBuff struct {
	ID                     uint32
	SkillID                uint16
	SkillLevel             uint8
	EndsAt                 time.Time
	Effect, Detail         uint8
	Attr                   uint8 // AttrType, for the element buffs 94/95
	DefenceType            uint8
	BlockLeft              uint32 // detail 85 blocks left / 86 chance
	CasterMob              uint32 // the mob that applied a debuff
	prevTick               time.Time
	Abilities              [3]buffAbility
	HealHP, HealSP, HealMP uint32
	nextTickAt             time.Time // zero: no heal component
}

type pendingCast struct {
	targetMobID uint32
	number      uint8
	op          packet.Opcode
	readyAt     time.Time
	targetPos   *[2]float32
}

type buffAbility struct {
	Type  uint8
	Value int32
}

// addBuffs applies each active buff's abilities, as CUser::AddApplySkillBuff
// does: detail 82 adds all three, detail 91 negates the first, 94 and 95
// set the attack and defence element; mob debuffs 62/67 subtract them and
// 68 clears the armour element.
func addBuffs(e EquipmentStats, buffs []ActiveBuff) EquipmentStats {
	for _, b := range buffs {
		debuff := b.Effect == typeEffectDebuff
		switch {
		case !debuff && b.Detail == 94: // attack element
			e.AttackAttr = b.Attr
		case !debuff && b.Detail == 95: // defence element
			e.DefenseAttr = b.Attr
		case debuff && b.Detail == detailNoElement2:
			e.DefenseAttr = 0
		}
		for i, a := range b.Abilities {
			v := a.Value
			if (b.Detail == 91 && i == 0) || debuff {
				v = -v
			}
			applyAbility(&e, b.Effect, a.Type, v)
		}
	}
	return e
}

func newBuff(id uint32, def *db.SkillDef, ends time.Time, log logPrinter) ActiveBuff {
	b := ActiveBuff{ID: id, SkillID: def.SkillID, SkillLevel: def.SkillLevel, EndsAt: ends, Effect: def.TypeEffect,
		Detail: uint8(min(def.TypeDetail, 255)), Attr: def.AttrType,
		HealHP: uint32(max(def.TimeHealHP, 0)), HealSP: uint32(max(def.TimeHealSP, 0)), HealMP: uint32(max(def.TimeHealMP, 0))}
	if b.HealHP+b.HealSP+b.HealMP > 0 {
		b.nextTickAt = time.Now().Add(periodicHealTick)
	}
	b.DefenceType, b.BlockLeft, b.prevTick = def.DefenceType, uint32(def.DefenceValue), time.Now()
	if def.TypeDetail == 82 || def.TypeDetail == 91 || def.TypeDetail == detailAbilityDown || def.TypeDetail == detailAbilityDown2 {
		for i, t := range def.AbilityType {
			b.Abilities[i] = buffAbility{t, int32(def.AbilityValue[i])}
			if !applyAbility(&EquipmentStats{}, def.TypeEffect, t, 0) {
				log.Printf("Rapture World buff ability_type=%d not modeled (value=%d) skill_id=%d", t, def.AbilityValue[i], def.SkillID)
			}
		}
	}
	return b
}

func buildCharacterActiveBuffs(buffs []ActiveBuff) []byte {
	w := packet.New(packet.CharacterActiveBuffs)
	buffs = buffs[:min(len(buffs), 255)]
	w.U8(uint8(len(buffs)))
	for _, b := range buffs {
		w.U32(b.ID)
		w.U16(b.SkillID)
		w.U8(b.SkillLevel)
		w.U32(uint32(max(time.Until(b.EndsAt), 0) / time.Second))
	}
	return w.MustFinish()
}

func buildSkillUseResult(op packet.Opcode, result uint8, sender, target uint32, skillID uint16, level uint8, hp, sp, mp uint16) []byte {
	w := packet.New(op)
	w.U8(result)
	w.U32(sender)
	w.U32(target)
	w.U16(skillID)
	w.U8(level)
	w.U16(hp)
	w.U16(sp)
	w.U16(mp)
	return w.MustFinish()
}

func buildSkillUsedSpMp(sp, mp uint32) []byte {
	w := packet.New(packet.SkillUsedSpMp)
	w.U32(sp)
	w.U32(mp)
	return w.MustFinish()
}

func (s *Session) knownSkill(number uint8) (db.CharacterSkill, *db.SkillDef, bool) {
	for _, k := range s.skills {
		if k.Number == number {
			def := db.MustDefs().Skill(k.SkillID, k.Level)
			return k, def, def != nil
		}
	}
	return db.CharacterSkill{}, nil, false
}

func (s *Session) skillReady(id uint16) bool {
	at, ok := s.skillCooldowns[id]
	return !ok || !at.After(time.Now())
}

// handleUseSkill (0x0517 / 0x0511): bar number, target id. The ack echoes
// the request opcode.
func (s *Session) handleUseSkill(op packet.Opcode, p []byte) {
	if len(p) < 7 || s.char == nil {
		return
	}
	s.cancelMove()
	ch := s.char
	number := p[2]
	target, _ := packet.U32(p[3:])
	if s.mounted || s.cast != nil || s.actionBlocked() {
		s.log.Printf("Rapture World rejected skill use (mounted, casting, stunned or asleep) character_id=%d", ch.ID)
		return
	}
	k, def, ok := s.knownSkill(number)
	if !ok {
		s.log.Printf("Rapture World rejected skill use: no skill in slot number=%d", number)
		return
	}
	castTime := time.Duration(def.ReadyTime) * 250 * time.Millisecond
	if castTime == 0 {
		s.resolveSkill(target, number, op, nil)
		return
	}
	if !s.skillReady(k.SkillID) {
		return
	}
	stats := s.stats()
	if sp, mp := s.skillCosts(def, &stats); stats.CurrentMP < mp || stats.CurrentSP < sp {
		s.log.Printf("Rapture World rejected skill use: not enough MP/SP skill_id=%d", k.SkillID)
		return
	}
	castTarget := target
	if def.TypeEffect == typeEffectHeal || def.TypeEffect == typeEffectBuff {
		castTarget = ch.ID
	}
	var pos *[2]float32
	if def.TypeEffect == typeEffectOnce {
		p, ok := s.userSkillTarget(def, target)
		if !ok {
			return
		}
		if !s.inUserSkillRange(p, def) {
			s.send(buildSkillUseResult(packet.SkillUseNoTarget, 5, ch.ID, 0, k.SkillID, k.Level, 0, 0, 0), "0x0511 skill out of range")
			return
		}
		castTarget = p.CharacterID
	} else if castTarget != ch.ID && def.TargetType != targetTypeEnemiesNearCaster {
		mob, ok := MobByID(ch.MapID, target)
		if !ok {
			return
		}
		pos = &[2]float32{mob.PosX, mob.PosZ}
	}
	s.send(packet.New(packet.AutoAttackStop).MustFinish(), "0x0212 auto attack stop (casting)")
	w := packet.New(packet.SkillCastStarted)
	w.U32(ch.ID)
	w.U32(castTarget)
	w.U16(k.SkillID)
	w.U8(k.Level)
	s.broadcastNear(w.MustFinish(), "0x0510 skill cast started", true)
	s.cast = &pendingCast{target, number, op, time.Now().Add(castTime), pos}
	s.log.Printf("Rapture World skill cast started skill_id=%d level=%d cast_time=%s", k.SkillID, k.Level, castTime)
}

func (s *Session) processPendingCast() {
	c := s.cast
	if c == nil || c.readyAt.After(time.Now()) {
		return
	}
	s.cast = nil
	s.resolveSkill(c.targetMobID, c.number, c.op, c.targetPos)
}

func (s *Session) startCooldown(k db.CharacterSkill, def *db.SkillDef) {
	s.skillCooldowns[k.SkillID] = time.Now().Add(time.Duration(def.ResetTimeSeconds) * time.Second)
	k.Delay = int16(min(def.ResetTimeSeconds, 32767))
	if err := db.SaveCharacterSkill(s.char.ID, k); err != nil {
		s.log.Printf("Rapture World failed to save skill cooldown: %v", err)
	}
}

func (s *Session) resolveSkill(target uint32, number uint8, op packet.Opcode, castPos *[2]float32) {
	ch := s.char
	if ch == nil {
		return
	}
	k, def, ok := s.knownSkill(number)
	if !ok || !s.skillReady(k.SkillID) {
		return
	}
	stats := s.stats()
	if sp, mp := s.skillCosts(def, &stats); stats.CurrentMP < mp || stats.CurrentSP < sp {
		s.log.Printf("Rapture World rejected skill use: not enough MP/SP skill_id=%d", k.SkillID)
		return
	}
	switch def.TypeEffect {
	case typeEffectBuff:
		s.startBuff(k, def, op)
	case typeEffectHeal:
		s.healSkill(k, def, op, stats)
	case typeEffectAttack, typeEffectDebuff:
		s.skillAttack(k, def, target, castPos, stats)
	case typeEffectOnce:
		s.skillOnce(k, def, target, stats)
	default:
		s.log.Printf("Rapture World rejected skill use: effect %d not usable skill_id=%d", def.TypeEffect, k.SkillID)
	}
}

func (s *Session) healSkill(k db.CharacterSkill, def *db.SkillDef, op packet.Opcode, stats CharacterStats) {
	ch := s.char
	s.startCooldown(k, def)
	// CUser::HealUser: a non-zero HealHP adds twice the caster's WIS.
	heal := uint32(max(def.HealHP, 0))
	if heal != 0 {
		heal = satAdd(heal, satMul(stats.Wis, 2))
	}
	hp, mp, sp := s.healDelta(heal, uint32(max(def.HealMP, 0)), uint32(max(def.HealSP, 0)))
	costSP, costMP := s.skillCosts(def, &stats)
	s.spendDelta(costMP, costSP)
	s.persist()
	s.send(buildSkillUsedSpMp(costSP, costMP), "0x050C skill used sp/mp")
	s.log.Printf("Rapture World skill heal skill_id=%d hp=%d mp=%d sp=%d", k.SkillID, hp, sp, mp)
	s.send(buildSkillUseResult(op, skillResultNormal, ch.ID, ch.ID, k.SkillID, k.Level, 0, 0, 0), "skill heal")
	st := s.stats()
	s.send(buildCurrentHitpoints(&st), "0x0521 current hp/mp/sp after heal")
}

func (s *Session) startBuff(k db.CharacterSkill, def *db.SkillDef, op packet.Opcode) {
	ch := s.char
	stats := s.stats()
	s.startCooldown(k, def)
	sp, mp := s.skillCosts(def, &stats)
	s.spendDelta(mp, sp)
	s.persist()
	s.send(buildSkillUsedSpMp(sp, mp), "0x050C skill used sp/mp")
	s.send(buildSkillUseResult(op, skillResultNormal, ch.ID, ch.ID, k.SkillID, k.Level, 0, 0, 0), "skill buff cast")
	if _, ok := s.addEffect(def, 0); ok {
		s.log.Printf("Rapture World skill buff started skill_id=%d level=%d keep_time=%d", k.SkillID, k.Level, def.KeepSeconds)
	}
}

func (s *Session) sendBuffStats(why string) {
	st := s.stats()
	s.send(buildAdditionalStats(&st), "0x0526 additional stats "+why)
	s.send(buildMaxHPMPSP(s.char, &st), "0x051F max hp/mp/sp "+why)
	s.send(buildCurrentHitpoints(&st), "0x0521 current hp/mp/sp "+why)
}

// processBuffs ticks heal-over-time and debuff damage, then expires
// applied skills (CUser::ChkKeepSkill / ChkEndTimeSkill).
func (s *Session) processBuffs() {
	if len(s.buffs) == 0 || s.char == nil || s.dead {
		return
	}
	now := time.Now()
	if s.tickDebuffs(now) {
		return
	}
	for i := range s.buffs {
		b := &s.buffs[i]
		expiring := !b.EndsAt.After(now)
		hasHeal := b.HealHP+b.HealSP+b.HealMP > 0
		due := !b.nextTickAt.IsZero() && !b.nextTickAt.After(now)
		if hasHeal && (due || expiring) {
			hp, mp, sp := s.healDelta(b.HealHP, b.HealMP, b.HealSP)
			s.persist()
			w := packet.New(packet.SkillKeep)
			w.U32(s.char.ID)
			w.U16(b.SkillID)
			w.U8(b.SkillLevel)
			w.U16(uint16(min(hp, 65535)))
			w.U16(uint16(min(mp, 65535)))
			w.U16(uint16(min(sp, 65535)))
			s.send(w.MustFinish(), "0x050F buff heal tick")
			st := s.stats()
			s.send(buildCurrentHitpoints(&st), "0x0521 current hp/mp/sp after buff heal tick")
			if !expiring {
				b.nextTickAt = b.nextTickAt.Add(periodicHealTick)
			}
		}
	}
	s.removeEffects(func(b *ActiveBuff) bool {
		if b.EndsAt.After(now) {
			return false
		}
		s.log.Printf("Rapture World applied skill expired skill_id=%d level=%d", b.SkillID, b.SkillLevel)
		return true
	}, "after expiry")
}

// persistBuffs snapshots remaining buff time (frozen while offline).
func (s *Session) persistBuffs() {
	if s.char == nil {
		return
	}
	var rows []db.AppliedBuff
	for _, b := range s.buffs {
		rows = append(rows, db.AppliedBuff{SkillID: b.SkillID, Level: b.SkillLevel, LeftSeconds: uint32(max(time.Until(b.EndsAt), 0) / time.Second)})
	}
	if err := db.SaveAppliedBuffs(s.char.ID, rows); err != nil {
		s.log.Printf("Rapture World failed to save buffs: %v", err)
	}
}

// persistSkillDelays saves each skill's remaining cooldown.
func (s *Session) persistSkillDelays() {
	if s.char == nil {
		return
	}
	for _, k := range s.skills {
		left := int16(0)
		if at, ok := s.skillCooldowns[k.SkillID]; ok {
			left = int16(min(max(time.Until(at), 0)/time.Second, 32767))
		}
		if left != k.Delay {
			k.Delay = left
			db.SaveCharacterSkill(s.char.ID, k)
		}
	}
}

func restoreBuffs(charID uint32, log logPrinter) []ActiveBuff {
	rows, err := db.AppliedBuffs(charID)
	if err != nil {
		log.Printf("Rapture World failed to read buffs: %v", err)
		return nil
	}
	var out []ActiveBuff
	for _, r := range rows {
		def := db.MustDefs().Skill(r.SkillID, r.Level)
		if def == nil || def.TypeEffect != typeEffectBuff {
			continue
		}
		out = append(out, newBuff(uint32(len(out)+1), def, time.Now().Add(time.Duration(r.LeftSeconds)*time.Second), log))
	}
	return out
}
