package world

import (
	"math"
	"math/rand/v2"

	"github.com/Spelunkern/Rapture/internal/db"
)

// Hit chance and damage as ps_game.exe computes them (CUser/CMob
// ::AttackSuccess and ::AttackDamage, recovered by Eternity's
// vertical_combat_success / vertical_combat_damage / vertical_mobs).
// Categories: 0 melee, 1 ranged, 2 magic.

const (
	attackMelee  = 0
	attackRanged = 1
	attackMagic  = 2
	maxDamage    = 30000
)

// attackElement is CGameData::m_AttackAttr (0x00542260): attacker element
// by defender element.
var attackElement = [9][9]float32{
	{1.0, 0.8, 0.8, 0.8, 0.8, 0.7, 0.7, 0.7, 0.7},
	{1.2, 1.0, 0.5, 1.0, 1.4, 1.0, 0.4, 1.0, 1.3},
	{1.2, 1.4, 1.0, 0.5, 1.0, 1.3, 1.0, 0.4, 1.0},
	{1.2, 1.0, 1.4, 1.0, 0.5, 1.0, 1.3, 1.0, 0.4},
	{1.2, 0.5, 1.0, 1.4, 1.0, 0.4, 1.0, 1.3, 1.0},
	{1.3, 1.0, 0.5, 1.0, 1.6, 1.0, 0.5, 1.0, 1.4},
	{1.3, 1.6, 1.0, 0.5, 1.0, 1.4, 1.0, 0.5, 1.0},
	{1.3, 1.0, 1.6, 1.0, 0.5, 1.0, 1.4, 1.0, 0.5},
	{1.3, 0.5, 1.0, 1.6, 1.0, 0.5, 1.0, 1.4, 1.0},
}

func f32(v int32) float64     { return float64(float32(v)) }
func store(v float64) float32 { return float32(v) }

// truncInt is the original FISTP truncation (INT_MIN on overflow).
func truncInt(v float64) int32 {
	v = math.Trunc(v)
	if math.IsNaN(v) || math.IsInf(v, 0) || v < math.MinInt32 || v > math.MaxInt32 {
		return math.MinInt32
	}
	return int32(v)
}

func clampInt(v, lo, hi int32) int32 { return min(max(v, lo), hi) }

// setAttack is CUser::SetAttack (0x00454280) on top of the equipment and
// the skill/passive additions per category.
func setAttack(s *CharacterStats, e EquipmentStats) {
	dex, str, wis, intel, luck := float32(s.Dex), float32(s.Str), float32(s.Wis), float32(s.Int), float32(s.Luc)
	halfDex := dex * 0.5
	defenseBase := luck*0.125 + halfDex
	fifthDex := dex * 0.2
	item := float32(e.ItemAttack)
	level := float32(s.Level)
	s.Critical = truncInt(float64(luck*0.19 + float32(e.Critical)))
	for c := 0; c < 2; c++ {
		m := e.Mods[c]
		s.Accuracy[c] = truncInt(float64(float32(m.Accuracy) + level + halfDex))
		s.Evasion[c] = truncInt(float64(float32(m.Evasion) + defenseBase + level))
		s.Guard[c] = m.Defense + int32(s.Rec) + int32(e.Defense)
	}
	s.Attack[attackMelee] = truncInt(float64(str*1.3 + float32(e.Mods[0].Attack) + item + fifthDex))
	s.Attack[attackRanged] = truncInt(float64(luck*0.3 + float32(e.Mods[1].Attack) + item + str + fifthDex))
	magic := e.Mods[2]
	s.Attack[attackMagic] = truncInt(float64(intel*1.3 + wis*0.2 + float32(magic.Attack) + item))
	s.Accuracy[attackMagic], s.Evasion[attackMagic] = magic.Accuracy, magic.Evasion
	s.Guard[attackMagic] = magic.Defense + int32(e.Resistance) + int32(s.Wis)
	s.Variance, s.AttackAttr, s.RangedWeapon = e.Variance, e.AttackAttr, e.RangedWeapon
	s.DefenseAttr, s.CrossRoad, s.Range, s.AttackSpeed = e.DefenseAttr, e.CrossRoad, e.Range, e.AttackSpeed
	s.SPCostCut, s.MPCostCut = e.SPCostCut, e.MPCostCut

	physical := attackMelee
	if e.RangedWeapon {
		physical = attackRanged
	}
	s.MinAttack = uint32(max(s.Attack[physical], 0))
	s.MaxAttack = s.MinAttack + e.Variance
	s.MinMagic = uint32(max(s.Attack[attackMagic], 0))
	s.MaxMagic = s.MinMagic + e.Variance
	s.Defense = uint32(max(s.Guard[attackMelee], 0))
	s.Resistance = uint32(max(s.Guard[attackMagic], 0))
}

