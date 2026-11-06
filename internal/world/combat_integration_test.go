//go:build integration

package world

import (
	"os"
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/config"
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
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

// TestMobEffects applies real player debuff skills to a mob: stun blocks
// it (0x0609 255), damage over time ticks every 3 s, a detail-63 skill
// takes the mob's whole HP when it ends, and damage wakes a sleeping mob.
func TestMobEffects(t *testing.T) {
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
	find := func(detail uint16) *db.SkillDef {
		for _, d := range db.MustDefs().Skills {
			if !d.MobSkill && d.TypeEffect == typeEffectDebuff && d.TypeDetail == detail && d.KeepSeconds > 0 {
				return d
			}
		}
		t.Fatalf("no player skill with detail %d", detail)
		return nil
	}
	const mapID = 60000
	md := db.MustDefs().Mobs[103]
	AddEntities(mapID, MapEntities{Mobs: []MapMob{NewStaticMob(md, 103, 10, 10, nil, 0)}})
	mob := EntitiesOnMap(mapID).Mobs[0]

	stun := find(detailStun)
	speed, ok := ApplyMobEffect(mapID, mob.ID, 7, stun)
	if !ok || speed == nil || speed[8] != 255 || speed[9] != 255 {
		t.Fatalf("stun speed packet %v ok %t", speed, ok)
	}
	m, _ := MobByID(mapID, mob.ID)
	if o := m.mods(); o.canAct() {
		t.Fatal("stunned mob can act")
	}

	sleep := find(detailSleep)
	ApplyMobEffect(mapID, mob.ID, 7, sleep)
	r, _ := DamageMob(mapID, mob.ID, 1)
	if r.Speed == nil {
		t.Fatal("damage did not wake the sleeping mob")
	}
	m, _ = MobByID(mapID, mob.ID)
	if o := m.mods(); o.Sleep != 0 {
		t.Fatal("sleep survived damage")
	}

	dot := find(detailTimeDamage)
	ApplyMobEffect(mapID, mob.ID, 7, dot)
	s := shard(mapID)
	s.mu.Lock()
	for i := range s.entities.Mobs[0].Effects {
		s.entities.Mobs[0].Effects[i].prevTick = time.Now().Add(-3 * time.Second)
	}
	s.mu.Unlock()
	var dotHits int
	for _, o := range TickMobEffects() {
		if o.MapID == mapID && o.Damaged && o.SkillID == dot.SkillID {
			dotHits++
			t.Logf("dot %d/%d type %d dealt %v", dot.SkillID, dot.SkillLevel, dot.TimeDamageType, o.Damage)
		}
	}
	if dotHits != 1 {
		t.Fatalf("dot ticks %d", dotHits)
	}

	s.mu.Lock()
	s.entities.Mobs[0].CurrentHP, s.entities.Mobs[0].Effects = s.entities.Mobs[0].MaxHP, nil
	s.mu.Unlock()
	sentence := find(detailDeathSentence)
	if _, ok := ApplyMobEffect(mapID, mob.ID, 7, sentence); !ok {
		t.Fatal("detail 63 not applied")
	}
	s.mu.Lock()
	for i := range s.entities.Mobs[0].Effects {
		s.entities.Mobs[0].Effects[i].endsAt = time.Now().Add(-time.Second)
	}
	s.mu.Unlock()
	killed := false
	for _, o := range TickMobEffects() {
		if o.MapID == mapID && o.Killed {
			killed = true
		}
	}
	if !killed {
		t.Fatal("detail 63 did not take the whole HP on expiry")
	}
}

// TestPlayerEffects runs mob debuffs, item charms, blocks and reflection
// on an offline session (sends are no-ops without a connection).
func TestPlayerEffects(t *testing.T) {
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
	logger, err := logx.New("rapture_world_test")
	if err != nil {
		t.Fatal(err)
	}
	ch := &db.Character{Level: 60, Race: 0, Class: 0, Mode: 1, Str: 100, Dex: 100, Rec: 100, Int: 50, Wis: 50, Luc: 50}
	ch.MaxHP, ch.MaxMP, ch.MaxSP = basePools(ch)
	ch.CurrentHP, ch.CurrentMP, ch.CurrentSP = ch.MaxHP, ch.MaxMP, ch.MaxSP
	s := &Session{char: ch, log: logger.Tagged("unit"), logger: logger, skillCooldowns: map[uint16]time.Time{}}
	mobSkill := func(detail uint16) *db.SkillDef {
		for _, d := range db.MustDefs().Skills {
			if d.MobSkill && d.TypeEffect == typeEffectDebuff && d.TypeDetail == detail && d.KeepSeconds > 0 {
				return d
			}
		}
		return nil
	}
	if stun := mobSkill(detailStun); stun != nil {
		s.applyMobDebuff(stun, 1)
		if !s.actionBlocked() || s.speeds().attack != 255 {
			t.Fatal("stun did not block actions")
		}
		s.removeEffects(func(*ActiveBuff) bool { return true }, "test")
	}
	if sleep := mobSkill(detailSleep); sleep != nil {
		s.applyMobDebuff(sleep, 1)
		s.takeMobDamage(pendingDamage{MobGlobalID: 1}, 1, nil)
		if s.actionBlocked() {
			t.Fatal("damage did not wake the player")
		}
	}
	if dot := mobSkill(detailTimeDamage); dot != nil {
		s.applyMobDebuff(dot, 1)
		s.buffs[len(s.buffs)-1].prevTick = time.Now().Add(-3 * time.Second)
		before := s.stats().CurrentHP
		s.tickDebuffs(time.Now())
		t.Logf("mob dot %d/%d type %d: hp %d -> %d", dot.SkillID, dot.SkillLevel, dot.TimeDamageType, before, s.stats().CurrentHP)
		s.removeEffects(func(*ActiveBuff) bool { return true }, "test")
	}
	if down := mobSkill(detailAbilityDown); down != nil {
		before := s.stats()
		s.applyMobDebuff(down, 1)
		after := s.stats()
		t.Logf("ability down %d/%d types %v values %v: str %d->%d dex %d->%d", down.SkillID, down.SkillLevel,
			down.AbilityType, down.AbilityValue, before.Str, after.Str, before.Dex, after.Dex)
		s.removeEffects(func(*ActiveBuff) bool { return true }, "test")
	}
	charm := func(skillID uint16) int {
		s.buffs = nil
		if d := db.MustDefs().Skill(skillID, 1); d != nil {
			s.addEffect(d, 0)
		}
		return s.deathExpLoss()
	}
	if got := charm(231); got != 1 {
		t.Errorf("Blue Dragon charm loss %d", got)
	}
	if got := charm(233); got != 0 {
		t.Errorf("Red Phoenix charm loss %d", got)
	}
	if got := charm(230); got != 0 {
		t.Errorf("Prevent EXP Drop loss %d", got)
	}
	s.buffs = nil
	if got := s.deathExpLoss(); got != deathExpPercent {
		t.Errorf("plain loss %d", got)
	}
	for _, d := range db.MustDefs().Skills {
		if d.TypeEffect == typeEffectBuff && d.TypeDetail == 85 && !d.MobSkill && d.DefenceValue > 0 {
			s.buffs = nil
			s.addEffect(d, 0)
			blocked := 0
			for range int(d.DefenceValue) + 2 {
				if s.blocksAttack(int(d.DefenceType)) {
					blocked++
				}
			}
			if blocked != int(d.DefenceValue) {
				t.Errorf("block %d/%d blocked %d of %d", d.SkillID, d.SkillLevel, blocked, d.DefenceValue)
			}
			break
		}
	}
}

// TestMobAI drives real mobs through TickMobCombat: target search (AI 1/3,
// country, the 5 s spawn delay), aggro retargeting, the 45-unit leash,
// returning home without a refill and the 5 s recovery.
func TestMobAI(t *testing.T) {
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
	var md *db.MobDef
	for _, d := range db.MustDefs().Mobs {
		if d.AI == 1 && d.ChaseRange >= 8 && d.Country == 2 && d.RecoverValue > 0 && d.ChaseStep > 0 && d.Attacks[0].OK != 0 {
			md = d
			break
		}
	}
	if md == nil {
		t.Skip("no aggressive mob")
	}
	const mapID = 60001
	AddEntities(mapID, MapEntities{Mobs: []MapMob{NewStaticMob(md, md.MobID, 100, 100, nil, 0)}})
	s := shard(mapID)
	mob := func() *MapMob { return &s.entities.Mobs[0] }
	snap := func(id uint32, x, z float32) PlayerCombatSnapshot {
		return PlayerCombatSnapshot{CharacterID: id, MapID: mapID, PosX: x, PosZ: z, Alive: true, Faction: 0, Cell: cellFor(x, z)}
	}
	near := snap(1, 100+md.ChaseRange-1, 100)
	TickMobCombat([]PlayerCombatSnapshot{near}, 64)
	if mob().AggroTarget != 0 {
		t.Fatal("targeted during the 5 s spawn delay")
	}
	s.mu.Lock()
	mob().noTargetUntil = time.Time{}
	s.mu.Unlock()
	TickMobCombat([]PlayerCombatSnapshot{near}, 64)
	if mob().AggroTarget != 1 {
		t.Fatalf("AI %d mob did not target a player within ChaseRange %.0f", md.AI, md.ChaseRange)
	}
	// A second attacker with more damage takes over at the next attack.
	other := snap(2, 101, 100)
	AddMobAggro(mapID, mob().ID, 1, 10)
	AddMobAggro(mapID, mob().ID, 2, 500)
	s.mu.Lock()
	m := mob()
	m.PosX, m.PosZ, m.nextAttackAt = 100, 100, time.Time{}
	s.mu.Unlock()
	TickMobCombat([]PlayerCombatSnapshot{snap(1, 100.5, 100), other}, 64)
	if mob().AggroTarget != 2 {
		t.Fatalf("ReTargetByAggro kept %d", mob().AggroTarget)
	}
	// The target runs past 45 units from home: the mob walks home.
	far := snap(2, 100+mobLeash+5, 100)
	TickMobCombat([]PlayerCombatSnapshot{far}, 64)
	if mob().AggroTarget != 0 || !mob().returningHome {
		t.Fatal("leash did not send the mob home")
	}
	s.mu.Lock()
	m = mob()
	m.CurrentHP = m.MaxHP / 2
	m.PosX, m.recoverAt = 100+mobHomeDone-1, time.Time{}
	m.nextChaseAt = time.Time{}
	s.mu.Unlock()
	before := mob().CurrentHP
	TickMobCombat(nil, 64)
	TickMobCombat([]PlayerCombatSnapshot{snap(3, 900, 900)}, 64)
	if got := mob().CurrentHP; got != min(before+uint32(md.RecoverValue), md.MaxHP) {
		t.Fatalf("hp after return and one recovery %d (before %d, recover %d)", got, before, md.RecoverValue)
	}
	if mob().returningHome {
		t.Fatal("return did not complete within 10 units of home")
	}
}

// TestGroupRespawn: a group of two that loses both mobs gets one back per
// timer (CZone::UpdateMobRespawn), the timer started by the first death.
func TestGroupRespawn(t *testing.T) {
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
	md := db.MustDefs().Mobs[103]
	const mapID = 60002
	g := &mobGroup{count: 2, alive: 2}
	area := MobArea{0, 0, 20, 20}
	var list []MapMob
	for range 2 {
		m := NewStaticMob(md, 103, 10, 10, &area, 1)
		m.group = g
		list = append(list, m)
	}
	AddEntities(mapID, MapEntities{Mobs: list})
	for _, m := range EntitiesOnMap(mapID).Mobs {
		if r, ok := DamageMob(mapID, m.ID, m.MaxHP); !ok || !r.Killed {
			t.Fatal("kill failed")
		}
	}
	if g.alive != 0 {
		t.Fatalf("alive %d", g.alive)
	}
	s := shard(mapID)
	s.mu.Lock()
	for i := range s.entities.Mobs {
		s.entities.Mobs[i].corpseGone = true
	}
	g.respawnAt = time.Now().Add(-time.Second)
	s.mu.Unlock()
	viewers := map[uint16][][2]float32{mapID: {{10, 10}}}
	if got := len(TickMobRespawns(viewers, 128)); got != 1 || g.alive != 1 {
		t.Fatalf("first timer respawned %d (alive %d)", got, g.alive)
	}
	if got := len(TickMobRespawns(viewers, 128)); got != 0 {
		t.Fatalf("respawned %d before the timer ran out again", got)
	}
}

// TestCraftRules checks the lapisia/gem rules against the EP4.5 data.
func TestCraftRules(t *testing.T) {
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
	weaponLap, armorLap := defs.Item(95, 1), defs.Item(95, 6)
	if weaponLap == nil || armorLap == nil {
		t.Skip("no lapisia")
	}
	if !lapisiaFits(weaponLap, 1) || lapisiaFits(weaponLap, 16) {
		t.Error("weapon lapisia targets")
	}
	if lapisiaFits(armorLap, 1) || !lapisiaFits(armorLap, 16) || !lapisiaFits(armorLap, 17) {
		t.Error("armor lapisia targets")
	}
	if lapisiaMoney(weaponLap.ReqIg) != 2500000 || lapisiaMoney(armorLap.ReqIg) != 1040000 {
		t.Errorf("lapisia costs %d %d", lapisiaMoney(weaponLap.ReqIg), lapisiaMoney(armorLap.ReqIg))
	}
	// A two-socket item takes two Slot-1 gems, not a third.
	var target *db.ItemDef
	for _, d := range defs.Items {
		if d.Slot == 2 && realType(d.Type) >= 1 && realType(d.Type) <= 15 {
			target = d
			break
		}
	}
	var gems []*db.ItemDef
	for _, d := range defs.Items {
		if d.Type == gemItemType && d.Slot == 1 && lapisiaFits(d, realType(target.Type)) {
			gems = append(gems, d)
		}
	}
	if target == nil || len(gems) < 3 {
		t.Skip("no two-socket weapon or slot-1 gems")
	}
	it := db.Item{Type: target.Type, TypeID: target.TypeID, Count: 1}
	for i := 0; i < 2; i++ {
		if !gemFits(&it, target, gems[i]) {
			t.Fatalf("gem %d refused", i)
		}
		it.Gems[i] = gems[i].TypeID
	}
	if gemFits(&it, target, gems[2]) || gemFits(&it, target, gems[0]) {
		t.Fatal("capacity or duplicate check failed")
	}
}

// TestRebirth follows KillSoul / KillSoulByItem(NoMove) / UpdateRebirth:
// the normal revive loses exp and restores full pools at the bind point;
// runes cost no exp, give half pools and are chosen by ReqVg (0 to stay,
// set to move); ultimate mode needs a rune, and without one the character
// ends.
func TestRebirth(t *testing.T) {
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
	logger, err := logx.New("rapture_world_test")
	if err != nil {
		t.Fatal(err)
	}
	const mapID = 60010
	RegisterMapGrid(mapID, 2048, nil)
	AddEntities(mapID, MapEntities{SpawnAreas: []MapSpawnArea{{Faction: 1, Boxes: []SpawnBox{{300, 10, 300, 310, 12, 310}}}}})
	ch := &db.Character{Level: 30, Mode: growNormal, Str: 100, Dex: 100, Rec: 100, Int: 50, Wis: 50, Luc: 50, MapID: mapID, PosX: 900, PosZ: 900}
	ch.MaxHP, ch.MaxMP, ch.MaxSP = basePools(ch)
	s := &Session{char: ch, log: logger.Tagged("unit"), logger: logger, faction: FactionLight, skillCooldowns: map[uint16]time.Time{}}
	prev, next := expBounds(ch)
	die := func() {
		ch.CurrentHP, ch.CurrentMP, ch.CurrentSP = 0, 0, 0
		ch.PosX, ch.PosZ = 900, 900
		s.dead = true
		s.deathLoss = s.deathExpLoss()
		s.startRebirthTimer()
	}
	full := func() CharacterStats {
		c := *ch
		c.CurrentHP, c.CurrentMP, c.CurrentSP = c.MaxHP, c.MaxMP, c.MaxSP
		return StatsFor(&c, s.equipment())
	}
	inBox := func() bool { return ch.PosX >= 300 && ch.PosX < 310 && ch.PosZ >= 300 && ch.PosZ < 310 }

	ch.Exp = prev + (next-prev)/2
	die()
	s.handleResurrect([]byte{0x53, 0x05, reviveAtBind})
	if s.dead || !inBox() || s.stats().CurrentHP != full().MaxHP || ch.Exp >= prev+(next-prev)/2 {
		t.Fatalf("bind revive: dead %v pos (%v, %v) hp %d/%d exp %d", s.dead, ch.PosX, ch.PosZ, s.stats().CurrentHP, full().MaxHP, ch.Exp)
	}

	s.items = []db.Item{{Bag: 1, Slot: 3, Type: 100, TypeID: 44, Count: 2}, {Bag: 1, Slot: 1, Type: 100, TypeID: 81, Count: 1}}
	exp := ch.Exp
	die()
	s.handleResurrect([]byte{0x53, 0x05, reviveRuneHere})
	if s.dead || ch.PosX != 900 || ch.Exp != exp || s.stats().CurrentHP != full().MaxHP/2 || len(s.items) != 2 || s.items[0].Count != 1 {
		t.Fatalf("rune here: dead %v pos %v exp %d hp %d items %+v", s.dead, ch.PosX, ch.Exp, s.stats().CurrentHP, s.items)
	}
	die()
	s.handleResurrect([]byte{0x53, 0x05, reviveRuneAtBind})
	if s.dead || !inBox() || ch.Exp != exp || len(s.items) != 1 || s.items[0].TypeID != 44 {
		t.Fatalf("rune at bind: dead %v pos (%v, %v) exp %d items %+v", s.dead, ch.PosX, ch.PosZ, ch.Exp, s.items)
	}

	die()
	s.tickRebirth(time.Now().Add(rebirthDeadline - time.Second))
	if !s.dead {
		t.Fatal("revived before the 200 s deadline")
	}
	s.tickRebirth(time.Now().Add(rebirthDeadline + time.Second))
	if s.dead || !inBox() {
		t.Fatal("not revived at the deadline")
	}

	ch.Mode = growUltimate
	exp = ch.Exp
	die()
	s.handleResurrect([]byte{0x53, 0x05, reviveAtBind})
	if s.dead || ch.Exp != exp || len(s.items) != 0 || s.stats().CurrentHP != full().MaxHP/2 {
		t.Fatalf("ultimate with rune: dead %v exp %d items %+v", s.dead, ch.Exp, s.items)
	}
	die()
	s.handleResurrect([]byte{0x53, 0x05, reviveAtBind})
	if s.char != nil {
		t.Fatal("ultimate death without a rune kept the character")
	}
}
