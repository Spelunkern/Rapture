package world

import (
	"math"

	"github.com/Spelunkern/Rapture/internal/db"
)

// Original EP4.5 game rules, recovered from ps_game.exe (with its PDB) by
// the Eternity reconstruction; each function names the original routine.
// Packet widths follow the 5.4 client, whose readers were checked in the
// client source.

// Grow modes (Chars.Grow).
const (
	growEasy     = 0
	growNormal   = 1
	growHard     = 2
	growUltimate = 3
)

// allowedJob is the family/job mask of CUser::PacketCharacter (0x0046bf20):
// humans fighter/defender/priest, elves ranger/archer/mage, vail
// ranger/mage/oracle, deatheaters warrior/guardian/hunter.
func allowedJob(family, job uint8) bool {
	masks := [4]uint32{0x23, 0x1c, 0x34, 0x0b}
	return family < 4 && job < 6 && masks[family]&(1<<job) != 0
}

// growScale is the HP/SP/MP multiplier of hard (1.1, single precision as
// in the original) and ultimate (1.5) modes.
func growScale(grow uint8) float64 {
	switch grow {
	case growHard:
		return float64(float32(1.1))
	case growUltimate:
		return 1.5
	}
	return 1
}

// startingStats are CUser::ProcessCharacterCreate (0x00480e90)'s base
// stats per job (str, dex, rec, int, wis, luc) plus the family bonus.
func startingStats(family, job uint8) (str, dex, rec, intel, wis, luc uint16) {
	base := [6][6]uint16{{14, 12, 9, 8, 7, 11}, {10, 9, 12, 10, 10, 10}, {10, 15, 9, 7, 8, 12},
		{11, 10, 10, 7, 10, 13}, {7, 9, 9, 15, 12, 9}, {8, 9, 10, 12, 14, 8}}
	b := base[job]
	str, dex, rec, intel, wis, luc = b[0], b[1], b[2], b[3], b[4], b[5]
	switch family {
	case 0:
		luc += 4
	case 1:
		dex += 4
	case 2:
		intel += 2
		wis += 2
	case 3:
		rec += 2
		str += 2
	}
	return
}

// familyPoolBonus is the per-family flat HP/MP/SP of CUser::InitEquipment.
func familyPoolBonus(family uint8) (hp, mp, sp uint32) {
	switch family {
	case 0:
		sp = 10
	case 1:
		mp = 10
	case 2:
		sp, mp = 5, 5
	case 3:
		hp = 10
	}
	return
}

// statusRow is StatusDefs for a job and level (levels 0..60, falling back
// to the last defined level).
func statusRow(level uint16, job uint8) db.Status {
	status := db.MustDefs().Status
	for l := min(level, 60); ; l-- {
		if st, ok := status[db.StatusKey{Level: l, Job: job}]; ok {
			return st
		}
		if l == 0 {
			return db.Status{HP: basePoolPerLevel, SP: basePoolPerLevel, MP: basePoolPerLevel}
		}
	}
}

// basePools are a character's base max HP/MP/SP per CUser::InitEquipment
// (0x00454560): StatusDefs scaled by grow mode, the family bonus and 5 per
// point of REC (HP), DEX (SP) and WIS (MP). Equipment adds on top.
func basePools(ch *db.Character) (hp, mp, sp uint32) {
	st := statusRow(ch.Level, ch.Class)
	scale := growScale(ch.Mode)
	hp = uint32(math.Trunc(float64(st.HP) * scale))
	mp = uint32(math.Trunc(float64(st.MP) * scale))
	sp = uint32(math.Trunc(float64(st.SP) * scale))
	fh, fm, fs := familyPoolBonus(ch.Race)
	hp += fh + uint32(ch.Rec)*5
	mp += fm + uint32(ch.Wis)*5
	sp += fs + uint32(ch.Dex)*5
	return
}

// newCharacterPools are the HP/MP/SP CUser::ProcessCharacterCreate stores:
// level-1 StatusDefs times the single-precision grow multiplier, plus the
// family bonus and the stat-derived part.
func newCharacterPools(family, job, grow uint8, rec, dex, wis uint16) (hp, mp, sp uint16) {
	st := statusRow(1, job)
	m := float32(1)
	if grow == growHard {
		m = 1.1
	} else if grow == growUltimate {
		m = 1.5
	}
	h, s, p := uint32(float32(st.HP)*m), uint32(float32(st.SP)*m), uint32(float32(st.MP)*m)
	fh, fm, fs := familyPoolBonus(family)
	return uint16(h + fh + uint32(rec)*5), uint16(p + fm + uint32(wis)*5), uint16(s + fs + uint32(dex)*5)
}

// starterItems places BaseItemsDefs as CGameData::LoadBaseItem (0x004bf800)
// does: the configured Slot and Quality are ignored, the first item of
// type 1..19 is equipped as the weapon (slot 5) and the rest fill bag 1.
func starterItems(family, job uint8) []db.Item {
	defs := db.MustDefs()
	var out []db.Item
	weapon, next := false, uint8(0)
	for _, b := range defs.BaseItems[db.BaseItemKey{Family: family, Job: job}] {
		def := defs.Item(b.Type, b.TypeID)
		if b.Type == 0 || def == nil {
			continue
		}
		it := db.Item{Type: b.Type, TypeID: b.TypeID, Quality: def.Quality, Count: max(b.Count, 1)}
		if !weapon && b.Type <= 19 {
			it.Bag, it.Slot, weapon = 0, weaponSlot, true
		} else {
			it.Bag, it.Slot = 1, next
			next++
		}
		out = append(out, it)
	}
	return out
}

