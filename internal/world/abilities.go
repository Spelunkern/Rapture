package world

import "github.com/Spelunkern/Rapture/internal/db"

// addSigned adds a signed change to an unsigned stat, flooring at 0.
func addSigned(u uint32, v int32) uint32 {
	if v >= 0 {
		return satAdd(u, uint32(v))
	}
	return satSub(u, uint32(-v))
}

// applyAbility is CUser::SetSkillAbility (0x00485e00) for the abilities
// that change combat stats. Debuffs (effect 3) do not raise maximum pools.
// It reports whether the ability type is one it models.
func applyAbility(e *EquipmentStats, effect, kind uint8, value int32) bool {
	pool := func(target *uint32) {
		if effect != 3 {
			*target = addSigned(*target, value)
		}
	}
	switch kind {
	case 1:
		pool(&e.MaxHP)
	case 2:
		pool(&e.MaxMP)
	case 3:
		pool(&e.MaxSP)
	case 4:
		e.Str = addSigned(e.Str, value)
	case 5:
		e.Rec = addSigned(e.Rec, value)
	case 6:
		e.Int = addSigned(e.Int, value)
	case 7:
		e.Wis = addSigned(e.Wis, value)
	case 8:
		e.Dex = addSigned(e.Dex, value)
	case 9:
		e.Luc = addSigned(e.Luc, value)
	case 10, 11, 12:
		e.SitRecoverPct[kind-10] += value // HP, SP, MP
	case 13:
		e.SPCostCut += value
	case 14:
		e.MPCostCut += value
	case 15:
		e.Range += value
	case 16:
		e.AttackSpeed += value
	case 17:
		e.MoveSpeed += value
	case 18:
		e.Critical += value
	case 20, 21, 22:
		e.Mods[kind-20].Accuracy += value
	case 23, 24, 25:
		e.Mods[kind-23].Attack += value
	case 26, 27, 28:
		e.Mods[kind-26].Defense += value
	case 29, 30, 31:
		e.Mods[kind-29].Evasion += value
	case 32, 33, 34:
		e.Disabled[kind-32] += value
	case 35, 37, 38, 40, 41, 42, 44:
		// Benefits read where they apply: 38 and the charms 40-42 set the
		// death exp loss (Session.deathExpLoss).
	case 0:
	default:
		return false
	}
	return true
}

// applyPassives is CUser::InitPassive / AddSkillPassive (0x00489a80) over
// the learned skills in load order. Weapon masteries keep a per-type table
// and each one adds the table's running total for the equipped weapon, as
// the original does.
func applyPassives(e *EquipmentStats, skills []db.CharacterSkill) {
	defs := db.MustDefs()
	var damage, speed [20]int32
	var shield int32
	mastery := func(table *[20]int32, d *db.SkillDef, applied *int32) {
		for _, t := range []uint8{d.Weapon1, d.Weapon2} {
			if t != 0 {
				table[t] += int32(d.WeaponValue)
			}
		}
		if w := e.WeaponType; w != 0 && w < 20 && (w == d.Weapon1 || w == d.Weapon2) {
			*applied += table[w]
		}
	}
	for _, k := range skills {
		d := defs.Skill(k.SkillID, k.Level)
		if d == nil || d.TypeEffect != 0 {
			continue
		}
		switch d.TypeDetail {
		case 1:
			var added int32
			mastery(&damage, d, &added)
			e.ItemAttack = addSigned(e.ItemAttack, added)
		case 4:
			for i, t := range d.AbilityType {
				applyAbility(e, d.TypeEffect, t, int32(d.AbilityValue[i]))
			}
		case 8:
			mastery(&speed, d, &e.AttackSpeed)
		case 9:
			e.CrossRoad++
		case 10:
			if d.Weapon1 != 0 {
				shield += int32(d.WeaponValue)
			}
			if e.ShieldType != 0 && e.ShieldType == d.Weapon1 {
				e.Defense = addSigned(e.Defense, shield)
			}
		}
	}
}
