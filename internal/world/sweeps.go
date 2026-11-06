package world

import (
	"time"

	"github.com/Spelunkern/Rapture/internal/logx"
)

// StartSweeps launches the background world ticks: map item despawn, mob
// roam/respawn/combat, NPC patrol and mob DoTs.
func StartSweeps(log *logx.Logger) {
	every(2*time.Second, func() {
		for _, e := range sweepExpiredMapItems() {
			log.Printf("Rapture World despawned map_item_id=%d on map=%d", e.Item.ID, e.MapID)
			sendNear(e.MapID, e.Item.PosX, e.Item.PosZ, ViewportRadius, buildMapRemoveItem(e.Item.ID), "0x0402 map item despawn")
		}
	})
	every(time.Second, func() { broadcastMapPackets(TickMobRoam(viewersByMap(), 48), ViewportRadius, "0x0603 mob roam move") })
	every(time.Second, func() {
		broadcastMapPackets(TickMobRespawns(viewersByMap(), 128), ViewportRadius, "0x0601 mob respawn")
	})
	every(4*time.Second, func() {
		broadcastMapPackets(TickNpcPatrol(viewersByMap(), 16), ViewportRadius, "0x0E03 npc patrol move")
	})
	every(time.Second, func() { runDotSweep(log) })
	every(500*time.Millisecond, func() {
		players := AllPlayers()
		if len(players) == 0 {
			return
		}
		snaps := make([]PlayerCombatSnapshot, 0, len(players))
		for _, p := range players {
			x, z := p.PositionXZ()
			snaps = append(snaps, PlayerCombatSnapshot{p.CharacterID, p.MapID(), x, z, p.IsAlive()})
		}
		tick := TickMobCombat(snaps, 256)
		for _, a := range tick.Attacks {
			if p := FindPlayerByID(a.TargetCharacterID); p != nil {
				p.QueueDamage(a.MobGlobalID, a.MobID, a.Attack)
			}
		}
		broadcastMapPackets(tick.Moves, ViewportRadius, "0x0603 mob chase move")
		broadcastMapPackets(tick.Restores, CombatRadius, "0x0610 mob hp restore on reset")
	})
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

func broadcastMapPackets(list []mapPacket, radius float32, label string) {
	for _, m := range list {
		sendNear(m.MapID, m.X, m.Z, radius, m.Packet, label)
	}
}

func runDotSweep(log *logx.Logger) {
	for _, o := range TickAllMobDots() {
		if o.AppliedHP == 0 {
			continue
		}
		m := &o.Mob
		if o.IsFinalTick {
			sendNear(o.MapID, m.PosX, m.PosZ, CombatRadius,
				buildMobDebuff(0x030D, MobDebuffTypeDOT, m.ID, o.SkillID, o.SkillLevel), "0x030D mob debuff remove")
		}
		sendNear(o.MapID, m.PosX, m.PosZ, CombatRadius,
			buildMobPeriodicDamage(m.ID, o.SkillID, o.SkillLevel, uint16(min(o.AppliedHP, 65535)), 0, 0), "0x0607 mob periodic damage")
		sendNear(o.MapID, m.PosX, m.PosZ, CombatRadius, buildMobCurrentHP(m.ID, m.CurrentHP), "0x0610 mob current hp")
		if !o.Killed {
			continue
		}
		log.Printf("Rapture World dot kill character_id=%d mob_global_id=%d mob_id=%d", o.CasterID, m.ID, m.MobID)
		sendNear(o.MapID, m.PosX, m.PosZ, CombatRadius, buildMobDeath(m.ID, o.CasterID), "0x0606 mob death (dot)")
		caster := FindPlayerByID(o.CasterID)
		var y float32
		if caster != nil {
			y = caster.Snapshot().PosY
		}
		rewardKill(caster, o.CasterID, o.MapID, m, y, log)
	}
}
