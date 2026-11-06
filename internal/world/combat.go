package world

import (
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
)

const (
	weaponSlot            = 5
	autoAttackRangeGrace  = 2.0
	autoAttackCancelRange = 80.0
	attackSuccessNormal   = 0
	deathTypeMob          = 0x02
)

type weaponCombat struct {
	ItemType, TypeID uint8
	AttackTime       uint8
	Range            float32
	OriginalDelay    time.Duration
	Delay            time.Duration
}

// weaponAttackDelay maps Items.AttackTime (1-10) to the native delay and
// Rapture's accelerated test delay.
func weaponAttackDelay(attackTime uint8) (time.Duration, time.Duration, bool) {
	table := [11][2]int{{}, {4000, 1500}, {3000, 1500}, {2750, 1375}, {2500, 1250}, {2250, 1125},
		{2000, 1000}, {1750, 875}, {1500, 750}, {1250, 625}, {1000, 500}}
	if attackTime < 1 || attackTime > 10 {
		return 0, 0, false
	}
	return time.Duration(table[attackTime][0]) * time.Millisecond, time.Duration(table[attackTime][1]) * time.Millisecond, true
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
	orig, delay, ok := weaponAttackDelay(at)
	if !ok {
		return weaponCombat{}, false
	}
	r := float32(d.Range)
	if r <= 0 {
		r = 2
	}
	return weaponCombat{d.Type, d.TypeID, at, r, orig, delay}, true
}

func basicAutoAttackDamage(s CharacterStats) uint32 {
	lo, hi := min(s.MinAttack, s.MaxAttack), max(s.MinAttack, s.MaxAttack)
	return max(satAdd(lo, hi)/2, 1)
}

func basicMagicAttackDamage(s CharacterStats) uint32 {
	lo, hi := min(s.MinMagic, s.MaxMagic), max(s.MinMagic, s.MaxMagic)
	return max(satAdd(lo, hi)/2, 1)
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
	result, ok := DamageMob(ch.MapID, target, basicAutoAttackDamage(stats))
	if !ok {
		return false
	}
	SetMobAggro(ch.MapID, target, ch.ID)
	s.lastAutoAttackAt = time.Now()
	s.log.Printf("Rapture World auto attack character_id=%d mob_global_id=%d mob_id=%d atk=%d-%d delay=%s range=%.2f distance=%.2f damage=%d hp=%d/%d killed=%t",
		ch.ID, result.Mob.ID, result.Mob.MobID, stats.MinAttack, stats.MaxAttack, wc.Delay, wc.Range, distance,
		result.DamageHP, result.Mob.CurrentHP, result.Mob.MaxHP, result.Killed)
	sendNear(ch.MapID, result.Mob.PosX, result.Mob.PosZ, CombatRadius,
		buildCharacterMobAttack(ch.ID, result.Mob.ID, attackSuccessNormal, result.DamageHP), "0x0503 character mob auto attack")
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
	for _, p := range rollMobDrops(ch.MapID, m.MobID, m.MoneyMin, m.MoneyMax, ch.ID, m.PosX, ch.PosY, m.PosZ, s.logger) {
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
	var damage uint32
	if d.Attack > 0 {
		damage = max(satSub(d.Attack, satAdd(stats.Defense, stats.Absorption)), 1)
	}
	newEffective := satSub(stats.CurrentHP, damage)
	ch.CurrentHP = satSub(ch.CurrentHP, stats.CurrentHP-newEffective)
	killed := damage > 0 && newEffective == 0
	if killed {
		ch.Deaths++
	}
	s.persist()
	result := uint8(0)
	if killed {
		result = 1
	}
	sendNear(ch.MapID, ch.PosX, ch.PosZ, CombatRadius,
		buildMobAttackCharacter(result, d.MobGlobalID, ch.ID, uint16(min(damage, 65535))), "0x0605 mob attack character")
	s.log.Printf("Rapture World mob attack character_id=%d mob_global_id=%d mob_id=%d raw_attack=%d defense=%d absorption=%d damage=%d hp=%d/%d killed=%t",
		ch.ID, d.MobGlobalID, d.MobID, d.Attack, stats.Defense, stats.Absorption, damage, newEffective, stats.MaxHP, killed)
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

// LevelCap is the highest level exposed; ExpDefs has rows up to 80.
const LevelCap = 60

const expGainRatePercent = 100

// scaledKillExp scales Mobs.Exp by the level difference (zero beyond -8/+6).
func scaledKillExp(charLevel, mobLevel uint16, mobExp uint32) uint32 {
	diff := int64(charLevel) - int64(mobLevel)
	if diff < -8 || diff > 6 {
		return 0
	}
	v := int64(mobExp) * (-24*diff + 167) / 100 * expGainRatePercent / 100
	return uint32(min(max(v, 0), int64(^uint32(0))))
}

// applyExpGain adds exp (Chars.Exp is progress within the level) and
// advances levels per ExpDefs; a level-up refreshes the base pools and
// heals fully. Base stats do not grow with level by design.
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
		ch.Exp -= req
		ch.Level++
		levels++
	}
	if levels > 0 {
		base := basePoolForLevel(ch.Level)
		ch.MaxHP, ch.MaxMP, ch.MaxSP = base, base, base
		ch.CurrentHP, ch.CurrentMP, ch.CurrentSP = base, base, base
	}
	return levels
}

func (s *Session) grantKillExp(mobID, mobLevel uint16, mobExp uint32) {
	ch := s.char
	if ch == nil {
		return
	}
	scaled := scaledKillExp(ch.Level, mobLevel, mobExp)
	levels := applyExpGain(ch, scaled)
	s.persist()
	s.log.Printf("Rapture World exp character_id=%d mob_id=%d mob_level=%d base_exp=%d gained=%d level=%d exp=%d levels_gained=%d",
		ch.ID, mobID, mobLevel, mobExp, scaled, ch.Level, ch.Exp, levels)
	s.sendStatPackets()
}

// grantOfflineKillExp credits a DoT kill to a caster who went offline.
func grantOfflineKillExp(characterID uint32, mobID, mobLevel uint16, mobExp uint32, log logPrinter) {
	ch, err := db.CharacterByID(characterID)
	if err != nil {
		return
	}
	scaled := scaledKillExp(ch.Level, mobLevel, mobExp)
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
