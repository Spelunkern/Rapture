package world

import (
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

const (
	skillResultNormal      = 0
	skillRangeGrace        = 2.0
	skillTargetEscapeRange = 100.0

	targetTypeAnyEnemy          = 1
	targetTypeSelectedEnemy     = 3
	targetTypeEnemiesNearCaster = 6
	targetTypeEnemiesNearTarget = 7

	typeDetailInstantHeal  = 41
	typeDetailPeriodicHeal = 81
	typeDetailBuff         = 82
	periodicHealTick       = 3 * time.Second
	typeAttackMagic        = 3
)

// ActiveBuff is a self-buff on the session's character: stat bonuses
// and/or a heal-over-time.
type ActiveBuff struct {
	ID                     uint32
	SkillID                uint16
	SkillLevel             uint8
	EndsAt                 time.Time
	Bonus                  EquipmentStats
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

func addBuffs(e EquipmentStats, buffs []ActiveBuff) EquipmentStats {
	for _, b := range buffs {
		e.Str, e.Rec, e.Int = satAdd(e.Str, b.Bonus.Str), satAdd(e.Rec, b.Bonus.Rec), satAdd(e.Int, b.Bonus.Int)
		e.Wis, e.Dex, e.Luc = satAdd(e.Wis, b.Bonus.Wis), satAdd(e.Dex, b.Bonus.Dex), satAdd(e.Luc, b.Bonus.Luc)
		e.MaxHP, e.MaxMP, e.MaxSP = satAdd(e.MaxHP, b.Bonus.MaxHP), satAdd(e.MaxMP, b.Bonus.MaxMP), satAdd(e.MaxSP, b.Bonus.MaxSP)
	}
	return e
}

func newBuff(id uint32, def *db.SkillDef, ends time.Time, log logPrinter) ActiveBuff {
	b := ActiveBuff{ID: id, SkillID: def.SkillID, SkillLevel: def.SkillLevel, EndsAt: ends,
		HealHP: uint32(max(def.TimeHealHP, 0)), HealSP: uint32(max(def.TimeHealSP, 0)), HealMP: uint32(max(def.TimeHealMP, 0))}
	if b.HealHP+b.HealSP+b.HealMP > 0 {
		b.nextTickAt = time.Now().Add(periodicHealTick)
	}
	for i, t := range def.AbilityType {
		v := uint32(max(def.AbilityValue[i], 0))
		switch t {
		case 0:
		case 1:
			b.Bonus.MaxHP += v
		case 2:
			b.Bonus.MaxMP += v
		case 3:
			b.Bonus.MaxSP += v
		case 4:
			b.Bonus.Str += v
		case 5:
			b.Bonus.Rec += v
		case 6:
			b.Bonus.Int += v
		case 7:
			b.Bonus.Wis += v
		case 8:
			b.Bonus.Dex += v
		case 9:
			b.Bonus.Luc += v
		default:
			log.Printf("Rapture World buff ability_type=%d not implemented (value=%d), ignored", t, def.AbilityValue[i])
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
	ch := s.char
	number := p[2]
	target, _ := packet.U32(p[3:])
	if s.mounted || s.cast != nil {
		s.log.Printf("Rapture World rejected skill use (mounted or already casting) character_id=%d", ch.ID)
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
	if stats.CurrentMP < uint32(def.MPCost) || stats.CurrentSP < uint32(def.SPCost) {
		s.log.Printf("Rapture World rejected skill use: not enough MP/SP skill_id=%d", k.SkillID)
		return
	}
	castTarget := target
	if def.TypeDetail == typeDetailInstantHeal || def.TypeDetail == typeDetailPeriodicHeal || def.TypeDetail == typeDetailBuff {
		castTarget = ch.ID
	}
	var pos *[2]float32
	if castTarget != ch.ID && def.TargetType != targetTypeEnemiesNearCaster {
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
	s.send(w.MustFinish(), "0x0510 skill cast started")
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
	if stats.CurrentMP < uint32(def.MPCost) || stats.CurrentSP < uint32(def.SPCost) {
		s.log.Printf("Rapture World rejected skill use: not enough MP/SP skill_id=%d", k.SkillID)
		return
	}
	switch def.TypeDetail {
	case typeDetailBuff, typeDetailPeriodicHeal:
		s.startBuff(k, def, op)
		return
	case typeDetailInstantHeal:
		s.healSkill(k, def, op, stats)
		return
	}
	var targets []uint32
	if def.TargetType == targetTypeEnemiesNearCaster {
		targets = mobsWithinRange(ch.MapID, ch.PosX, ch.PosZ, def.ApplyRange)
	} else {
		mob, ok := MobByID(ch.MapID, target)
		if !ok || mob.CurrentHP == 0 {
			return
		}
		if castPos != nil {
			if DistanceXZ(castPos[0], castPos[1], mob.PosX, mob.PosZ) > skillTargetEscapeRange {
				s.log.Printf("Rapture World rejected skill use: target escaped during cast skill_id=%d", k.SkillID)
				return
			}
		} else if d := DistanceXZ(ch.PosX, ch.PosZ, mob.PosX, mob.PosZ); d > max(float32(def.AttackRange), 1)+skillRangeGrace {
			s.log.Printf("Rapture World rejected skill use: out of range skill_id=%d distance=%.2f range=%d", k.SkillID, d, def.AttackRange)
			return
		}
		switch def.TargetType {
		case targetTypeAnyEnemy, targetTypeSelectedEnemy:
			targets = []uint32{mob.ID}
		case targetTypeEnemiesNearTarget:
			targets = mobsWithinRange(ch.MapID, mob.PosX, mob.PosZ, def.ApplyRange)
		default:
			s.log.Printf("Rapture World rejected skill use: unsupported target_type=%d skill_id=%d", def.TargetType, k.SkillID)
			return
		}
	}
	// Skills.TypeAttack 3 is magic; otherwise the weapon decides melee or
	// ranged. The skill's DamageHP adds to the attack before
	// CUser::AttackDamage, as the original seeds damage[0].
	kind := attackMelee
	if def.TypeAttack == typeAttackMagic {
		kind = attackMagic
	} else if stats.RangedWeapon {
		kind = attackRanged
	}
	extra := def.DamageHP
	s.startCooldown(k, def)
	s.spendDelta(uint32(def.MPCost), uint32(def.SPCost))
	s.persist()
	s.send(buildSkillUsedSpMp(uint32(def.SPCost), uint32(def.MPCost)), "0x050C skill used sp/mp")
	s.log.Printf("Rapture World skill use skill_id=%d level=%d target_type=%d targets=%d kind=%d", k.SkillID, k.Level, def.TargetType, len(targets), kind)
	if len(targets) == 0 {
		s.send(buildSkillUseResult(op, skillResultNormal, ch.ID, ch.ID, k.SkillID, k.Level, 0, 0, 0), "skill use (no targets hit)")
	}
	for _, t := range targets {
		s.skillHit(t, &stats, kind, extra, def, k, op)
	}
}

func mobsWithinRange(mapID uint16, x, z float32, r uint8) []uint32 {
	limit := max(float32(r), 1) + skillRangeGrace
	var out []uint32
	for _, m := range EntitiesOnMap(mapID).Mobs {
		if m.CurrentHP > 0 && DistanceXZ(x, z, m.PosX, m.PosZ) <= limit {
			out = append(out, m.ID)
		}
	}
	return out
}

func (s *Session) skillHit(target uint32, stats *CharacterStats, kind int, extra int32, def *db.SkillDef, k db.CharacterSkill, op packet.Opcode) {
	ch := s.char
	mob, ok := MobByID(ch.MapID, target)
	md := db.MustDefs().Mobs[mob.MobID]
	if !ok || md == nil {
		return
	}
	mc := mobCombatFor(md)
	SetMobAggro(ch.MapID, target, ch.ID)
	s.lastCombatAt = time.Now()
	if !playerHits(stats, &mc, kind) {
		sendNear(ch.MapID, mob.PosX, mob.PosZ, CombatRadius,
			buildSkillUseResult(op, attackResultMiss, ch.ID, mob.ID, k.SkillID, k.Level, 0, 0, 0), "skill use (miss)")
		return
	}
	damage, crit := playerDamage(stats, &mc, kind, extra, stats.AttackAttr)
	r, ok := DamageMob(ch.MapID, target, uint32(damage))
	if !ok {
		return
	}
	code := uint8(skillResultNormal)
	if crit {
		code = attackResultCritical
	}
	sendNear(ch.MapID, r.Mob.PosX, r.Mob.PosZ, CombatRadius,
		buildSkillUseResult(op, code, ch.ID, r.Mob.ID, k.SkillID, k.Level, r.DamageHP, 0, 0), "skill use")
	s.resolveMobDamage(&r)
	if r.Killed {
		return
	}
	if def.AddDamageHP > 0 {
		if x, ok := DamageMob(ch.MapID, target, uint32(def.AddDamageHP)); ok {
			w := packet.New(packet.SkillExtraDamage)
			w.U32(ch.ID)
			w.U32(x.Mob.ID)
			w.U16(k.SkillID)
			w.U8(k.Level)
			w.U16(x.DamageHP)
			w.U16(0)
			w.U16(0)
			sendNear(ch.MapID, x.Mob.PosX, x.Mob.PosZ, CombatRadius, w.MustFinish(), "0x051A skill extra damage")
			s.resolveMobDamage(&x)
		}
	}
	if def.TimeDamageHP > 0 || def.TimeDamageSP > 0 || def.TimeDamageMP > 0 {
		ApplyDotToMob(ch.MapID, target, ch.ID, k.SkillID, k.Level, uint32(max(def.TimeDamageHP, 0)), time.Duration(def.KeepTime)*time.Second)
		sendNear(ch.MapID, r.Mob.PosX, r.Mob.PosZ, CombatRadius,
			buildMobDebuff(packet.MobDebuffAdd, MobDebuffTypeDOT, target, k.SkillID, k.Level), "0x030C mob debuff add")
		s.log.Printf("Rapture World skill dot started skill_id=%d mob_global_id=%d keep_time=%d", k.SkillID, target, def.KeepTime)
	}
}

func (s *Session) healSkill(k db.CharacterSkill, def *db.SkillDef, op packet.Opcode, stats CharacterStats) {
	ch := s.char
	s.startCooldown(k, def)
	heal := uint32(max(def.HealHP, 0))
	if def.TypeAttack == typeAttackMagic {
		heal = satAdd(heal, satMul(stats.Wis, 2))
	}
	hp, mp, sp := s.healDelta(heal, uint32(max(def.HealMP, 0)), uint32(max(def.HealSP, 0)))
	s.spendDelta(uint32(def.MPCost), uint32(def.SPCost))
	s.persist()
	s.send(buildSkillUsedSpMp(uint32(def.SPCost), uint32(def.MPCost)), "0x050C skill used sp/mp")
	s.log.Printf("Rapture World skill heal skill_id=%d hp=%d mp=%d sp=%d", k.SkillID, hp, sp, mp)
	s.send(buildSkillUseResult(op, skillResultNormal, ch.ID, ch.ID, k.SkillID, k.Level, 0, 0, 0), "skill heal")
	st := s.stats()
	s.send(buildCurrentHitpoints(&st), "0x0521 current hp/mp/sp after heal")
}

func (s *Session) startBuff(k db.CharacterSkill, def *db.SkillDef, op packet.Opcode) {
	ch := s.char
	s.startCooldown(k, def)
	s.spendDelta(uint32(def.MPCost), uint32(def.SPCost))
	s.persist()
	s.send(buildSkillUsedSpMp(uint32(def.SPCost), uint32(def.MPCost)), "0x050C skill used sp/mp")
	id := uint32(0)
	kept := s.buffs[:0]
	for _, b := range s.buffs {
		if b.SkillID == k.SkillID {
			id = b.ID
			continue
		}
		kept = append(kept, b)
	}
	s.buffs = kept
	if id == 0 {
		for _, b := range s.buffs {
			id = max(id, b.ID)
		}
		id++
	}
	s.buffs = append(s.buffs, newBuff(id, def, time.Now().Add(time.Duration(def.KeepTime)*time.Second), s.log))
	s.log.Printf("Rapture World skill buff started skill_id=%d level=%d keep_time=%d", k.SkillID, k.Level, def.KeepTime)
	s.send(buildSkillUseResult(op, skillResultNormal, ch.ID, ch.ID, k.SkillID, k.Level, 0, 0, 0), "skill buff cast")
	w := packet.New(packet.BuffAdd)
	w.U32(id)
	w.U16(k.SkillID)
	w.U8(k.Level)
	w.U32(uint32(def.KeepTime))
	s.send(w.MustFinish(), "0x050D buff add")
	s.sendBuffStats("after buff")
}

func (s *Session) sendBuffStats(why string) {
	st := s.stats()
	s.send(buildAdditionalStats(&st), "0x0526 additional stats "+why)
	s.send(buildMaxHPMPSP(s.char, &st), "0x051F max hp/mp/sp "+why)
	s.send(buildCurrentHitpoints(&st), "0x0521 current hp/mp/sp "+why)
}

// processBuffs ticks heal-over-time and expires buffs.
func (s *Session) processBuffs() {
	if len(s.buffs) == 0 || s.char == nil {
		return
	}
	now := time.Now()
	var removed []uint32
	for i := 0; i < len(s.buffs); {
		b := &s.buffs[i]
		expiring := !b.EndsAt.After(now)
		hasHeal := b.HealHP+b.HealSP+b.HealMP > 0
		due := !b.nextTickAt.IsZero() && !b.nextTickAt.After(now)
		if due || (expiring && hasHeal) {
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
		if expiring {
			removed = append(removed, b.ID)
			s.log.Printf("Rapture World skill buff expired skill_id=%d level=%d", b.SkillID, b.SkillLevel)
			s.buffs = append(s.buffs[:i], s.buffs[i+1:]...)
		} else {
			i++
		}
	}
	if len(removed) == 0 {
		return
	}
	for _, id := range removed {
		w := packet.New(packet.BuffRemove)
		w.U32(id)
		s.send(w.MustFinish(), "0x050E buff remove")
	}
	s.sendBuffStats("after buff expiry")
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
		if def == nil || (def.TypeDetail != typeDetailBuff && def.TypeDetail != typeDetailPeriodicHeal) {
			continue
		}
		out = append(out, newBuff(uint32(len(out)+1), def, time.Now().Add(time.Duration(r.LeftSeconds)*time.Second), log))
	}
	return out
}
