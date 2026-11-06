package world

import (
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

const (
	weaponSlot            = 5
	autoAttackRangeGrace  = 2.0
	autoAttackCancelRange = 80.0
	attackSuccessNormal   = 0
	attackResultCritical  = 1
	attackResultMiss      = 3
	deathTypeMob          = 0x02
)

type weaponCombat struct {
	ItemType, TypeID uint8
	AttackTime       uint8
	Range            float32
	Delay            time.Duration
}

// weaponAttackDelay maps Items.AttackTime (1-10) to the original swing
// delay.
func weaponAttackDelay(attackTime uint8) (time.Duration, bool) {
	table := [11]int{0, 4000, 3000, 2750, 2500, 2250, 2000, 1750, 1500, 1250, 1000}
	if attackTime < 1 || attackTime > 10 {
		return 0, false
	}
	return time.Duration(table[attackTime]) * time.Millisecond, true
}

func equippedWeaponCombat(items []db.Item) (weaponCombat, bool) {
	w := equippedItem(items, weaponSlot)
	if w == nil {
		return weaponCombat{}, false
	}
	d := db.MustDefs().Item(w.Type, w.TypeID)
	if d == nil {
		return weaponCombat{}, false
	}
	at := min(d.AttackTime, 10)
	delay, ok := weaponAttackDelay(at)
	if !ok {
		return weaponCombat{}, false
	}
	r := float32(d.Range)
	if r <= 0 {
		r = 2
	}
	return weaponCombat{d.Type, d.TypeID, at, r, delay}, true
}

func sendNear(mapID uint16, x, z, radius float32, frame []byte, label string) {
	for _, p := range PlayersNear(mapID, x, z, radius) {
		p.Send(frame, label)
	}
}

// autoAttackSwing resolves one swing; false cancels the auto-attack.
func (s *Session) autoAttackSwing(target uint32) bool {
	ch := s.char
	if ch == nil {
		return false
	}
	if s.mounted {
		s.log.Printf("Rapture World cancelled auto attack while mounted character_id=%d mob_global_id=%d", ch.ID, target)
		return false
	}
	wc, ok := equippedWeaponCombat(s.items)
	if !ok {
		s.log.Printf("Rapture World cancelled auto attack without valid weapon character_id=%d mob_global_id=%d", ch.ID, target)
		return false
	}
	if !s.lastAutoAttackAt.IsZero() && time.Since(s.lastAutoAttackAt) < wc.Delay {
		return true
	}
	mob, ok := MobByID(ch.MapID, target)
	if !ok || mob.CurrentHP == 0 {
		s.log.Printf("Rapture World cancelled auto attack unknown or dead mob character_id=%d mob_global_id=%d", ch.ID, target)
		return false
	}
	distance := DistanceXZ(ch.PosX, ch.PosZ, mob.PosX, mob.PosZ)
	if distance > wc.Range+autoAttackRangeGrace {
		if distance > autoAttackCancelRange {
			s.log.Printf("Rapture World cancelled auto attack target too far character_id=%d mob_global_id=%d distance=%.2f", ch.ID, target, distance)
			return false
		}
		return true
	}
	stats := StatsFor(ch, s.equipment())
	def := db.MustDefs().Mobs[mob.MobID]
	if def == nil {
		return false
	}
	mc := mobCombatFor(def)
	kind := attackMelee
	if stats.RangedWeapon {
		kind = attackRanged
	}
	s.lastAutoAttackAt, s.lastCombatAt = time.Now(), time.Now()
	SetMobAggro(ch.MapID, target, ch.ID)
	// CUser::AttackSuccess, then CUser::AttackDamage; 0x0503 result is
	// 0 normal, 1 critical, 3 miss.
	if !playerHits(&stats, &mc, kind) {
		sendNear(ch.MapID, mob.PosX, mob.PosZ, CombatRadius,
			buildCharacterMobAttack(ch.ID, mob.ID, attackResultMiss, 0), "0x0503 character mob auto attack (miss)")
		return true
	}
	damage, crit := playerDamage(&stats, &mc, kind, 0, stats.AttackAttr)
	result, ok := DamageMob(ch.MapID, target, uint32(damage))
	if !ok {
		return false
	}
	code := uint8(attackSuccessNormal)
	if crit {
		code = attackResultCritical
	}
	s.log.Printf("Rapture World auto attack mob_global_id=%d mob_id=%d kind=%d damage=%d crit=%t hp=%d/%d killed=%t",
		result.Mob.ID, result.Mob.MobID, kind, result.DamageHP, crit, result.Mob.CurrentHP, result.Mob.MaxHP, result.Killed)
	sendNear(ch.MapID, result.Mob.PosX, result.Mob.PosZ, CombatRadius,
		buildCharacterMobAttack(ch.ID, result.Mob.ID, code, result.DamageHP), "0x0503 character mob auto attack")
	return s.resolveMobDamage(&result)
}

// resolveMobDamage handles the aftermath of any damage to a mob: death,
// drops and exp, or an HP sync. Returns whether the mob is still alive.
func (s *Session) resolveMobDamage(r *MobDamageResult) bool {
	ch := s.char
	m := &r.Mob
	if !r.Killed {
		sendNear(ch.MapID, m.PosX, m.PosZ, CombatRadius, buildMobCurrentHP(m.ID, m.CurrentHP), "0x0610 mob current hp after attack")
		s.send(buildMobTargetHP(m), "0x0305 mob target hp after attack")
		return true
	}
	sendNear(ch.MapID, m.PosX, m.PosZ, CombatRadius, buildMobDeath(m.ID, ch.ID), "0x0606 mob death")
	// The killer's live Y tracks terrain better than the svmap area average.
	for _, p := range rollMobDrops(ch.MapID, m, ch.ID, ch.PosY, s.logger) {
		sendNear(ch.MapID, m.PosX, m.PosZ, CombatRadius, p, "0x0401 mob drop")
	}
	if m.Exp > 0 {
		s.grantKillExp(m.MobID, m.Level, m.Exp)
	}
	return false
}

// applyIncomingMobDamage applies one queued mob hit; true when it killed.
func (s *Session) applyIncomingMobDamage(d pendingDamage) bool {
	ch := s.char
	if ch == nil {
		return false
	}
	equip := s.equipment()
	stats := StatsFor(ch, equip)
	if stats.CurrentHP == 0 {
		return false
	}
	s.lastCombatAt = time.Now()
	def := db.MustDefs().Mobs[d.MobID]
	if def == nil {
		return false
	}
	// CMob::AttackSuccess / CMob::AttackDamage with the first attack slot.
	mc := mobCombatFor(def)
	slot := &def.Attacks[0]
	kind := int(min(slot.Type, 2))
	var damage uint32
	result := uint8(attackSuccessNormal)
	if !mobHits(&mc, &stats, kind) {
		result = attackResultMiss
	} else {
		v, crit := mobDamage(&mc, slot, &stats)
		damage = uint32(max(v, 0))
		if stats.Absorption > 0 {
			damage = max(satSub(damage, stats.Absorption), 1)
		}
		if crit {
			result = attackResultCritical
		}
	}
	newEffective := satSub(stats.CurrentHP, damage)
	ch.CurrentHP = satSub(ch.CurrentHP, stats.CurrentHP-newEffective)
	killed := damage > 0 && newEffective == 0
	if killed {
		ch.Deaths++
	}
	s.persist()
	sendNear(ch.MapID, ch.PosX, ch.PosZ, CombatRadius,
		buildMobAttackCharacter(result, d.MobGlobalID, ch.ID, uint16(min(damage, 65535))), "0x0605 mob attack character")
	s.log.Printf("Rapture World mob attack mob_global_id=%d mob_id=%d result=%d damage=%d hp=%d/%d killed=%t",
		d.MobGlobalID, d.MobID, result, damage, newEffective, stats.MaxHP, killed)
	refreshed := StatsFor(ch, equip)
	s.send(buildCurrentHitpoints(&refreshed), "0x0521 current hp after mob attack")
	if killed {
		state, death := buildCharacterDeathState(ch.ID), buildCharacterDeath(ch.ID, deathTypeMob, d.MobGlobalID)
		for _, p := range PlayersNear(ch.MapID, ch.PosX, ch.PosZ, CombatRadius) {
			p.Send(state, "0x051C character death state")
			p.Send(death, "0x0504 character death")
		}
		ClearMobAggro(ch.MapID, d.MobGlobalID)
		if s.player != nil {
			s.player.SetAlive(false)
		}
		s.log.Printf("Rapture World character died character_id=%d killer_mob_global_id=%d mob_id=%d", ch.ID, d.MobGlobalID, d.MobID)
	}
	return killed
}

// persist saves the active character (usp_Save_Char_Info_E) and refreshes
// the shared snapshot.
func (s *Session) persist() {
	if s.char == nil {
		return
	}
	if err := db.SaveCharacter(s.char); err != nil {
		s.log.Printf("Rapture World failed to save character_id=%d: %v", s.char.ID, err)
	}
	s.syncPlayer()
}

// --- experience ---

// LevelCap is the last level in StatusDefs and ExpDefs.
const LevelCap = 60

// applyExpGain adds exp. Chars.Exp is the cumulative total and ExpDefs
// gives, per level and growth mode, the total needed to reach the next
// level, as in the original server. A level-up refreshes the base pools
// from StatusDefs and heals fully.
func applyExpGain(ch *db.Character, gained uint32) (levels uint16) {
	if ch.Level >= LevelCap || gained == 0 {
		return 0
	}
	ch.Exp = satAdd(ch.Exp, gained)
	exp := db.MustDefs().Exp
	for ch.Level < LevelCap {
		req := exp[db.ExpKey{Level: ch.Level, Grow: ch.Mode + 1}]
		if req == 0 || ch.Exp < req {
			break
		}
		levelUp(ch)
		levels++
	}
	if levels > 0 {
		ch.MaxHP, ch.MaxMP, ch.MaxSP = basePools(ch)
		ch.CurrentHP, ch.CurrentMP, ch.CurrentSP = ch.MaxHP, ch.MaxMP, ch.MaxSP
	}
	return levels
}

// expBounds are the cumulative exp at the start of the character's level
// and the total needed for the next one (ExpDefs).
func expBounds(ch *db.Character) (prev, next uint32) {
	exp := db.MustDefs().Exp
	grow := ch.Mode + 1
	if ch.Level > 1 {
		prev = exp[db.ExpKey{Level: ch.Level - 1, Grow: grow}]
	}
	return prev, exp[db.ExpKey{Level: ch.Level, Grow: grow}]
}

func (s *Session) grantKillExp(mobID, mobLevel uint16, mobExp uint32) {
	ch := s.char
	if ch == nil {
		return
	}
	gained := mobExperience(mobLevel, uint16(min(mobExp, 65535)), ch.Level)
	levels := applyExpGain(ch, gained)
	s.persist()
	s.log.Printf("Rapture World exp character_id=%d mob_id=%d mob_level=%d base_exp=%d gained=%d level=%d exp=%d levels_gained=%d",
		ch.ID, mobID, mobLevel, mobExp, gained, ch.Level, ch.Exp, levels)
	if gained > 0 {
		s.send(buildExpGain(gained), "0x0207 exp gained")
	}
	if levels > 0 {
		// The level-up notice goes to the character (0x0508) and to the
		// players around it (0x051E), within 60 units.
		s.send(buildLevelUp(packet.LevelUpSelf, ch), "0x0508 level up")
		others := buildLevelUp(packet.LevelUpOther, ch)
		for _, o := range PlayersNearExcluding(ch.MapID, ch.ID, ch.PosX, ch.PosZ, 60) {
			o.Send(others, "0x051E level up (nearby)")
		}
		s.sendStatPackets()
	}
}

// buildExpGain (0x0207): exp gained and a second word the original sends
// as 0.
func buildExpGain(amount uint32) []byte {
	w := packet.New(packet.ExpGain)
	w.U32(amount)
	w.U32(0)
	return w.MustFinish()
}

// buildLevelUp (0x0508 self / 0x051E others): character id, level, stat
// points, skill points, exp at the level's start and for the next level.
func buildLevelUp(op packet.Opcode, ch *db.Character) []byte {
	prev, next := expBounds(ch)
	w := packet.New(op)
	w.U32(ch.ID)
	w.U16(ch.Level)
	w.U16(ch.StatPoint)
	w.U16(ch.SkillPoint)
	w.U32(prev)
	w.U32(next)
	return w.MustFinish()
}

// grantOfflineKillExp credits a DoT kill to a caster who went offline.
func grantOfflineKillExp(characterID uint32, mobID, mobLevel uint16, mobExp uint32, log logPrinter) {
	ch, err := db.CharacterByID(characterID)
	if err != nil {
		return
	}
	scaled := mobExperience(mobLevel, uint16(min(mobExp, 65535)), ch.Level)
	applyExpGain(&ch, scaled)
	if err := db.SaveCharacter(&ch); err != nil {
		log.Printf("Rapture World offline exp save failed character_id=%d: %v", characterID, err)
		return
	}
	log.Printf("Rapture World offline exp character_id=%d mob_id=%d gained=%d level=%d exp=%d", characterID, mobID, scaled, ch.Level, ch.Exp)
}

type logPrinter interface {
	Printf(format string, args ...any)
}