// mobCombat is a mob's derived combat values (rebuild_mob_attack).
type mobCombat struct {
	Level    int32
	Accuracy [3]int32
	Evasion  [3]int32
	Guard    [3]int32
	Attack   [3]int32
	Critical int32
	Luck     int32
	Wis      int32
	Dex      int32
	Attrib   uint8
}

func mobCombatFor(d *db.MobDef) mobCombat {
	dex, level, luck := float32(d.Dex), float32(d.Level), float32(d.Luc)
	var m mobCombat
	m.Level, m.Luck, m.Wis, m.Dex, m.Attrib = int32(d.Level), int32(d.Luc), int32(d.Wis), int32(d.Dex), d.Attrib
	for c := 0; c < 2; c++ {
		m.Accuracy[c] = truncInt(float64(dex*0.5 + level))
	}
	m.Evasion[0] = truncInt(float64(dex*0.5 + level/2 + luck*0.125))
	m.Evasion[1] = truncInt(float64(dex*0.5 + luck*0.125 + level))
	m.Guard[0] = int32(d.Defense)
	m.Guard[1] = int32(d.ShieldDefense) + int32(d.Defense)
	m.Attack[2] = int32(d.Wis) + int32(d.Level)
	m.Guard[2] = int32(d.Magic)
	m.Critical = truncInt(float64(luck * 0.19))
	return m
}

// magicResisted is the magic branch shared by both directions: a miss when
// the roll is under the level/dex resistance chance (1..90).
// The stat term reads WIS on both sides (CUser +0x122c, stMOBInfo +0x3a).
func magicResisted(attLevel, defLevel, defEvasion, attDex, defDex, attAccuracy int32) bool {
	denominator := float64(float32(attLevel + defLevel))
	levelTerm := float64(float32((f32(defLevel-attLevel-2)*100+f32(defLevel))/denominator*float64(float32(1.1)) + f32(defEvasion)))
	numerator := defDex*11 - attDex*10
	dexTerm := float64(float32(f32(numerator)/f32(attDex+defDex)*float64(float32(3.9)) + f32(attAccuracy)))
	clamp := func(v float64) float64 { return min(max(v, 1), 70) }
	chance := clampInt(truncInt(clamp(levelTerm)+clamp(dexTerm)), 1, 90)
	return rand.Int32N(100) < chance
}

// playerHits is CUser::AttackSuccess for a normal attack on a mob.
func playerHits(s *CharacterStats, m *mobCombat, kind int) bool {
	if kind == attackMagic {
		return !magicResisted(int32(s.Level), m.Level, m.Evasion[2], int32(s.Wis), m.Wis, s.Accuracy[2])
	}
	level := float64(float32(s.Level))
	levels := float64(float32(int32(s.Level) + m.Level))
	levelPart := level / levels * 160
	defense := f32(m.Accuracy[kind]) / f32(s.Evasion[kind]+m.Accuracy[kind]) * 100
	accuracy := f32(s.Accuracy[kind]) / f32(m.Evasion[kind]+s.Accuracy[kind]) * 100
	chance := clampInt(truncInt(levelPart-(defense-accuracy)), 20, 99)
	return rand.Int32N(100) < chance
}

// closeRangePenalty is the bow rule in CUser::AttackDamage: a ranged hit
// within 3 units does 70%, unless a cross-road passive (detail 9) is learned.
const closeRangeDistance = 3.0

