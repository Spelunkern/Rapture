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

// setAttack is CUser::SetAttack (0x00454280) on top of the equipment.
func setAttack(s *CharacterStats, e EquipmentStats) {
	dex, str, wis, intel, luck := float32(s.Dex), float32(s.Str), float32(s.Wis), float32(s.Int), float32(s.Luc)
	halfDex := dex * 0.5
	defenseBase := luck*0.125 + halfDex
	fifthDex := dex * 0.2
	item := float32(e.ItemAttack)
	level := float32(s.Level)
	s.Critical = truncInt(float64(luck * 0.19))
	for c := 0; c < 2; c++ {
		s.Accuracy[c] = truncInt(float64(level + halfDex))
		s.Evasion[c] = truncInt(float64(defenseBase + level))
		s.Guard[c] = int32(s.Rec) + int32(e.Defense)
	}
	s.Attack[attackMelee] = truncInt(float64(str*1.3 + item + fifthDex))
	s.Attack[attackRanged] = truncInt(float64(luck*0.3 + item + str + fifthDex))
	s.Attack[attackMagic] = truncInt(float64(intel*1.3 + wis*0.2 + item))
	s.Guard[attackMagic] = int32(e.Resistance) + int32(s.Wis)
	s.Variance, s.AttackAttr, s.RangedWeapon = e.Variance, e.AttackAttr, e.RangedWeapon

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
		return !magicResisted(int32(s.Level), m.Level, m.Evasion[2], int32(s.Dex), m.Dex, s.Accuracy[2])
	}
	level := float64(float32(s.Level))
	levels := float64(float32(int32(s.Level) + m.Level))
	levelPart := level / levels * 160
	defense := f32(m.Accuracy[kind]) / f32(s.Evasion[kind]+m.Accuracy[kind]) * 100
	accuracy := f32(s.Accuracy[kind]) / f32(m.Evasion[kind]+s.Accuracy[kind]) * 100
	chance := clampInt(truncInt(levelPart-(defense-accuracy)), 20, 99)
	return rand.Int32N(100) < chance
}

// playerDamage is CUser::AttackDamage against a mob; extra is added to the
// attack first (a skill's damage). Returns damage and whether it crit.
func playerDamage(s *CharacterStats, m *mobCombat, kind int, extra int32, element uint8) (int32, bool) {
	levelRaw := store((f32(m.Level-int32(s.Level)-1)*100 + f32(m.Level)) / 100 * 10)
	level := float64(min(max(levelRaw, 1), 95))
	attack := store(f32(s.Attack[kind] + extra))
	if s.Variance > 0 {
		attack = store(float64(attack) + f32(int32(rand.Uint32N(s.Variance))))
	}
	factor := float32(1)
	if element < 9 && m.Attrib < 9 {
		factor = attackElement[element][m.Attrib]
	}
	wisRoll := func() int32 {
		if s.Wis == 0 {
			return 0
		}
		return int32(rand.Uint32N(s.Wis)) + 1
	}
	first := wisRoll()
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
		bonus := store(f32(wisRoll()) * float64(float32(1.4)))
		value = store(float64(value)*1.5 + float64(bonus) - float64(bonus)/100*level)
	}
	return damageLimits(truncInt(float64(value))), crit
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
		return !magicResisted(m.Level, int32(s.Level), s.Evasion[2], m.Dex, int32(s.Dex), m.Accuracy[2])
	}
	level := float64(float32(m.Level))
	levels := float64(float32(int32(s.Level) + m.Level))
	levelPart := level / levels * 160
	defense := f32(s.Accuracy[kind]) / f32(m.Evasion[kind]+s.Accuracy[kind]) * 100
	accuracy := f32(m.Accuracy[kind]) / f32(s.Evasion[kind]+m.Accuracy[kind]) * 100
	chance := clampInt(truncInt(levelPart-(defense-accuracy)), 5, 95)
	return rand.Int32N(100) < chance
}

// mobDamage is CMob::AttackDamage with one of its attack slots. The
// player's armour element is not modeled, so the element factor is 1.
func mobDamage(m *mobCombat, a *db.MobAttack, s *CharacterStats) (int32, bool) {
	kind := int(min(a.Type, 2))
	levelRaw := store((f32(m.Level-int32(s.Level)-1)*100 + f32(m.Level)) / 100 * 10)
	level := float64(min(max(levelRaw, 1), 95))
	attack := store(f32(int32(a.Attack)) + f32(m.Attack[kind]))
	if a.Plus > 0 {
		attack = store(float64(attack) + f32(int32(rand.Uint32N(uint32(a.Plus)))))
	}
	wisRoll := func() int32 {
		if m.Wis <= 0 {
			return 0
		}
		return rand.Int32N(m.Wis) + 1
	}
	first := wisRoll()
	defense := f32(s.Guard[kind])
	attackPart := float64(attack) + float64(attack)/100*level
	defensePart := defense - defense/100*level
	value := store(attackPart - defensePart + f32(first)*float64(float32(0.1)))
	if value < 0 {
		value = 0
	}
	critChance := clampInt(truncInt(f32(m.Critical)/100*100-f32(int32(s.Wis))*float64(float32(0.034))), 5, 99)
	crit := rand.Int32N(100) < critChance
	if crit {
		bonus := store(f32(wisRoll()) * float64(float32(1.4)))
		value = store(float64(value)*1.5 + float64(bonus)/100*level + float64(bonus))
	}
	return damageLimits(truncInt(float64(value))), crit
}
