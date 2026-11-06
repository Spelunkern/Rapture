package world

import (
	"math"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// Skills left on a mob (TypeEffect 3), as CMob::AddApplySkill (0x004a0aa0),
// CMob::AddApplySkillDebuff, CMob::RemApplySkill (0x004a0da0),
// CMob::ChkKeepSkill (0x004a10d0, every 3 s from CZone::UpdateApplyEffectTick)
// and CMob::ChkEndTimeSkill (every second) run them:
//
//	61  damage over time (TimeDamageType 0/3/4/5, scaled by elapsed/3000 ms)
//	62, 67  lower the abilities (WIS, DEX, LUC, speeds, category additions)
//	63  on expiry, damage equal to the mob's current HP
//	64 stun, 65 stop (no movement), 66 sleep (ends on any damage)
//	68  removes the mob's element
//	72  fixes the mob's target on the caster
//
// The original sends no packet for the application itself; speed changes
// go out as 0x0609 and damage as 0x0607.

const (
	detailTimeDamage    = 61
	detailAbilityDown   = 62
	detailDeathSentence = 63
	detailStun          = 64
	detailStop          = 65
	detailSleep         = 66
	detailAbilityDown2  = 67
	detailNoElement2    = 68
	detailVisionFix     = 72
	effectTick          = 3 * time.Second
)

// MobEffect is one skill applied to a mob.
type MobEffect struct {
	Def      *db.SkillDef
	CasterID uint32
	endsAt   time.Time
	prevTick time.Time
}

// mobMods are CMob::SetSkillAbility's runtime changes.
type mobMods struct {
	Wis, Dex, Luc          int32
	AttackSpeed, MoveSpeed int32
	Cat                    [3]CategoryMods
	Disabled               [3]int32
	Stun, Stop, Sleep      int32
	NoElement              bool
	VisionFix              uint32
}

func (m *MapMob) mods() mobMods {
	var o mobMods
	for _, e := range m.Effects {
		d := e.Def
		switch d.TypeDetail {
		case detailAbilityDown, detailAbilityDown2:
			for i, t := range d.AbilityType {
				o.addAbility(t, -int32(d.AbilityValue[i]))
			}
		case detailStun:
			o.Stun++
		case detailStop:
			o.Stop++
		case detailSleep:
			o.Sleep++
		case detailNoElement2:
			o.NoElement = true
		case detailVisionFix:
			o.VisionFix = e.CasterID
		}
	}
	return o
}

func (o *mobMods) addAbility(t uint8, v int32) {
	switch {
	case t == 7:
		o.Wis += v
	case t == 8:
		o.Dex += v
	case t == 9:
		o.Luc += v
	case t == 16:
		o.AttackSpeed += v
	case t == 17:
		o.MoveSpeed += v
	case t >= 20 && t <= 31:
		c := &o.Cat[(t-20)%3]
		switch (t - 20) / 3 {
		case 0:
			c.Accuracy += v
		case 1:
			c.Attack += v
		case 2:
			c.Defense += v
		case 3:
			c.Evasion += v
		}
	case t >= 32 && t <= 34:
		o.Disabled[t-32] += v
	}
}

// canAct: stunned or sleeping mobs neither attack nor move.
func (o mobMods) canAct() bool { return o.Stun == 0 && o.Sleep == 0 }

// canMove: a stopped mob still attacks in reach.
func (o mobMods) canMove() bool { return o.canAct() && o.Stop == 0 }

// attackSpeedIndex and moveSpeedIndex are CMob::SendSpeed's values: 255
// while blocked, otherwise the speed index (3 and 2 at normal speed).
func (o mobMods) attackSpeedIndex() uint8 {
	if !o.canAct() {
		return 255
	}
	return uint8(min(max(3+o.AttackSpeed, 0), 6))
}

func (o mobMods) moveSpeedIndex() uint8 {
	if !o.canMove() {
		return 255
	}
	return uint8(min(max(2+o.MoveSpeed, 0), 4))
}

// mobAttackSpeedOffsets is g_ConstMobAttackSpeed by speed index.
var mobAttackSpeedOffsets = [7]time.Duration{1500, 1000, 500, 0, -500, -1000, -1500}

func (o mobMods) attackDelayOffset() time.Duration {
	return mobAttackSpeedOffsets[min(max(3+o.AttackSpeed, 0), 6)] * time.Millisecond
}

// buildMobSpeed (0x0609): mob id, attack speed, move speed.
func buildMobSpeed(m *MapMob) []byte {
	o := m.mods()
	w := packet.New(0x0609)
	w.U32(m.ID)
	w.U8(o.attackSpeedIndex())
	w.U8(o.moveSpeedIndex())
	return w.MustFinish()
}

// mobCombatOf is mobCombatFor with the mob's applied skills.
func mobCombatOf(m *MapMob) mobCombat {
	if m.def == nil {
		return mobCombat{Level: int32(m.Level)}
	}
	o := m.mods()
	d := *m.def
	d.Wis = uint16(max(int32(d.Wis)+o.Wis, 0))
	d.Dex = uint16(max(int32(d.Dex)+o.Dex, 0))
	d.Luc = uint16(max(int32(d.Luc)+o.Luc, 0))
	c := mobCombatFor(&d)
	for k := 0; k < 3; k++ {
		c.Accuracy[k] += o.Cat[k].Accuracy
		c.Attack[k] += o.Cat[k].Attack
		c.Guard[k] += o.Cat[k].Defense
		c.Evasion[k] += o.Cat[k].Evasion
	}
	if o.NoElement {
		c.Attrib = 0
	}
	return c
}

// MobCombatByID returns a live mob's combat values with its applied skills.
func MobCombatByID(mapID uint16, id uint32) (mobCombat, bool) {
	s := shard(mapID)
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.entities.Mobs {
		if m := &s.entities.Mobs[i]; m.ID == id {
			return mobCombatOf(m), true
		}
	}
	return mobCombat{}, false
}

// ApplyMobEffect is CMob::AddApplySkill: the same skill at the same level
// refreshes; a higher level (levels 1-3) replaces a lower one; a lower one
// is ignored. Returns the 0x0609 packet when the mob's speed changed.
func ApplyMobEffect(mapID uint16, id, casterID uint32, def *db.SkillDef) (speed []byte, ok bool) {
	if def.TypeEffect != typeEffectDebuff {
		return nil, false
	}
	s := shard(mapID)
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.entities.Mobs {
		m := &s.entities.Mobs[i]
		if m.ID != id {
			continue
		}
		if m.CurrentHP == 0 || m.returningHome {
			return nil, false
		}
		now := time.Now()
		before := m.mods()
		for j := range m.Effects {
			e := &m.Effects[j]
			if e.Def.SkillID != def.SkillID {
				continue
			}
			if e.Def.SkillLevel == def.SkillLevel {
				e.CasterID, e.prevTick, e.endsAt = casterID, now, now.Add(time.Duration(def.KeepSeconds)*time.Second)
				if def.TypeDetail == detailVisionFix {
					m.AggroTarget = casterID
				}
				return nil, true
			}
			if def.SkillLevel <= 3 && def.SkillLevel <= e.Def.SkillLevel {
				return nil, false
			}
			m.Effects = append(m.Effects[:j], m.Effects[j+1:]...)
			break
		}
		m.Effects = append(m.Effects, MobEffect{Def: def, CasterID: casterID, prevTick: now,
			endsAt: now.Add(time.Duration(def.KeepSeconds) * time.Second)})
		if def.TypeDetail == detailVisionFix && casterID != 0 {
			m.AggroTarget = casterID
		}
		after := m.mods()
		if before.attackSpeedIndex() != after.attackSpeedIndex() || before.moveSpeedIndex() != after.moveSpeedIndex() {
			return buildMobSpeed(m), true
		}
		return nil, true
	}
	return nil, false
}

// removeSleep is CMob::RemApplySkillSleep: damage wakes the mob. Returns
// the 0x0609 packet when it was asleep. The shard lock is held.
func (m *MapMob) removeSleep() []byte {
	kept := m.Effects[:0]
	woke := false
	for _, e := range m.Effects {
		if e.Def.TypeDetail == detailSleep {
			woke = true
			continue
		}
		kept = append(kept, e)
	}
	m.Effects = kept
	if !woke {
		return nil
	}
	return buildMobSpeed(m)
}

// mobTimeDamage is the TimeDamage formula for a mob target: mode 0 the
// raw values, 3 the mob's current pools times the value, 4 and 5 a
// percentage of its maximum or current pools; then scaled by elapsed/3000
// and capped at 30000.
func mobTimeDamage(def *db.SkillDef, cur, maxv [3]uint32, elapsed time.Duration) [3]int32 {
	values := [3]int32{int32(uint16(def.TimeDamageHP)), int32(uint16(def.TimeDamageSP)), int32(uint16(def.TimeDamageMP))}
	var out [3]int32
	for i, v := range values {
		switch def.TimeDamageType {
		case 0:
			out[i] = v
		case 3:
			if v != 0 {
				out[i] = max(int32(cur[i]*uint32(v)), 1)
			}
		case 4, 5:
			if v != 0 {
				source := maxv[i]
				if def.TimeDamageType == 5 {
					source = cur[i]
				}
				out[i] = max(int32(uint32(int64(f32(v)/100*float64(float32(source))))), 1)
			}
		}
	}
	factor := float64(elapsed.Milliseconds()) / 3000
	for i := range out {
		out[i] = min(int32(uint32(int64(math.Trunc(float64(out[i])*factor)))), maxDamage)
	}
	return out
}

// MobEffectOutcome is one damage or expiry event from TickMobEffects.
type MobEffectOutcome struct {
	MapID          uint16
	Mob            MapMob
	CasterID       uint32
	SkillID        uint16
	SkillLevel     uint8
	Damage         [3]int32
	AppliedHP      uint32
	Damaged        bool
	Killed         bool
	RespawnSeconds uint64
	Speed          []byte // 0x0609 when an expiry changed the speed
}

// TickMobEffects is ChkKeepSkill (detail 61, every 3 s) and ChkEndTimeSkill
// (expiry, with the last partial tick of 61 and the whole-HP hit of 63).
func TickMobEffects() []MobEffectOutcome {
	now := time.Now()
	var out []MobEffectOutcome
	for _, mapID := range shardIDs() {
		s := shard(mapID)
		s.mu.Lock()
		for i := range s.entities.Mobs {
			m := &s.entities.Mobs[i]
			if m.CurrentHP == 0 || len(m.Effects) == 0 || m.returningHome {
				continue
			}
			hit := func(e *MobEffect, damage [3]int32, hp uint32) bool {
				applied := min(hp, m.CurrentHP)
				m.CurrentHP -= applied
				killed := m.CurrentHP == 0
				o := MobEffectOutcome{MapID: mapID, CasterID: e.CasterID, SkillID: e.Def.SkillID, SkillLevel: e.Def.SkillLevel,
					Damage: damage, AppliedHP: applied, Damaged: true, Killed: killed}
				if killed {
					m.died(now)
					o.RespawnSeconds = uint64(m.respawnDelay / time.Second)
				} else if applied > 0 {
					o.Speed = m.removeSleep()
				}
				o.Mob = cloneMob(m)
				out = append(out, o)
				return killed
			}
			pools := func() ([3]uint32, [3]uint32) {
				var maxSP, maxMP uint32
				if m.def != nil {
					maxSP, maxMP = m.def.MaxSP, m.def.MaxMP
				}
				return [3]uint32{m.CurrentHP, maxSP, maxMP}, [3]uint32{m.MaxHP, maxSP, maxMP}
			}
			for j := 0; j < len(m.Effects); {
				e := m.Effects[j]
				expired := !e.endsAt.After(now)
				if e.Def.TypeDetail == detailTimeDamage && (expired || now.Sub(e.prevTick) >= effectTick) {
					cur, maxv := pools()
					dmg := mobTimeDamage(e.Def, cur, maxv, now.Sub(e.prevTick))
					m.Effects[j].prevTick = now
					if hit(&e, dmg, uint32(max(dmg[0], 0))) {
						break
					}
				}
				if !expired {
					j++
					continue
				}
				before := m.mods()
				m.Effects = append(m.Effects[:j], m.Effects[j+1:]...)
				if e.Def.TypeDetail == detailDeathSentence {
					hp := m.CurrentHP
					if hit(&e, [3]int32{int32(min(hp, 65535)), 0, 0}, hp) {
						break
					}
				}
				after := m.mods()
				if before.attackSpeedIndex() != after.attackSpeedIndex() || before.moveSpeedIndex() != after.moveSpeedIndex() {
					out = append(out, MobEffectOutcome{MapID: mapID, Mob: cloneMob(m), Speed: buildMobSpeed(m)})
				}
			}
		}
		s.mu.Unlock()
	}
	return out
}
