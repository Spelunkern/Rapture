package world

import (
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
)

// StartSweeps launches the background world ticks: map item despawn, mob
// roam/respawn/combat, NPC patrol and mob DoTs.
func StartSweeps(log *logx.Logger) {
	every(2*time.Second, func() {
		for _, e := range sweepExpiredMapItems() {
			log.Printf("Rapture World despawned map_item_id=%d on map=%d", e.Item.ID, e.MapID)
			dispatchEntityLeave(e.MapID, viewItem, e.Item.ID, "0x0402 map item despawn")
		}
	})
	every(time.Second, func() { dispatchMoves(TickMobRoam(viewersByMap(), 48)) })
	every(time.Second, func() {
		dispatchMoves(TickMobRespawns(viewersByMap(), 128))
		for _, c := range TickMobCorpses() {
			dispatchEntityLeave(c.MapID, viewMob, c.ID, "0x0602 mob corpse removed")
		}
	})
	every(250*time.Millisecond, func() { dispatchMoves(TickNpcPatrol(viewersByMap(), 64)) })
	every(time.Second, func() { runDotSweep(log) })
	every(5*time.Second, func() { TickGuildRanking(time.Now(), log) })
	every(500*time.Millisecond, func() {
		players := AllPlayers()
		if len(players) == 0 {
			return
		}
		snaps := make([]PlayerCombatSnapshot, 0, len(players))
		for _, p := range players {
			x, z := p.PositionXZ()
			c, _ := p.viewCell()
			snaps = append(snaps, PlayerCombatSnapshot{p.CharacterID, p.MapID(), x, z, p.IsAlive(), p.Faction(), c, p.Snapshot().CurrentHP})
		}
		tick := TickMobCombat(snaps, 256)
		for _, a := range tick.Attacks {
			queueMobAttack(a)
		}
		dispatchMoves(tick.Moves)
		for _, r := range tick.Restores {
			sendView(r.MapID, r.X, r.Z, r.Packet, "0x0610 mob hp restore")
		}
	})
}

// queueMobAttack hands a mob's attack to its target, or for an area mob
// skill (TargetType 6 around the mob, 7 around the target) to every living
// player within ApplyRange.
func queueMobAttack(a MobAttackAction) {
	target := FindPlayerByID(a.TargetCharacterID)
	if target == nil {
		return
	}
	first := pendingDamage{a.MobGlobalID, a.MobID, a.Attack, a.Slot, true}
	def := db.MustDefs().Mobs[a.MobID]
	if def == nil || def.Attacks[a.Slot].Type == 0 {
		target.QueueDamage(first)
		return
	}
	sk := db.MustDefs().Skill(def.Attacks[a.Slot].Type, 100)
	if sk == nil || (sk.TargetType != targetTypeEnemiesNearCaster && sk.TargetType != targetTypeEnemiesNearTarget) {
		target.QueueDamage(first)
		return
	}
	x, z := a.MobX, a.MobZ
	if sk.TargetType == targetTypeEnemiesNearTarget {
		x, z = target.PositionXZ()
	}
	for _, p := range PlayersNear(target.MapID(), x, z, float32(sk.ApplyRange)) {
		if p.IsAlive() {
			p.QueueDamage(pendingDamage{a.MobGlobalID, a.MobID, a.Attack, a.Slot, false})
		}
	}
}

func every(d time.Duration, fn func()) {
	go func() {
		t := time.NewTicker(d)
		defer t.Stop()
		for range t.C {
			fn()
		}
	}()
}

func viewersByMap() map[uint16][][2]float32 {
	out := map[uint16][][2]float32{}
	for _, p := range AllPlayers() {
		x, z := p.PositionXZ()
		out[p.MapID()] = append(out[p.MapID()], [2]float32{x, z})
	}
	return out
}

func dispatchMoves(list []entityMove) {
	for _, e := range list {
		dispatchEntityMove(e)
	}
}

func runDotSweep(log *logx.Logger) {
	for _, o := range TickMobEffects() {
		m := &o.Mob
		if o.Speed != nil {
			sendNear(o.MapID, m.PosX, m.PosZ, BroadcastRadius, o.Speed, "0x0609 mob speed")
		}
		if !o.Damaged {
			continue
		}
		sendNear(o.MapID, m.PosX, m.PosZ, CombatRadius,
			buildMobPeriodicDamage(m.ID, o.SkillID, o.SkillLevel, uint16(min(max(o.Damage[0], 0), 65535)),
				uint16(min(max(o.Damage[1], 0), 65535)), uint16(min(max(o.Damage[2], 0), 65535))), "0x0607 mob skill damage")
		sendNear(o.MapID, m.PosX, m.PosZ, CombatRadius, buildMobCurrentHP(m.ID, m.CurrentHP), "0x0610 mob current hp")
		if !o.Killed {
			continue
		}
		log.Printf("Rapture World skill effect kill character_id=%d mob_global_id=%d mob_id=%d", o.CasterID, m.ID, m.MobID)
		sendNear(o.MapID, m.PosX, m.PosZ, CombatRadius, buildMobDeath(m.ID, o.CasterID), "0x0606 mob death (skill effect)")
		rewardKill(FindPlayerByID(o.CasterID), o.CasterID, o.MapID, m, log)
	}
}
