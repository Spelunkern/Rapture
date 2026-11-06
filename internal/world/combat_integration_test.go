//go:build integration

package world

import (
	"os"
	"testing"

	"github.com/Spelunkern/Rapture/internal/config"
	"github.com/Spelunkern/Rapture/internal/db"
)

// TestCombatNumbers simulates the migrated test characters against real
// mobs with the original formulas and logs hit rates and damage.
func TestCombatNumbers(t *testing.T) {
	if _, err := os.Stat("data/game"); err != nil {
		os.Chdir("../..")
	}
	config.LoadRuntimeEnv()
	if err := db.Open(db.UserData, db.GameData, db.GameDefs); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Definitions(); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"Interloper", "Valiant", "Arcanist"} {
		id, uid, found, err := db.CharacterByName(name)
		if err != nil || !found {
			t.Skipf("%s not found", name)
		}
		ch, _ := db.CharacterDetail(uid, id)
		items, _ := db.CharacterItems(id)
		st := StatsFor(&ch, GetEquipmentStats(items))
		t.Logf("%s L%d job %d: atk %v acc %v eva %v guard %v crit %d var %d hp %d", name, ch.Level, ch.Class,
			st.Attack, st.Accuracy, st.Evasion, st.Guard, st.Critical, st.Variance, st.MaxHP)
		for _, mobID := range []uint16{103, 60, 61} {
			def := db.MustDefs().Mobs[mobID]
			mc := mobCombatFor(def)
			hits, dmg, mhits, mdmg := 0, int32(0), 0, int32(0)
			for range 2000 {
				if playerHits(&st, &mc, attackMelee) {
					hits++
					d, _ := playerDamage(&st, &mc, attackMelee, st.AttackAttr, false)
					dmg += d
				}
				if mobHits(&mc, &st, attackMelee) {
					mhits++
					d, _ := mobDamage(&mc, &def.Attacks[0], &st)
					mdmg += d
				}
			}
			t.Logf("  vs mob %d L%d hp %d: player hit %d%% avg %d | mob hit %d%% avg %d", mobID, def.Level, def.MaxHP,
				hits*100/2000, dmg/int32(max(hits, 1)), mhits*100/2000, mdmg/int32(max(mhits, 1)))
		}
	}
}

func TestNewCharacterCombat(t *testing.T) {
	if _, err := os.Stat("data/game"); err != nil {
		os.Chdir("../..")
	}
	config.LoadRuntimeEnv()
	if err := db.Open(db.UserData, db.GameData, db.GameDefs); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Definitions(); err != nil {
		t.Fatal(err)
	}
	ch := db.Character{Level: 1, Race: 0, Class: 0, Mode: 1}
	ch.Str, ch.Dex, ch.Rec, ch.Int, ch.Wis, ch.Luc = startingStats(0, 0)
	items := starterItems(0, 0)
	ch.CurrentHP, ch.CurrentMP, ch.CurrentSP = 9999, 9999, 9999
	st := StatsFor(&ch, GetEquipmentStats(items))
	t.Logf("new human fighter: atk %v acc %v eva %v guard %v var %d hp %d", st.Attack, st.Accuracy, st.Evasion, st.Guard, st.Variance, st.MaxHP)
	for id, def := range db.MustDefs().Mobs {
		if def.Level > 3 || def.MaxHP > 400 || def.Attacks[0].Attack == 0 {
			continue
		}
		mc := mobCombatFor(def)
		hits, dmg, mhits, mdmg := 0, int32(0), 0, int32(0)
		for range 2000 {
			if playerHits(&st, &mc, attackMelee) {
				hits++
				d, _ := playerDamage(&st, &mc, attackMelee, st.AttackAttr, false)
				dmg += d
			}
			if mobHits(&mc, &st, attackMelee) {
				mhits++
				d, _ := mobDamage(&mc, &def.Attacks[0], &st)
				mdmg += d
			}
		}
		t.Logf("  vs mob %d %q L%d hp %d atk %d+%d: player hit %d%% avg %d | mob hit %d%% avg %d", id, def.Name, def.Level, def.MaxHP,
			def.Attacks[0].Attack, def.Attacks[0].Plus, hits*100/2000, dmg/int32(max(hits, 1)), mhits*100/2000, mdmg/int32(max(mhits, 1)))
	}
}

