package world

import (
	"math"
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
	if s.actionBlocked() {
		return false // CUser::GetAttackSpeed is 255 while stunned or asleep
	}
	wc, ok := equippedWeaponCombat(s.items)
	if !ok {
		s.log.Printf("Rapture World cancelled auto attack without valid weapon character_id=%d mob_global_id=%d", ch.ID, target)
		return false
	}
	stats := StatsFor(ch, s.equipment())
	// CUser::GetAttackSpeedNow: the swing index moves with gems, passives
	// and buffs (0..9 over g_ConstUserAttackSpeed).
	if idx := min(max(int32(wc.AttackTime)-1+stats.AttackSpeed, 0), 9); idx != int32(wc.AttackTime)-1 {
		wc.Delay, _ = weaponAttackDelay(uint8(idx + 1))
	}
	if !s.lastAutoAttackAt.IsZero() && time.Since(s.lastAutoAttackAt) < wc.Delay {
		return true
	}
	mob, ok := MobByID(ch.MapID, target)
	if !ok || mob.CurrentHP == 0 {
		s.log.Printf("Rapture World cancelled auto attack unknown or dead mob character_id=%d mob_global_id=%d", ch.ID, target)
		return false
	}
	// CUser::EnableAttackRange: weapon reach + mob size + 2.3, plus the
	// movement allowance (at least 2 units).
	size := float32(0)
	if md := db.MustDefs().Mobs[mob.MobID]; md != nil {
		size = float32(md.Size)
	}
	reach := wc.Range + float32(stats.Range) + size + 2.3 + autoAttackRangeGrace
	distance := DistanceXZ(ch.PosX, ch.PosZ, mob.PosX, mob.PosZ)
	if distance > reach {
		if distance > autoAttackCancelRange {
			s.log.Printf("Rapture World cancelled auto attack target too far character_id=%d mob_global_id=%d distance=%.2f", ch.ID, target, distance)
			return false
		}
		return true
	}
	def := db.MustDefs().Mobs[mob.MobID]
	if def == nil {
		return false
	}
	mc, _ := MobCombatByID(ch.MapID, target)
	kind := attackMelee
	if stats.RangedWeapon {
		kind = attackRanged
	}
	if stats.Disabled[kind] > 0 {
		return true // the category is disabled (abilities 32-34); wait it out
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
	s.wearWeapon()
	closeRanged := DistanceXZ(ch.PosX, ch.PosZ, mob.PosX, mob.PosZ) <= closeRangeDistance
	damage, crit := playerDamage(&stats, &mc, kind, stats.AttackAttr, closeRanged)
	result, ok := DamageMob(ch.MapID, target, uint32(damage))
	if !ok {
		return false
	}
	AddMobAggro(ch.MapID, target, ch.ID, int64(result.AppliedHP))
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
	if r.Speed != nil {
		sendNear(ch.MapID, m.PosX, m.PosZ, BroadcastRadius, r.Speed, "0x0609 mob speed (woke)")
	}
	if !r.Killed {
		sendNear(ch.MapID, m.PosX, m.PosZ, CombatRadius, buildMobCurrentHP(m.ID, m.CurrentHP), "0x0610 mob current hp after attack")
		s.send(buildMobTargetHP(m), "0x0305 mob target hp after attack")
		return true
	}
	sendNear(ch.MapID, m.PosX, m.PosZ, CombatRadius, buildMobDeath(m.ID, ch.ID), "0x0606 mob death")
	rewardKill(s.player, ch.ID, ch.MapID, m, s.logger)
	return false
}

// applyIncomingMobDamage applies one queued mob attack; true when it
// killed. Slot attacks with a skill (Attacktype = a level-100 Skills row)
// go through CMob::AttackUser(skill); others are CMob::Attack.
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
	mc, ok := MobCombatByID(ch.MapID, d.MobGlobalID)
	if !ok {
		mc = mobCombatFor(def)
	}
	slot := &def.Attacks[min(d.Slot, 2)]
	if slot.Type != 0 {
		if sk := db.MustDefs().Skill(slot.Type, 100); sk != nil {
			return s.applyMobSkill(d, def, &mc, sk, &stats)
		}
	}
	// CMob::AttackSuccess / CMob::AttackDamage: normal attacks are melee.
	var damage uint32
	result := uint8(attackSuccessNormal)
	if s.blocksAttack(attackMelee) || !mobHits(&mc, &stats, attackMelee) {
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
	// CMob::Attack: a reflected hit spares the player and goes back to
	// the mob (0x0605 then carries no damage).
	reflected := result != attackResultMiss && s.reflects(attackMelee, def)
	killed := false
	shown := damage
	if reflected {
		shown = 0
	} else {
		killed = s.takeMobDamage(d, damage, &stats)
	}
	sendNear(ch.MapID, ch.PosX, ch.PosZ, CombatRadius,
		buildMobAttackCharacter(result, d.MobGlobalID, ch.ID, uint16(min(shown, 65535))), "0x0605 mob attack character")
	if reflected {
		s.reflectToMob(d.MobGlobalID, [3]uint16{uint16(min(damage, 65535)), 0, 0})
	}
	s.log.Printf("Rapture World mob attack mob_global_id=%d mob_id=%d slot=%d result=%d damage=%d killed=%t",
		d.MobGlobalID, d.MobID, d.Slot, result, damage, killed)
	s.afterMobDamage(d, killed)
	return killed
}

// applyMobSkill is a mob skill on this player: 0x060D on the selected
// target, 0x060B on area or repeated hits, 0x060C for a drain (the mob
// heals), then 0x060E for the skill's AddDamage.
func (s *Session) applyMobSkill(d pendingDamage, md *db.MobDef, mc *mobCombat, sk *db.SkillDef, stats *CharacterStats) bool {
	ch := s.char
	mob, _ := MobByID(ch.MapID, d.MobGlobalID)
	hits := 1
	if sk.TypeDetail == detailMulti {
		hits = int(sk.MultiAttack)
	}
	killed := false
	for i := 0; i < hits && !killed; i++ {
		first := d.First && sk.TypeDetail != detailMulti
		drain := first && sk.TypeDetail == detailDrain
		var damage [3]int32
		result := uint8(attackSuccessNormal)
		if s.blocksAttack(int(sk.Kind)) || !mobSkillHits(mc, md, sk, stats) {
			result = attackResultMiss
		} else {
			var crit bool
			damage, crit = mobSkillDamage(mc, md, sk, stats, mob.CurrentHP)
			if crit {
				result = attackResultCritical
			}
		}
		hp := uint32(max(damage[0], 0))
		killed = s.takeMobDamage(d, hp, stats)
		words := [3]uint16{uint16(min(hp, 65535)), uint16(min(max(damage[1], 0), 65535)), uint16(min(max(damage[2], 0), 65535))}
		var frame []byte
		switch {
		case drain:
			if r, ok := HealMob(ch.MapID, d.MobGlobalID, hp); ok {
				sendNear(ch.MapID, r.PosX, r.PosZ, CombatRadius, buildMobCurrentHP(r.ID, r.CurrentHP), "0x0610 mob hp after drain")
			}
			frame = buildMobSkillPacket(0x060C, result, d.MobGlobalID, ch.ID, sk, words, &words)
		case first:
			frame = buildMobSkillPacket(0x060D, result, d.MobGlobalID, ch.ID, sk, words, nil)
		default:
			frame = buildMobSkillPacket(0x060B, result, d.MobGlobalID, ch.ID, sk, words, nil)
		}
		sendNear(ch.MapID, ch.PosX, ch.PosZ, CombatRadius, frame, "mob skill on character")
		s.log.Printf("Rapture World mob skill mob_global_id=%d mob_id=%d skill_id=%d result=%d damage=%d killed=%t",
			d.MobGlobalID, d.MobID, sk.SkillID, result, hp, killed)
		if !killed && !drain && result != attackResultMiss && sk.AddDamageHP != 0 {
			add := uint32(uint16(sk.AddDamageHP))
			killed = s.takeMobDamage(d, add, stats)
			w := packet.New(0x060E)
			w.U32(d.MobGlobalID)
			w.U32(ch.ID)
			w.U16(sk.SkillID)
			w.U8(sk.SkillLevel)
			w.U16(uint16(min(add, 65535)))
			w.U16(uint16(sk.AddDamageSP))
			w.U16(uint16(sk.AddDamageMP))
			sendNear(ch.MapID, ch.PosX, ch.PosZ, CombatRadius, w.MustFinish(), "0x060E mob skill extra damage")
		}
		// CMob::AttackUser(skill): a debuff stays on the player.
		if !killed && result != attackResultMiss && sk.TypeEffect == typeEffectDebuff {
			s.applyMobDebuff(sk, d.MobGlobalID)
		}
		refreshed := s.stats()
		stats = &refreshed
	}
	s.afterMobDamage(d, killed)
	return killed
}

// buildMobSkillPacket: result, mob id, target id, a byte the client reads
// before the skill id (0x060B/0x060C only), skill id, level, HP/SP/MP
// damage and, for 0x060C, the healed HP/SP/MP.
func buildMobSkillPacket(op packet.Opcode, result uint8, mobID, target uint32, sk *db.SkillDef, damage [3]uint16, healed *[3]uint16) []byte {
	w := packet.New(op)
	w.U8(result)
	w.U32(mobID)
	w.U32(target)
	if op != 0x060D {
		w.U8(0)
	}
	w.U16(sk.SkillID)
	w.U8(sk.SkillLevel)
	for _, v := range damage {
		w.U16(v)
	}
	if healed != nil {
		for _, v := range healed {
			w.U16(v)
		}
	}
	return w.MustFinish()
}

// takeMobDamage lowers the character's HP; true when it died.
func (s *Session) takeMobDamage(d pendingDamage, damage uint32, stats *CharacterStats) bool {
	ch := s.char
	cur := s.stats()
	if cur.CurrentHP == 0 || damage == 0 {
		return false
	}
	s.wearArmour()
	newEffective := satSub(cur.CurrentHP, damage)
	ch.CurrentHP = satSub(ch.CurrentHP, cur.CurrentHP-newEffective)
	killed := newEffective == 0
	if killed {
		ch.Deaths++
	}
	s.persist()
	s.dismount()
	if !killed {
		s.wake()
	}
	return killed
}

func (s *Session) afterMobDamage(d pendingDamage, killed bool) {
	ch := s.char
	refreshed := s.stats()
	s.send(buildCurrentHitpoints(&refreshed), "0x0521 current hp after mob attack")
	if !killed {
		return
	}
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
	s.partyPools()
}

// --- experience ---

// LevelCap is the last level in StatusDefs and ExpDefs.
const LevelCap = 60

// levelCap is award_experience's: easy mode stops at 30.
func levelCap(ch *db.Character) uint16 {
	if ch.Mode == 0 {
		return 30
	}
	return LevelCap
}

// applyExpGain adds exp. Chars.Exp is the cumulative total and ExpDefs
// gives, per level and growth mode, the total needed to reach the next
// level, as in the original server. A level-up refreshes the base pools
// from StatusDefs and heals fully.
func applyExpGain(ch *db.Character, gained uint32) (levels uint16) {
	limit := levelCap(ch)
	if ch.Level >= limit || gained == 0 {
		return 0
	}
	ch.Exp = satAdd(ch.Exp, gained)
	exp := db.MustDefs().Exp
	for ch.Level < limit {
		req := exp[db.ExpKey{Level: ch.Level, Grow: ch.Mode + 1}]
		if req == 0 || ch.Exp < req {
			break
		}
		levelUp(ch)
		levels++
	}
	if ch.Level >= limit {
		// Reaching the cap leaves the exp at the level's start.
		prev, _ := expBounds(ch)
		ch.Exp = prev
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

// rewardKill hands out a kill's loot and exp: drops on the ground, gold
// split across the party (or dropped for a solo killer) and each share of
// exp queued to its recipient's session. An offline DoT caster gets the
// solo exp straight in the database.
func rewardKill(killer *ConnectedPlayer, killerID uint32, mapID uint16, m *MapMob, log logPrinter) {
	var party []*ConnectedPlayer
	if killer != nil {
		party = partyMembersNear(killer, mapID, m.PosX, m.PosZ)
	}
	recipients := party
	if len(recipients) == 0 && killer != nil {
		recipients = []*ConnectedPlayer{killer}
	}
	for _, it := range rollMobDrops(mapID, m, killerID, party, log) {
		dispatchItemEnter(mapID, &it, "0x0401 mob drop")
	}
	if def := questItemDrop(m, recipients); def != nil {
		x, z := dropPosition(m.PosX, m.PosZ)
		mi := MapItem{ID: nextMapItemID(), OwnerID: killerID, DroppedAt: time.Now(), PosX: x, PosZ: z,
			Item: db.Item{Type: def.Type, TypeID: def.TypeID, Quality: def.Quality, Count: 1}}
		addMapItem(mapID, mi)
		dispatchItemEnter(mapID, &mi, "0x0401 quest item drop")
	}
	for _, p := range recipients {
		p.QueueQuestKill(m.MobID)
	}
	if m.Exp == 0 {
		return
	}
	base := uint16(min(m.Exp, 65535))
	if killer == nil {
		grantOfflineKillExp(killerID, m.MobID, m.Level, uint32(base), log)
		return
	}
	for member, amount := range killRewards(killer, mapID, m.PosX, m.PosZ, m.Level, base) {
		if amount > 0 {
			member.QueueExp(amount)
		}
	}
}

// expScale is award_experience's modifiers: x1.5 with exp doubling
// (ability 35), +0.2 with any charm (abilities 40-42).
func (s *Session) expScale(gained uint32) uint32 {
	double, charm := false, false
	for _, b := range s.buffs {
		for _, a := range b.Abilities {
			if a.Value > 0 {
				double = double || a.Type == 35
				charm = charm || (a.Type >= 40 && a.Type <= 42)
			}
		}
	}
	scale := 1.0
	if double {
		scale = 1.5
	}
	if charm {
		scale += 0.2
	}
	return uint32(math.Trunc(float64(gained) * scale))
}

// awardExp applies an exp reward to the session's character.
func (s *Session) awardExp(gained uint32, modifiers bool) {
	ch := s.char
	if ch == nil {
		return
	}
	if modifiers {
		gained = s.expScale(gained) // quest rewards skip them
	}
	levels := applyExpGain(ch, gained)
	s.persist()
	s.log.Printf("Rapture World exp character_id=%d gained=%d level=%d exp=%d levels_gained=%d",
		ch.ID, gained, ch.Level, ch.Exp, levels)
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
		s.partyLevel()
		s.guildLevelChanged()
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
	scaled := uint32(float64(mobExperience(mobLevel, uint16(min(mobExp, 65535)), ch.Level)) * 1.2)
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