// levelUp is StatusRules::level_up (CUser level-up path): +1 to the job's
// primary stat, stat and skill points by grow mode, and the pools follow
// the new level.
func levelUp(ch *db.Character) {
	ch.Level++
	switch ch.Class {
	case 0:
		ch.Str++
	case 1:
		ch.Rec++
	case 2:
		ch.Dex++
	case 3:
		ch.Luc++
	case 4:
		ch.Int++
	case 5:
		ch.Wis++
	}
	if ch.Mode > growUltimate {
		return
	}
	statPts, skillPts := uint16(5), uint16(3)
	switch ch.Mode {
	case growHard:
		statPts, skillPts = 7, 4
	case growUltimate:
		statPts, skillPts = 9, 5
	}
	ch.StatPoint += statPts
	ch.SkillPoint += skillPts
}

// mobExperience is CMob::GetExp (0x00491690) without the event
// multiplier: base * (1 + 0.2 * (mobLevel - userLevel + 1)), nothing when
// the mob is more than 9 levels above, never negative.
func mobExperience(mobLevel, base, userLevel uint16) uint32 {
	diff := int32(mobLevel) - int32(userLevel)
	if diff > 9 {
		return 0
	}
	factor := float64(diff+1)*0.2 + 1.0
	v := math.Trunc(float64(base) * factor)
	if v <= 0 || math.IsNaN(v) {
		return 0
	}
	return uint32(min(v, math.MaxInt32))
}

// sittingRecovery is the 5-second recovery while sitting: 5% of each max
// (the original's standing_amount, 0.05f) plus skill/equipment bonuses,
// which Rapture does not model yet.
// sittingRecovery is CUser::UpdateRecover's sitting amount: 5% of the
// maximum plus the sitting-recovery abilities' percent (10-12).
func sittingRecovery(maxValue uint32, extraPct int32) uint32 {
	m := float64(float32(maxValue))
	v := math.RoundToEven(m*float64(float32(0.05)) + float64(float32(extraPct))*m*float64(float32(0.01)))
	return uint32(max(v, 0))
}

// resetStats undoes stat allocation: the job's starting stats plus the
// automatic +1 per level, and every stat point the levels granted.
func resetStats(ch *db.Character) {
	ch.Str, ch.Dex, ch.Rec, ch.Int, ch.Wis, ch.Luc = startingStats(ch.Race, ch.Class)
	probe := db.Character{Class: ch.Class, Mode: ch.Mode, Level: 1}
	for probe.Level < ch.Level {
		levelUp(&probe)
	}
	ch.Str += probe.Str
	ch.Dex += probe.Dex
	ch.Rec += probe.Rec
	ch.Int += probe.Int
	ch.Wis += probe.Wis
	ch.Luc += probe.Luc
	ch.StatPoint = probe.StatPoint
}

// equipSlotRealType is the real item type each equipment slot takes in
// CUser::EnableEquipment (0x0045b450); 0 means any weapon, -1 no check.
var equipSlotRealType = map[uint8]int{0: 16, 1: 17, 2: 18, 3: 20, 4: 21, 5: 0, 6: 19, 7: 24, 8: 23,
	9: 22, 10: 22, 11: 25, 12: 25}

func isWeaponReal(rt uint8) bool  { return rt > 0 && rt < 16 }
func isOneHandReal(rt uint8) bool { return rt == 1 || rt == 3 || rt == 7 || rt == 9 }

// canEquip is CUser::EnableEquipment: level, slot type, two-handed
// weapons against shields, race (Items.Country), class, mode (Items.Grow)
// and the sex-only items (Special 64/65).
func canEquip(ch *db.Character, def *db.ItemDef, slot uint8, items []db.Item) bool {
	if def == nil || ch.Level < def.ReqLevel {
		return false
	}
	rt := realType(def.Type)
	equippedReal := func(s uint8) (uint8, bool) {
		if it := equippedItem(items, s); it != nil {
			return realType(it.Type), true
		}
		return 0, false
	}
	switch want, ok := equipSlotRealType[slot]; {
	case slot == 5:
		if !isWeaponReal(rt) {
			return false
		}
		if _, shield := equippedReal(6); shield && !isOneHandReal(rt) {
			return false
		}
	case slot == 6:
		if int(rt) != want {
			return false
		}
		if w, weapon := equippedReal(5); weapon && !isOneHandReal(w) {
			return false
		}
	case slot == 13:
		if def.Type != mountItemType {
			return false
		}
	case ok && int(rt) != want:
		return false
	}
	race := ch.Race
	switch def.Country {
	case 0, 1:
		if race != def.Country {
			return false
		}
	case 2:
		if race != 0 && race != 1 {
			return false
		}
	case 3:
		if race != 3 {
			return false
		}
	case 4:
		if race != 2 {
			return false
		}
	case 5:
		if race != 2 && race != 3 {
			return false
		}
	}
	if int(ch.Class) >= len(def.Classes) || !def.Classes[ch.Class] {
		return false
	}
	switch ch.Mode {
	case growEasy:
		if def.Grow != 0 {
			return false
		}
	case growNormal:
		if def.Grow > 1 {
			return false
		}
	case growHard:
		if def.Grow == 3 {
			return false
		}
	}
	switch def.Special {
	case 64:
		return ch.Gender != 1
	case 65:
		return ch.Gender != 0
	}
	return true
}