// weaponDamage is the weapon branch of CUser::AttackDamage (0x0044d540)
// against a mob. damage[0] seeds the attack (skill modes 1 and 2); element
// is the attacking element and defElement the mob's (0 for detail 24).
func weaponDamage(s *CharacterStats, m *mobCombat, kind int, damage *[3]int32, element, defElement uint8, closeRanged bool) bool {
	levelRaw := store((f32(m.Level-int32(s.Level)-1)*100 + f32(m.Level)) / 100 * 10)
	level := float64(min(max(levelRaw, 1), 95))
	attack := store(f32(int32(uint32(s.Attack[kind]) + uint32(damage[0]))))
	if s.Variance > 0 {
		attack = store(float64(attack) + f32(int32(rand.Uint32N(s.Variance))))
	}
	penalty := kind == attackRanged && closeRanged && s.CrossRoad == 0
	if penalty {
		attack = store(float64(attack) * float64(float32(0.7)))
	}
	factor := float32(1)
	if element < 9 && defElement < 9 {
		factor = attackElement[element][defElement]
	}
	// The two random terms read the attacker's LUC (CUser +0x1234; the
	// same field SetAttack uses for luck).
	luckRoll := func() int32 {
		if s.Luc == 0 {
			return 0
		}
		return int32(rand.Uint32N(s.Luc)) + 1
	}
	first := luckRoll()
	defense := f32(m.Guard[kind])
	attackPart := (float64(attack) - float64(attack)/100*level) * float64(factor)
	defensePart := defense + defense/100*level
	value := store(attackPart - defensePart + f32(first)*float64(float32(0.1)))
	if value < 0 {
		value = 0
	}
	critChance := clampInt(truncInt(f32(s.Critical)/100*100-f32(m.Luck)*float64(float32(0.034))), 5, 99)
	crit := rand.Int32N(100) < critChance
	if crit {
		bonus := store(f32(luckRoll()) * float64(float32(1.4)))
		value = store(float64(value)*1.5 + float64(bonus) - float64(bonus)/100*level)
		if penalty {
			value = store(float64(value) * float64(float32(0.7)))
		}
	}
	damage[0] = truncInt(float64(value))
	return crit
}

// playerDamage is CUser::AttackDamage for a normal attack. Returns the
// HP damage and whether it crit.
func playerDamage(s *CharacterStats, m *mobCombat, kind int, element uint8, closeRanged bool) (int32, bool) {
	var damage [3]int32
	crit := weaponDamage(s, m, kind, &damage, element, m.Attrib, closeRanged)
	return damageLimits(damage[0]), crit
}

// skillTarget is what a skill's damage modes read from the mob.
type skillTarget struct {
	MaxHP, MaxMP, MaxSP uint32
	HP, MP, SP          uint32
}

// playerSkillDamage is CUser::AttackDamage with a skill: DamageType 0 is the
// raw DamageHP/SP/MP, 1 adds them to the weapon attack, 2 multiplies the
// weapon damage by them, 3 multiplies the caster's own HP/SP/MP, 4 and 5 are
// a percentage of the mob's maximum or current HP/MP/SP. The element is the
// skill's AttrType when set; detail 24 ignores the mob's element.
func playerSkillDamage(s *CharacterStats, m *mobCombat, def *db.SkillDef, t skillTarget, caster [3]uint32, closeRanged bool) ([3]int32, bool) {
	values := [3]int32{def.DamageHP, def.DamageSP, def.DamageMP}
	for i := range values {
		values[i] = int32(uint16(values[i]))
	}
	var damage [3]int32
	capAll := func() {
		for i := range damage {
			damage[i] = min(damage[i], maxDamage)
		}
	}
	switch def.DamageType {
	case 0:
		return values, false
	case 3:
		for i, v := range values {
			if v != 0 {
				damage[i] = max(int32(caster[i]*uint32(v)), 1)
			}
		}
		capAll()
		return damage, false
	case 4, 5:
		source := [3]uint32{t.MaxHP, t.MaxMP, t.MaxSP}
		if def.DamageType == 5 {
			source = [3]uint32{t.HP, t.MP, t.SP}
		}
		for i, v := range values {
			if v != 0 {
				product := math.Trunc(f32(v) / 100 * float64(float32(source[i])))
				damage[i] = max(int32(uint32(int64(product))), 1)
			}
		}
		capAll()
		return damage, false
	case 1:
		damage = values
	}
	element := s.AttackAttr
	if def.AttrType != 0 {
		element = def.AttrType
	}
	defElement := m.Attrib
	if def.TypeDetail == 24 {
		defElement = 0
	}
	crit := weaponDamage(s, m, int(def.Kind), &damage, element, defElement, closeRanged)
	if def.DamageType == 2 {
		for i, v := range values {
			if v != 0 {
				damage[i] = int32(uint32(damage[i]) * uint32(v))
			}
		}
	}
	capAll()
	if damage[0] < 11 {
		damage[0] = rand.Int32N(10) + 1
	}
	return damage, crit
}