func TestEquipRules(t *testing.T) {
	if _, err := os.Stat("data/game"); err != nil {
		os.Chdir("../..")
	}
	config.LoadRuntimeEnv()
	if err := db.Open(db.UserData, db.GameData, db.GameDefs); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Definitions(); err != nil {
		t.Fatal(err)
	}
	defs := db.MustDefs()
	for family := uint8(0); family < 4; family++ {
		for job := uint8(0); job < 6; job++ {
			if !allowedJob(family, job) {
				continue
			}
			ch := db.Character{Level: 1, Race: family, Class: job, Mode: 1}
			for _, it := range starterItems(family, job) {
				if it.Bag == 0 && !canEquip(&ch, defs.Item(it.Type, it.TypeID), it.Slot, nil) {
					t.Errorf("family %d job %d cannot equip starter %d/%d in slot %d", family, job, it.Type, it.TypeID, it.Slot)
				}
			}
		}
	}
	for _, name := range []string{"Interloper", "Valiant", "Bulwark", "Shade", "Sylvan", "Arcanist", "Reaper", "Ironwall", "Stalker", "Viper", "Hexen", "Seer"} {
		id, uid, found, _ := db.CharacterByName(name)
		if !found {
			continue
		}
		ch, _ := db.CharacterDetail(uid, id)
		items, _ := db.CharacterItems(id)
		for i, it := range items {
			if it.Bag != 0 {
				continue
			}
			others := append(append([]db.Item{}, items[:i]...), items[i+1:]...)
			if !canEquip(&ch, defs.Item(it.Type, it.TypeID), it.Slot, others) {
				d := defs.Item(it.Type, it.TypeID)
				t.Logf("%s (race %d job %d mode %d sex %d) fails %d/%d %q slot %d: reqlvl %d country %d grow %d classes %v special %d",
					name, ch.Race, ch.Class, ch.Mode, ch.Gender, it.Type, it.TypeID, d.Name, it.Slot, d.ReqLevel, d.Country, d.Grow, d.Classes, d.Special)
			}
		}
	}
}

// TestSkillCombat runs every player attack skill (levels 1-3) and every
// mob skill through the original success and damage modes against a real
// mob and character, checking the results stay in the original limits.
func TestSkillCombat(t *testing.T) {
	if _, err := os.Stat("data/game"); err != nil {
		os.Chdir("../..")
	}
	config.LoadRuntimeEnv()
	if err := db.Open(db.UserData, db.GameData, db.GameDefs); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Definitions(); err != nil {
		t.Fatal(err)
	}
	id, uid, found, err := db.CharacterByName("Arcanist")
	if err != nil || !found {
		t.Skip("Arcanist not found")
	}
	ch, _ := db.CharacterDetail(uid, id)
	items, _ := db.CharacterItems(id)
	st := StatsFor(&ch, GetEquipmentStats(items))
	md := db.MustDefs().Mobs[103]
	mc := mobCombatFor(md)
	modes := map[uint8]int{}
	players, mobs := 0, 0
	for _, def := range db.MustDefs().Skills {
		if def.TypeEffect != typeEffectAttack && def.TypeEffect != typeEffectDebuff {
			continue
		}
		if def.Kind > 2 || def.DefenceType > 2 {
			t.Fatalf("skill %d/%d not normalized: kind %d defence %d", def.SkillID, def.SkillLevel, def.Kind, def.DefenceType)
		}
		if def.MobSkill {
			mobs++
			dmg, _ := mobSkillDamage(&mc, md, def, &st, md.MaxHP)
			_ = mobSkillHits(&mc, md, def, &st)
			if dmg[0] > maxDamage || (def.DamageType != 0 && dmg[0] < 0) {
				t.Errorf("mob skill %d mode %d damage %v", def.SkillID, def.DamageType, dmg)
			}
			continue
		}
		players++
		modes[def.DamageType]++
		target := skillTarget{MaxHP: md.MaxHP, HP: md.MaxHP, MaxMP: md.MaxMP, MaxSP: md.MaxSP, MP: md.MaxMP, SP: md.MaxSP}
		dmg, _ := playerSkillDamage(&st, &mc, def, target, [3]uint32{st.CurrentHP, st.CurrentSP, st.CurrentMP}, false)
		_ = playerSkillHits(&st, &mc, md, def)
		weapon := def.DamageType == 1 || def.DamageType == 2 || def.DamageType > 5
		if dmg[0] > maxDamage || (weapon && dmg[0] < 1) || (!weapon && def.DamageType != 0 && def.DamageHP != 0 && dmg[0] < 1) {
			t.Errorf("skill %d/%d mode %d damage %v", def.SkillID, def.SkillLevel, def.DamageType, dmg)
		}
	}
	t.Logf("player attack/debuff skills %d by mode %v, mob skills %d", players, modes, mobs)
	if players == 0 || mobs == 0 {
		t.Fatal("no skills loaded")
	}
	var e EquipmentStats
	e.WeaponType = 5
	applyPassives(&e, []db.CharacterSkill{})
	if e.ItemAttack != 0 {
		t.Fatal("passives without skills changed the attack")
	}
}
