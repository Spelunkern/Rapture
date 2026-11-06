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
	os.Chdir("../..")
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
					d, _ := playerDamage(&st, &mc, attackMelee, 0, st.AttackAttr)
					dmg += d
				}
				if mobHits(&mc, &st, int(def.Attacks[0].Type)) {
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
	os.Chdir("../..")
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
				d, _ := playerDamage(&st, &mc, attackMelee, 0, st.AttackAttr)
				dmg += d
			}
			if mobHits(&mc, &st, int(def.Attacks[0].Type)) {
				mhits++
				d, _ := mobDamage(&mc, &def.Attacks[0], &st)
				mdmg += d
			}
		}
		t.Logf("  vs mob %d %q L%d hp %d atk %d+%d: player hit %d%% avg %d | mob hit %d%% avg %d", id, def.Name, def.Level, def.MaxHP,
			def.Attacks[0].Attack, def.Attacks[0].Plus, hits*100/2000, dmg/int32(max(hits, 1)), mhits*100/2000, mdmg/int32(max(mhits, 1)))
	}
}