// playerSkillHits is CUser::AttackSuccess with a skill: mobs resist a
// skill's StateType or its ID outright; SuccessType 1-3 roll SuccessValue
// (2 and 3 add WIS or LUC against the mob's, as an integer ratio);
// otherwise the normal hit chance of the skill's category applies.
func playerSkillHits(s *CharacterStats, m *mobCombat, md *db.MobDef, def *db.SkillDef) bool {
	if def.StateType != 0 && md.ResistState[def.StateType-1] != 0 {
		return false
	}
	for _, id := range md.ResistSkill {
		if id == def.SkillID {
			return false
		}
	}
	if def.SuccessType >= 1 && def.SuccessType <= 3 {
		chance := int32(def.SuccessValue)
		if def.SuccessType != 1 {
			mine, theirs := int32(s.Wis), int32(md.Wis)
			if def.SuccessType == 3 {
				mine, theirs = int32(s.Luc), int32(md.Luc)
			}
			if mine+theirs != 0 {
				chance += mine / (mine + theirs) * 100
			}
		}
		return rand.Int32N(100) < chance
	}
	return playerHits(s, m, int(def.Kind))
}

func damageLimits(v int32) int32 {
	v = min(v, maxDamage)
	if v < 11 {
		v = rand.Int32N(10) + 1
	}
	return v
}

// mobHits is CMob::AttackSuccess against a player.
func mobHits(m *mobCombat, s *CharacterStats, kind int) bool {
	if kind == attackMagic {
		return !magicResisted(m.Level, int32(s.Level), s.Evasion[2], m.Wis, int32(s.Wis), m.Accuracy[2])
	}
	level := float64(float32(m.Level))
	levels := float64(float32(int32(s.Level) + m.Level))
	levelPart := level / levels * 160
	defense := f32(s.Accuracy[kind]) / f32(m.Evasion[kind]+s.Accuracy[kind]) * 100
	accuracy := f32(m.Accuracy[kind]) / f32(s.Evasion[kind]+m.Accuracy[kind]) * 100
	chance := clampInt(truncInt(levelPart-(defense-accuracy)), 5, 95)
	return rand.Int32N(100) < chance
}

// mobDamage is CMob::AttackDamage (0x004a2920) for a normal attack: the
// slot's Attack + rand(Attackplus) on top of the category attack, element
// row 0 against the player's armour element.
func mobDamage(m *mobCombat, a *db.MobAttack, s *CharacterStats) (int32, bool) {
	var damage [3]int32
	attack := store(f32(int32(a.Attack)) + f32(m.Attack[attackMelee]))
	crit := mobWeaponDamage(m, attackMelee, attack, a.Plus, 0, s.DefenseAttr, s, &damage)
	return damageLimits(damage[0]), crit
}

// mobWeaponDamage is the shared weapon branch of CMob::AttackDamage.
func mobWeaponDamage(m *mobCombat, kind int, attack float32, plus uint16, element, defElement uint8, s *CharacterStats, damage *[3]int32) bool {
	levelRaw := store((f32(m.Level-int32(s.Level)-1)*100 + f32(m.Level)) / 100 * 10)
	level := float64(min(max(levelRaw, 1), 95))
	if plus > 0 {
		attack = store(float64(attack) + f32(int32(rand.Uint32N(uint32(plus)))))
	}
	factor := float32(1)
	if element < 9 && defElement < 9 {
		factor = attackElement[element][defElement]
	}
	// The mob's LUC (stMOBInfo +0x3c).
	luckRoll := func() int32 {
		if m.Luck <= 0 {
			return 0
		}
		return rand.Int32N(m.Luck) + 1
	}
	first := luckRoll()
	defense := f32(s.Guard[kind])
	attackPart := (float64(attack) + float64(attack)/100*level) * float64(factor)
	defensePart := defense - defense/100*level
	value := store(attackPart - defensePart + f32(first)*float64(float32(0.1)))
	if value < 0 {
		value = 0
	}
	// CMob::CriticalSuccess: minus the player's LUC (+0x1234) x 0.034.
	critChance := clampInt(truncInt(f32(m.Critical)/100*100-f32(int32(s.Luc))*float64(float32(0.034))), 5, 99)
	crit := rand.Int32N(100) < critChance
	if crit {
		bonus := store(f32(luckRoll()) * float64(float32(1.4)))
		value = store(float64(value)*1.5 + float64(bonus)/100*level + float64(bonus))
	}
	damage[0] = truncInt(float64(value))
	return crit
}

// mobSkillDamage is CMob::AttackDamage (0x004a3070) with a mob skill
// (Skills rows of level 100): the same DamageType modes as the player's,
// with the mob's own resources for mode 3 and the player's pools for 4
// and 5. The weapon branch uses attack slot 1's Attack/Attackplus and
// element (the skill's AttrType when set).
func mobSkillDamage(m *mobCombat, md *db.MobDef, def *db.SkillDef, s *CharacterStats, mobHP uint32) ([3]int32, bool) {
	values := [3]int32{int32(uint16(def.DamageHP)), int32(uint16(def.DamageSP)), int32(uint16(def.DamageMP))}
	var damage [3]int32
	capAll := func() {
		for i := range damage {
			damage[i] = min(damage[i], maxDamage)
		}
	}
	switch def.DamageType {
	case 0:
		return values, false
	case 3:
		own := [3]uint32{mobHP, md.MaxSP, md.MaxMP}
		for i, v := range values {
			if v != 0 {
				damage[i] = max(int32(own[i]*uint32(v)), 1)
			}
		}
		capAll()
		return damage, false
	case 4, 5:
		source := [3]uint32{s.MaxHP, s.MaxSP, s.MaxMP}
		if def.DamageType == 5 {
			source = [3]uint32{s.CurrentHP, s.CurrentSP, s.CurrentMP}
		}
		for i, v := range values {
			if v != 0 {
				product := math.Trunc(f32(v) / 100 * float64(float32(source[i])))
				damage[i] = max(int32(uint32(int64(product))), 1)
			}
		}
		capAll()
		return damage, false
	case 1:
		damage = values
	}
	kind := int(def.Kind)
	slot := &md.Attacks[0]
	element := slot.Attrib
	if def.AttrType != 0 {
		element = def.AttrType
	}
	defElement := s.DefenseAttr
	if def.TypeDetail == detailNoElement {
		defElement = 0
	}
	attack := store(f32(int32(slot.Attack)) + f32(m.Attack[kind]) + f32(damage[0]))
	crit := mobWeaponDamage(m, kind, attack, slot.Plus, element, defElement, s, &damage)
	if def.DamageType == 2 {
		for i, v := range values {
			if v != 0 {
				damage[i] = int32(uint32(damage[i]) * uint32(v))
			}
		}
	}
	capAll()
	if damage[0] <= 10 {
		damage[0] = rand.Int32N(10) + 1
	}
	return damage, crit
}

// mobSkillHits is CMob::AttackSuccess with a skill: SuccessType 1-3 roll
// SuccessValue, otherwise the category's normal chance.
func mobSkillHits(m *mobCombat, md *db.MobDef, def *db.SkillDef, s *CharacterStats) bool {
	if def.SuccessType >= 1 && def.SuccessType <= 3 {
		chance := int32(def.SuccessValue)
		if def.SuccessType != 1 {
			mine, theirs := int32(md.Wis), int32(s.Wis)
			if def.SuccessType == 3 {
				mine, theirs = int32(md.Luc), int32(s.Luc)
			}
			if mine+theirs != 0 {
				chance += mine / (mine + theirs) * 100
			}
		}
		return rand.Int32N(100) < chance
	}
	return mobHits(m, s, int(def.Kind))
}
