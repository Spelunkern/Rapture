package world

import (
	"math"
	"math/rand/v2"
	"sync"
	"sync/atomic"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// Visibility and combat radii, matching the real server's cell view model.
const (
	VisibilityCellSize = 40
	ViewportRadius     = 120.0
	CombatRadius       = BroadcastRadius // CZone::PSendView
	MobAttackRange     = 3.0
	MobAttackInterval  = 2500 * time.Millisecond

	mobMotionRun  = 1
	mobMotionWalk = 0
)

type NpcPatrolPoint struct {
	X, Y, Z float32
	Angle   uint16
}

type MapNpc struct {
	ID               uint32
	Type             uint8
	TypeID           uint16
	PosX, PosY, PosZ float32
	Angle            uint16
	patrol           []NpcPatrolPoint
	patrolIndex      int
	cell             cell
	hasCell          bool
	backward         bool      // walking the patrol points back
	nextMoveAt       time.Time // CNpc +0x4c
}

type MobArea struct{ LowerX, LowerZ, UpperX, UpperZ float32 }

func (a MobArea) normalized() MobArea {
	return MobArea{min(a.LowerX, a.UpperX), min(a.LowerZ, a.UpperZ), max(a.LowerX, a.UpperX), max(a.LowerZ, a.UpperZ)}
}
func (a MobArea) clampX(x float32) float32 { return min(max(x, a.LowerX), a.UpperX) }
func (a MobArea) clampZ(z float32) float32 { return min(max(z, a.LowerZ), a.UpperZ) }
func (a MobArea) contains(x, z float32) bool {
	return x >= a.LowerX && x <= a.UpperX && z >= a.LowerZ && z <= a.UpperZ
}

type MapMob struct {
	ID                 uint32
	MobID              uint16
	Level              uint16
	PosX, PosZ         float32
	homeX, homeZ       float32
	CurrentHP, MaxHP   uint32
	MoneyMin, MoneyMax uint32
	Exp                uint32
	Area               *MobArea
	respawnDelay       time.Duration // 0: no respawn
	respawnAt          time.Time
	roamStep           uint32
	Attack             uint32
	AI                 uint8
	ChaseRange         float32
	AggroTarget        uint32 // 0: none
	lastAttackAt       time.Time
	nextAttackAt       time.Time        // CMob m_dwNextAttackTime
	noTargetUntil      time.Time        // CMob::Init: no target search for 5 s
	recoverAt          time.Time        // CMob::UpdateRecover, every 5 s
	aggro              map[uint32]int64 // CDamage ledger: damage and skill aggro
	slotReadyAt        [3]time.Time     // per attack slot (AttackAI)
	returningHome      bool
	def                *db.MobDef
	nextMoveAt         time.Time   // idle wander
	nextChaseAt        time.Time   // chase / return step
	Effects            []MobEffect // applied skills (TypeEffect 3)
	cell               cell        // view cell, with the move hysteresis
	hasCell            bool
	diedAt             time.Time
	corpseGone         bool // the corpse left the view (5 s after death)
}

type MapPortal struct {
	Index                     uint8
	PortalID                  int32
	FactionOrPortalID         int32
	MinLevel, MaxLevel        uint16
	TargetMapID               uint16
	PosX, PosY, PosZ          float32
	TargetX, TargetY, TargetZ float32
}

func (p *MapPortal) Contains(x, y, z float32) bool {
	return abs32(p.PosX-x) <= 5 && abs32(p.PosY-y) <= 5 && abs32(p.PosZ-z) <= 5
}
func (p *MapPortal) IsOpen() bool { return p.FactionOrPortalID <= 2 }
func (p *MapPortal) AllowsFaction(f uint8) bool {
	switch p.FactionOrPortalID {
	case 1:
		return f == 0
	case 2:
		return f == 1
	}
	return true
}
func (p *MapPortal) AllowsLevel(level uint16) bool { return level >= p.MinLevel && level <= p.MaxLevel }

type MapSpawnArea struct {
	Faction                int32
	LowerX, LowerY, LowerZ float32
	UpperX, UpperY, UpperZ float32
}

func (a *MapSpawnArea) Center() (float32, float32, float32) {
	return (a.LowerX + a.UpperX) * 0.5, (a.LowerY + a.UpperY) * 0.5, (a.LowerZ + a.UpperZ) * 0.5
}
func (a *MapSpawnArea) distanceSq(x, z float32) float32 {
	cx, _, cz := a.Center()
	return (cx-x)*(cx-x) + (cz-z)*(cz-z)
}
func (a *MapSpawnArea) allowsFaction(f uint8) bool {
	switch a.Faction {
	case 0:
		return true
	case 1:
		return f == 0
	case 2:
		return f == 1
	}
	return false
}

// MapEntities is one map's NPCs, mobs, portals and spawn areas.
type MapEntities struct {
	NPCs       []MapNpc
	Mobs       []MapMob
	Portals    []MapPortal
	SpawnAreas []MapSpawnArea
}

type mapShard struct {
	mu       sync.Mutex
	entities MapEntities
}

type mapGrid struct {
	size uint32
	mask []byte
}

var (
	shardsMu  sync.Mutex
	shards    = map[uint16]*mapShard{}
	gridsMu   sync.RWMutex
	grids     = map[uint16]mapGrid{}
	nextNpcID atomic.Uint32
	nextMobID atomic.Uint32
)

func init() {
	nextNpcID.Store(0x7000_0000)
	nextMobID.Store(0x7100_0000)
}

func shard(mapID uint16) *mapShard {
	shardsMu.Lock()
	defer shardsMu.Unlock()
	s, ok := shards[mapID]
	if !ok {
		s = &mapShard{}
		shards[mapID] = s
	}
	return s
}

func shardIDs() []uint16 {
	shardsMu.Lock()
	defer shardsMu.Unlock()
	ids := make([]uint16, 0, len(shards))
	for id := range shards {
		ids = append(ids, id)
	}
	return ids
}

func cloneMob(m *MapMob) MapMob {
	c := *m
	c.Effects = append([]MobEffect(nil), m.Effects...)
	if m.Area != nil {
		a := *m.Area
		c.Area = &a
	}
	return c
}

// EntitiesOnMap returns a copy of a map's entities.
func EntitiesOnMap(mapID uint16) MapEntities {
	s := shard(mapID)
	s.mu.Lock()
	defer s.mu.Unlock()
	out := MapEntities{NPCs: append([]MapNpc(nil), s.entities.NPCs...),
		Portals:    append([]MapPortal(nil), s.entities.Portals...),
		SpawnAreas: append([]MapSpawnArea(nil), s.entities.SpawnAreas...)}
	for i := range s.entities.Mobs {
		out.Mobs = append(out.Mobs, cloneMob(&s.entities.Mobs[i]))
	}
	return out
}

func RegisterMapGrid(mapID uint16, size uint32, mask []byte) {
	gridsMu.Lock()
	grids[mapID] = mapGrid{size, mask}
	gridsMu.Unlock()
}

func hasGrid(mapID uint16) bool {
	gridsMu.RLock()
	defer gridsMu.RUnlock()
	_, ok := grids[mapID]
	return ok
}

func abs32(v float32) float32 { return float32(math.Abs(float64(v))) }

func DistanceXZ(ax, az, bx, bz float32) float32 {
	dx, dz := ax-bx, az-bz
	return float32(math.Sqrt(float64(dx*dx + dz*dz)))
}

type cell struct{ x, z int32 }

func coordCell(v float32, size float32) int32 {
	if math.IsNaN(float64(v)) || math.IsInf(float64(v), 0) || v <= 0 {
		return 0
	}
	return int32(math.Floor(math.Round(float64(v)) / float64(size)))
}

func cellFor(x, z float32) cell {
	return cell{coordCell(x, VisibilityCellSize), coordCell(z, VisibilityCellSize)}
}

// isNeighborCell is the 5x5 view window.
func isNeighborCell(a, b cell) bool {
	dx, dz := a.x-b.x, a.z-b.z
	if dx < 0 {
		dx = -dx
	}
	if dz < 0 {
		dz = -dz
	}
	return dx <= 2 && dz <= 2
}

// EntitiesNear returns the entities in the 5x5 cell window around (x, z).
func EntitiesNear(mapID uint16, x, z float32) MapEntities {
	return EntitiesInView(mapID, cellFor(x, z))
}

// EntitiesInView returns the NPCs and the visible mobs whose cell is in the
// 5x5 window around the viewer's cell.
func EntitiesInView(mapID uint16, viewer cell) MapEntities {
	s := shard(mapID)
	s.mu.Lock()
	defer s.mu.Unlock()
	var out MapEntities
	for i := range s.entities.NPCs {
		n := &s.entities.NPCs[i]
		c := n.cell
		if !n.hasCell {
			c = cellFor(n.PosX, n.PosZ)
		}
		if isNeighborCell(viewer, c) {
			out.NPCs = append(out.NPCs, *n)
		}
	}
	for i := range s.entities.Mobs {
		m := &s.entities.Mobs[i]
		c := m.cell
		if !m.hasCell {
			c = cellFor(m.PosX, m.PosZ)
		}
		if m.visible() && isNeighborCell(viewer, c) {
			out.Mobs = append(out.Mobs, cloneMob(m))
		}
	}
	return out
}

func IsMapWalkable(mapID uint16, x, z float32) bool {
	gridsMu.RLock()
	g, ok := grids[mapID]
	gridsMu.RUnlock()
	if !ok {
		return true
	}
	return walkableInMask(g.size, g.mask, x, z)
}

func walkableInMask(size uint32, mask []byte, x, z float32) bool {
	if size == 0 || len(mask) == 0 || math.IsNaN(float64(x)) || math.IsInf(float64(x), 0) ||
		math.IsNaN(float64(z)) || math.IsInf(float64(z), 0) {
		return true
	}
	if x < 0 || z < 0 {
		return false
	}
	ix, iz := uint32(math.Floor(float64(x))), uint32(math.Floor(float64(z)))
	if ix >= size || iz >= size {
		return false
	}
	bit := uint64(iz)*uint64(size) + uint64(ix)
	if bit/8 >= uint64(len(mask)) {
		return false
	}
	return (mask[bit/8]>>(bit%8))&1 == 0
}

func MobByID(mapID uint16, id uint32) (MapMob, bool) {
	s := shard(mapID)
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.entities.Mobs {
		if s.entities.Mobs[i].ID == id {
			return cloneMob(&s.entities.Mobs[i]), true
		}
	}
	return MapMob{}, false
}

type MobDamageResult struct {
	Mob            MapMob
	Speed          []byte // 0x0609 when the hit woke the mob
	DamageHP       uint16
	AppliedHP      uint32
	Killed         bool
	RespawnSeconds uint64
}

// DamageMob applies damage unless the mob is dead or resetting home.
func DamageMob(mapID uint16, id uint32, damage uint32) (MobDamageResult, bool) {
	s := shard(mapID)
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.entities.Mobs {
		m := &s.entities.Mobs[i]
		if m.ID != id {
			continue
		}
		if m.CurrentHP == 0 || m.returningHome {
			return MobDamageResult{}, false
		}
		applied := min(damage, m.CurrentHP)
		m.CurrentHP -= applied
		r := MobDamageResult{DamageHP: uint16(min(damage, 65535)), AppliedHP: applied, Killed: m.CurrentHP == 0}
		if r.Killed {
			m.Effects, m.diedAt, m.corpseGone = nil, time.Now(), false
		} else {
			r.Speed = m.removeSleep()
		}
		if r.Killed && m.respawnDelay > 0 {
			m.respawnAt = time.Now().Add(m.respawnDelay)
			r.RespawnSeconds = uint64(m.respawnDelay / time.Second)
		}
		r.Mob = cloneMob(m)
		return r, true
	}
	return MobDamageResult{}, false
}

type PlayerCombatSnapshot struct {
	CharacterID uint32
	MapID       uint16
	PosX, PosZ  float32
	Alive       bool
	Faction     uint8
	Cell        cell
}

type MobAttackAction struct {
	MobGlobalID       uint32
	MobID             uint16
	Attack            uint32
	TargetCharacterID uint32
	Slot              uint8
	MobX, MobZ        float32
}

// mapPacket is a packet to broadcast near (x, z) on a map.
type mapPacket struct {
	MapID  uint16
	X, Z   float32
	Packet []byte
}

type MobCombatTick struct {
	Attacks  []MobAttackAction
	Moves    []entityMove
	Restores []mapPacket
}

type viewerBuckets map[cell][][2]float32

func bucketViewers(viewers [][2]float32) viewerBuckets {
	b := viewerBuckets{}
	for _, v := range viewers {
		c := cellFor(v[0], v[1])
		b[c] = append(b[c], v)
	}
	return b
}

func (b viewerBuckets) nearAny(x, z float32, radius float32) bool {
	own := cellFor(x, z)
	r2 := radius * radius
	for c, vs := range b {
		if !isNeighborCell(own, c) {
			continue
		}
		for _, v := range vs {
			dx, dz := x-v[0], z-v[1]
			if dx*dx+dz*dz <= r2 {
				return true
			}
		}
	}
	return false
}

// TickMobRoam moves idle mobs near viewers within their area.
func TickMobRoam(viewersByMap map[uint16][][2]float32, maxPerMap int) []entityMove {
	var out []entityMove
	for mapID, viewers := range viewersByMap {
		s := shard(mapID)
		s.mu.Lock()
		buckets := bucketViewers(viewers)
		moved := 0
		for i := range s.entities.Mobs {
			if moved >= maxPerMap {
				break
			}
			m := &s.entities.Mobs[i]
			if m.CurrentHP == 0 || m.Area == nil || m.AggroTarget != 0 || m.returningHome ||
				!buckets.nearAny(m.PosX, m.PosZ, ViewportRadius) || !m.movable() {
				continue
			}
			if m.advanceRoam(mapID) {
				moved++
				out = append(out, m.relocate(mapID, 0))
			}
		}
		s.mu.Unlock()
	}
	return out
}

// TickMobRespawns revives due mobs at a fresh position in their area.
func TickMobRespawns(viewersByMap map[uint16][][2]float32, maxPerMap int) []entityMove {
	var out []entityMove
	now := time.Now()
	for mapID, viewers := range viewersByMap {
		s := shard(mapID)
		s.mu.Lock()
		buckets := bucketViewers(viewers)
		respawned := 0
		for i := range s.entities.Mobs {
			if respawned >= maxPerMap {
				break
			}
			m := &s.entities.Mobs[i]
			if m.CurrentHP > 0 || m.respawnAt.IsZero() || m.respawnAt.After(now) {
				continue
			}
			x, z := m.pickRespawnPosition(mapID)
			m.PosX, m.PosZ, m.homeX, m.homeZ = x, z, x, z
			m.CurrentHP = max(m.MaxHP, 1)
			m.respawnAt = time.Time{}
			m.AggroTarget, m.lastAttackAt, m.nextAttackAt, m.returningHome = 0, time.Time{}, time.Time{}, false
			respawned++
			m.hasCell, m.diedAt, m.corpseGone = false, time.Time{}, false
			m.noTargetUntil, m.aggro = now.Add(5*time.Second), nil
			e := m.relocate(mapID, 0)
			e.Move, e.Enter = nil, buildMobEnter(m, true)
			out = append(out, e)
			_ = buckets
		}
		s.mu.Unlock()
	}
	return out
}

// TickNpcPatrol is CNpc::UpdateMove for patrolling NPCs near viewers:
// every MoveSpeed ms an NPC steps MoveDistance toward its next patrol
// point; on reaching it, it walks the points forward then back and rests
// rand(10 s) + MoveSpeed + 10 s. NPCs in use by a player stand still.
func TickNpcPatrol(viewersByMap map[uint16][][2]float32, maxPerMap int) []entityMove {
	var out []entityMove
	now := time.Now()
	for mapID, viewers := range viewersByMap {
		s := shard(mapID)
		s.mu.Lock()
		buckets := bucketViewers(viewers)
		moved := 0
		for i := range s.entities.NPCs {
			if moved >= maxPerMap {
				break
			}
			n := &s.entities.NPCs[i]
			if len(n.patrol) <= 1 || now.Before(n.nextMoveAt) || !buckets.nearAny(n.PosX, n.PosZ, ViewportRadius) || npcInUse(n.ID) {
				continue
			}
			if n.stepPatrol(now) {
				moved++
				out = append(out, n.relocate(mapID))
			}
		}
		s.mu.Unlock()
	}
	return out
}

func (n *MapNpc) stepPatrol(now time.Time) bool {
	def := findNpcDef(n.Type, n.TypeID)
	if def == nil || def.MoveDistance <= 0 {
		n.nextMoveAt = now.Add(time.Minute)
		return false
	}
	interval := time.Duration(max(def.MoveSpeed, 0)) * time.Millisecond
	p := n.patrol[n.patrolIndex]
	dx, dz := p.X-float32(math.Round(float64(n.PosX))), p.Z-float32(math.Round(float64(n.PosZ)))
	dist := float32(math.Hypot(float64(dx), float64(dz)))
	step := float32(def.MoveDistance)
	if dist <= step {
		n.PosX, n.PosY, n.PosZ, n.Angle = p.X, p.Y, p.Z, p.Angle
		if n.backward {
			n.patrolIndex--
			if n.patrolIndex < 0 {
				n.patrolIndex, n.backward = 1, false
			}
		} else {
			n.patrolIndex++
			if n.patrolIndex >= len(n.patrol) {
				n.patrolIndex, n.backward = len(n.patrol)-2, true
			}
		}
		n.nextMoveAt = now.Add(time.Duration(rand.IntN(10000))*time.Millisecond + interval + 10*time.Second)
		return true
	}
	n.PosX += dx / dist * step
	n.PosZ += dz / dist * step
	n.nextMoveAt = now.Add(interval)
	return true
}

// relocate updates the mob's cell after a position change and returns the
// update for the users around it.
func (m *MapMob) relocate(mapID uint16, motion uint8) entityMove {
	from, fresh := m.cell, !m.hasCell
	m.cell, m.hasCell = cellAfterMove(m.cell, m.hasCell, m.PosX, m.PosZ), true
	return entityMove{MapID: mapID, Kind: viewMob, ID: m.ID, From: from, To: m.cell, Fresh: fresh,
		Changed: fresh || from != m.cell, Move: buildMobMove(m, motion), Enter: buildMobEnter(m, false)}
}

func (n *MapNpc) relocate(mapID uint16) entityMove {
	from, fresh := n.cell, !n.hasCell
	n.cell, n.hasCell = cellAfterMove(n.cell, n.hasCell, n.PosX, n.PosZ), true
	return entityMove{MapID: mapID, Kind: viewNpc, ID: n.ID, From: from, To: n.cell, Fresh: fresh,
		Changed: fresh || from != n.cell, Move: buildNpcMove(n, 0), Enter: buildNpcEnter(n)}
}

// mobCorpseDelay is CMob::Damage's death tick: the corpse stays 5 s.
const mobCorpseDelay = 5 * time.Second

// visibleMob: alive, or dead with its corpse still shown.
func (m *MapMob) visible() bool { return m.CurrentHP > 0 || (!m.diedAt.IsZero() && !m.corpseGone) }

// TickMobCorpses is CZone::UpdateMobDeath: corpses older than 5 s leave
// the view (0x0602).
func TickMobCorpses() []entityMove {
	now := time.Now()
	var out []entityMove
	for _, mapID := range shardIDs() {
		s := shard(mapID)
		s.mu.Lock()
		for i := range s.entities.Mobs {
			m := &s.entities.Mobs[i]
			if m.CurrentHP == 0 && !m.diedAt.IsZero() && !m.corpseGone && now.Sub(m.diedAt) >= mobCorpseDelay {
				m.corpseGone = true
				out = append(out, entityMove{MapID: mapID, Kind: viewMob, ID: m.ID})
			}
		}
		s.mu.Unlock()
	}
	return out
}

// SetMobAggro is CMob::Damage's ledger and CMob::SetTarget for a player's
// hit: the attacker's aggro grows by the damage plus ten times the skill's
// aggro, and an idle mob takes the attacker as its target. CMob::AttackAI
// later turns to the top of the ledger (ReTargetByAggro).
func SetMobAggro(mapID uint16, id, characterID uint32) { AddMobAggro(mapID, id, characterID, 0) }

func AddMobAggro(mapID uint16, id, characterID uint32, amount int64) {
	s := shard(mapID)
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.entities.Mobs {
		if m := &s.entities.Mobs[i]; m.ID == id && m.CurrentHP > 0 {
			if m.aggro == nil {
				m.aggro = map[uint32]int64{}
			}
			m.aggro[characterID] += amount
			if m.AggroTarget == 0 {
				m.AggroTarget, m.returningHome = characterID, false
			}
		}
	}
}

// ClearMobAggro is CMob::Attack after killing its target: back to idle
// (status 0) with the dead user's aggro gone; UpdateIdle walks it home
// when it is more than 10 units away.
func ClearMobAggro(mapID uint16, id uint32) {
	s := shard(mapID)
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.entities.Mobs {
		if m := &s.entities.Mobs[i]; m.ID == id {
			delete(m.aggro, m.AggroTarget)
			m.dropTarget(false)
		}
	}
}

// Mob combat as CMob::UpdateIdle / UpdateAttack / Chase / ReturnHome run it.
const (
	mobLeash        = 45.0 // target farther than this from home or from the mob
	mobHomeDone     = 10.0 // ReturnHome completes within 10 units of home
	mobChaseReached = 1.5  // Chase: within Size + range + 1.5 no step
	mobRecoverEvery = 5 * time.Second
)

// TickMobCombat advances targeting, attacks, chasing and walking home.
func TickMobCombat(players []PlayerCombatSnapshot, maxPerMap int) MobCombatTick {
	var tick MobCombatTick
	now := time.Now()
	byMap := map[uint16][]PlayerCombatSnapshot{}
	for _, p := range players {
		byMap[p.MapID] = append(byMap[p.MapID], p)
	}
	for mapID, mapPlayers := range byMap {
		s := shard(mapID)
		s.mu.Lock()
		actions := 0
		for i := range s.entities.Mobs {
			if actions >= maxPerMap {
				break
			}
			m := &s.entities.Mobs[i]
			if m.CurrentHP == 0 {
				m.AggroTarget, m.returningHome, m.aggro = 0, false, nil
				continue
			}
			if r := m.recover(now); r != nil {
				tick.Restores = append(tick.Restores, mapPacket{mapID, m.PosX, m.PosZ, r})
			}
			mods := m.mods()
			if mods.VisionFix != 0 {
				for _, p := range mapPlayers {
					if p.CharacterID == mods.VisionFix && p.Alive {
						m.AggroTarget, m.returningHome = mods.VisionFix, false
					}
				}
			}
			if !mods.canAct() {
				continue
			}
			target := m.liveTarget(mapPlayers)
			if m.AggroTarget != 0 && target == nil {
				m.AggroTarget = 0 // gone, dead or on another map: back to idle
			}
			if target != nil && mods.VisionFix == 0 {
				switch {
				case DistanceXZ(target.PosX, target.PosZ, m.homeX, m.homeZ) > mobLeash:
					m.dropTarget(true)
					target = nil
				case DistanceXZ(target.PosX, target.PosZ, m.PosX, m.PosZ) > mobLeash:
					m.dropTarget(false)
					target = nil
				}
			}
			if target == nil && !m.returningHome {
				target = m.findTarget(now, mapPlayers)
			}
			if target == nil {
				if m.returningHome && m.stepHome(now, mapID, &mods, &tick) {
					actions++
				}
				continue
			}
			d := DistanceXZ(m.PosX, m.PosZ, target.PosX, target.PosZ)
			if d < m.attackReach() && !now.Before(m.nextAttackAt) {
				m.retargetByAggro(mapPlayers)
				if t := m.liveTarget(mapPlayers); t != nil {
					target = t
					d = DistanceXZ(m.PosX, m.PosZ, target.PosX, target.PosZ)
				}
				slot, ok := m.pickAttackSlot(now, d, &mods)
				if !ok {
					// No slot ready: CMob::AttackAI waits 2.5 s.
					m.nextAttackAt = now.Add(mobAttackIdle + mods.attackDelayOffset())
					continue
				}
				m.lastAttackAt = now
				actions++
				if heal := m.useSlot(slot, now); heal != nil {
					tick.Restores = append(tick.Restores, mapPacket{mapID, m.PosX, m.PosZ, heal})
					continue
				}
				tick.Attacks = append(tick.Attacks, MobAttackAction{m.ID, m.MobID, m.Attack, m.AggroTarget, slot, m.PosX, m.PosZ})
				continue
			}
			if m.stepChase(now, mapID, target, d, &mods, &tick) {
				actions++
			}
		}
		s.mu.Unlock()
	}
	return tick
}

func (m *MapMob) liveTarget(players []PlayerCombatSnapshot) *PlayerCombatSnapshot {
	if m.AggroTarget == 0 {
		return nil
	}
	for k := range players {
		if players[k].CharacterID == m.AggroTarget && players[k].Alive {
			return &players[k]
		}
	}
	return nil
}

// dropTarget: status 0 (idle) or 3 (walk home).
func (m *MapMob) dropTarget(home bool) {
	m.AggroTarget, m.lastAttackAt, m.nextAttackAt = 0, time.Time{}, time.Time{}
	m.returningHome = home
}

// findTarget is CMob::FindTarget for AI 1 and 3: the first living user of
// another country within ChaseRange, cell by cell over the 3x3 cells
// around the mob.
func (m *MapMob) findTarget(now time.Time, players []PlayerCombatSnapshot) *PlayerCombatSnapshot {
	if m.def == nil || (m.AI != 1 && m.AI != 3) || m.ChaseRange <= 0 || now.Before(m.noTargetUntil) {
		return nil
	}
	for dz := int32(-1); dz <= 1; dz++ {
		for dx := int32(-1); dx <= 1; dx++ {
			c := cell{m.cell.x + dx, m.cell.z + dz}
			for k := range players {
				p := &players[k]
				if p.Cell != c || !p.Alive || (m.def.Country < 2 && p.Faction == m.def.Country) {
					continue
				}
				if DistanceXZ(p.PosX, p.PosZ, m.PosX, m.PosZ) <= m.ChaseRange {
					m.AggroTarget = p.CharacterID
					return p
				}
			}
		}
	}
	return nil
}

// retargetByAggro is CMob::ReTargetByAggro: the living attacker on the map
// with the most aggro.
func (m *MapMob) retargetByAggro(players []PlayerCombatSnapshot) {
	var best uint32
	var top int64 = -1
	for k := range players {
		p := &players[k]
		if v, ok := m.aggro[p.CharacterID]; ok && p.Alive && v > top {
			best, top = p.CharacterID, v
		}
	}
	if best != 0 {
		m.AggroTarget = best
	}
}

// stepChase is CMob::Chase: ChaseStep (plus the speed offset) every
// ChaseTime toward the target, stopping just inside attack reach.
func (m *MapMob) stepChase(now time.Time, mapID uint16, t *PlayerCombatSnapshot, d float32, mods *mobMods, tick *MobCombatTick) bool {
	if now.Before(m.nextChaseAt) || !mods.canMove() || m.def == nil {
		return false
	}
	reach := float32(m.def.Size) + float32(m.def.Attacks[0].Range)
	if d < reach+mobChaseReached && d < m.attackReach() {
		return false
	}
	step := m.chaseStep(mods)
	interval := m.chaseInterval()
	stop := reach + 0.2 // inside CMob::EnableAttackRange's Size + range + 0.3
	remaining := d - stop
	if remaining <= 0 {
		return false
	}
	if remaining < step {
		m.nextChaseAt = now.Add(time.Duration(float64(remaining) / float64(step) * float64(interval)))
		step = remaining
	} else {
		m.nextChaseAt = now.Add(interval)
	}
	nx := m.PosX + (t.PosX-m.PosX)/d*step
	nz := m.PosZ + (t.PosZ-m.PosZ)/d*step
	if !IsMapWalkable(mapID, nx, nz) {
		// No straight line (the original then paths around walls; not
		// modeled): give up and walk home.
		m.dropTarget(true)
		return false
	}
	m.PosX, m.PosZ = nx, nz
	tick.Moves = append(tick.Moves, m.relocate(mapID, mobMotionRun))
	return true
}

// stepHome is CMob::ReturnHome: ChaseStep every ChaseTime toward the spawn
// point; within 10 units the return completes, clearing the applied
// skills and the damage ledger (HP is not refilled: UpdateRecover does it).
func (m *MapMob) stepHome(now time.Time, mapID uint16, mods *mobMods, tick *MobCombatTick) bool {
	if now.Before(m.nextChaseAt) {
		return false
	}
	d := DistanceXZ(m.PosX, m.PosZ, m.homeX, m.homeZ)
	if d < mobHomeDone {
		m.returningHome, m.lastAttackAt, m.Effects, m.aggro = false, time.Time{}, nil, nil
		return false
	}
	if !mods.canMove() {
		return false
	}
	step := m.chaseStep(mods)
	interval := m.chaseInterval()
	remaining := d - (float32(m.def.Size) + 1)
	if remaining < step {
		m.nextChaseAt = now.Add(time.Duration(float64(max(remaining, 0)) / float64(step) * float64(interval)))
		m.PosX, m.PosZ = m.homeX, m.homeZ
	} else {
		m.nextChaseAt = now.Add(interval)
		nx := m.PosX + (m.homeX-m.PosX)/d*step
		nz := m.PosZ + (m.homeZ-m.PosZ)/d*step
		if IsMapWalkable(mapID, nx, nz) {
			m.PosX, m.PosZ = nx, nz
		} else {
			m.PosX, m.PosZ = m.homeX, m.homeZ
		}
	}
	tick.Moves = append(tick.Moves, m.relocate(mapID, mobMotionWalk))
	return true
}

// recover is CMob::UpdateRecover: every 5 s a wounded mob regains its
// RecoverValue (0x0610 to 60 units).
func (m *MapMob) recover(now time.Time) []byte {
	if m.def == nil || m.def.RecoverValue <= 0 || m.CurrentHP >= m.MaxHP || now.Before(m.recoverAt) {
		return nil
	}
	m.recoverAt = now.Add(mobRecoverEvery)
	m.CurrentHP = min(m.CurrentHP+uint32(m.def.RecoverValue), m.MaxHP)
	return buildMobCurrentHP(m.ID, m.CurrentHP)
}

func AddEntities(mapID uint16, e MapEntities) {
	s := shard(mapID)
	s.mu.Lock()
	defer s.mu.Unlock()
	s.entities.NPCs = append(s.entities.NPCs, e.NPCs...)
	s.entities.Mobs = append(s.entities.Mobs, e.Mobs...)
	s.entities.Portals = append(s.entities.Portals, e.Portals...)
	s.entities.SpawnAreas = append(s.entities.SpawnAreas, e.SpawnAreas...)
}

func FindNearestSpawnArea(mapID uint16, faction uint8, x, z float32) (MapSpawnArea, bool) {
	var best MapSpawnArea
	found := false
	for _, a := range EntitiesOnMap(mapID).SpawnAreas {
		if a.allowsFaction(faction) && (!found || a.distanceSq(x, z) < best.distanceSq(x, z)) {
			best, found = a, true
		}
	}
	return best, found
}

func FindAnySpawnArea(mapID uint16, faction uint8) (MapSpawnArea, bool) {
	for _, a := range EntitiesOnMap(mapID).SpawnAreas {
		if a.allowsFaction(faction) {
			return a, true
		}
	}
	return MapSpawnArea{}, false
}

func FindPortalByIndex(mapID uint16, index uint8) (MapPortal, bool) {
	for _, p := range EntitiesOnMap(mapID).Portals {
		if p.Index == index {
			return p, true
		}
	}
	return MapPortal{}, false
}

func FindPortalAt(mapID uint16, x, y, z float32) (MapPortal, bool) {
	for _, p := range EntitiesOnMap(mapID).Portals {
		if p.Contains(x, y, z) {
			return p, true
		}
	}
	return MapPortal{}, false
}

func NewPatrolNpc(npcType uint8, typeID uint16, points []NpcPatrolPoint) (MapNpc, bool) {
	if len(points) == 0 {
		return MapNpc{}, false
	}
	p := points[0]
	return MapNpc{ID: nextNpcID.Add(1) - 1, Type: npcType, TypeID: typeID, PosX: p.X, PosY: p.Y, PosZ: p.Z,
		Angle: p.Angle, patrol: points, cell: cellFor(p.X, p.Z), hasCell: true, patrolIndex: min(1, len(points)-1),
		nextMoveAt: time.Now().Add(time.Duration(10000+rand.IntN(10000)) * time.Millisecond)}, true
}

// NewStaticMob builds a mob from its definition; respawn 0 never respawns.
func NewStaticMob(def *db.MobDef, mobID uint16, x, z float32, area *MobArea, respawnSeconds uint64) MapMob {
	m := MapMob{ID: nextMobID.Add(1) - 1, MobID: mobID, Level: 1, PosX: x, PosZ: z, homeX: x, homeZ: z,
		MaxHP: 100, respawnDelay: time.Duration(respawnSeconds) * time.Second}
	if def != nil {
		m.Level, m.MaxHP, m.MoneyMin, m.MoneyMax = def.Level, max(def.MaxHP, 1), def.MoneyMin, def.MoneyMax
		m.Exp, m.Attack, m.AI, m.ChaseRange = def.Exp, def.Attack, def.AI, def.ChaseRange
		m.def = def
	}
	// CMob::Init: the first wander comes within a minute.
	m.nextMoveAt = time.Now().Add(time.Duration(rand.IntN(60000)) * time.Millisecond)
	m.CurrentHP = m.MaxHP
	m.cell, m.hasCell = cellFor(x, z), true
	m.noTargetUntil = time.Now().Add(5 * time.Second)
	if area != nil {
		a := area.normalized()
		m.Area = &a
	}
	return m
}

func (m *MapMob) pickRespawnPosition(mapID uint16) (float32, float32) {
	if m.Area == nil {
		return m.homeX, m.homeZ
	}
	a := m.Area.normalized()
	m.roamStep++
	for attempt := uint32(0); attempt < 64; attempt++ {
		seed := m.ID + m.roamStep*97 + attempt
		x := a.LowerX + (a.UpperX-a.LowerX)*float32(seed%997)/996
		z := a.LowerZ + (a.UpperZ-a.LowerZ)*float32((seed/997)%991)/990
		if roamWalkable(mapID, x, z, a) {
			return x, z
		}
	}
	x, z := a.clampX(m.homeX), a.clampZ(m.homeZ)
	if roamWalkable(mapID, x, z, a) {
		return x, z
	}
	return m.homeX, m.homeZ
}

// mobSpeedOffsets are the original move-speed step offsets (0x00542538);
// mobs start at speed 2 (no offset).
var mobSpeedOffsets = [5]float32{-4, -2, 0, 2, 4}

const mobHomeRadius = 10.0

// advanceRoam is the idle state's wander (update_mob_ai_idle): every
// 10 s + NormalTime + up to 30 s, a step of NormalStep + 0..4 units in a
// random direction, staying within 10 units of home.
func (m *MapMob) advanceRoam(mapID uint16) bool {
	now := time.Now()
	if m.def == nil || now.Before(m.nextMoveAt) {
		return false
	}
	if DistanceXZ(m.PosX, m.PosZ, m.homeX, m.homeZ) > mobHomeRadius {
		m.returningHome = true // CMob::UpdateIdle: status 3
		return false
	}
	pause := 10*time.Second + time.Duration(rand.IntN(30000))*time.Millisecond
	if m.def.NormalStep == 0 || m.AI == 4 {
		m.nextMoveAt = now.Add(pause)
		return false
	}
	m.nextMoveAt = now.Add(pause + time.Duration(m.def.NormalTimeMS)*time.Millisecond)
	mods := m.mods()
	length := float32(m.def.NormalStep) + mobSpeedOffsets[min(max(2+mods.MoveSpeed, 0), 4)] + float32(rand.IntN(500)/100)
	angle := float64(rand.IntN(10000)) * math.Pi / 180
	nx := m.PosX - float32(math.Sin(angle))*length
	nz := m.PosZ + float32(math.Cos(angle))*length
	if DistanceXZ(nx, nz, m.homeX, m.homeZ) >= mobHomeRadius || !IsMapWalkable(mapID, nx, nz) {
		return false
	}
	m.PosX, m.PosZ = nx, nz
	return true
}

// chaseStep and chaseInterval are ChaseStep / ChaseTime (used both to
// chase and to walk home).
func (m *MapMob) chaseStep(mods *mobMods) float32 {
	if m.def == nil {
		return 1
	}
	idx := min(max(2+mods.MoveSpeed, 0), 4)
	return max(float32(m.def.ChaseStep)+mobSpeedOffsets[idx], 1)
}

func (m *MapMob) chaseInterval() time.Duration {
	if m.def == nil || m.def.ChaseTimeMS == 0 {
		return time.Second
	}
	return time.Duration(m.def.ChaseTimeMS) * time.Millisecond
}

// attackReach is vertical_mobs_attack_range: Size + Attackrange + 0.3.
func (m *MapMob) attackReach() float32 {
	if m.def == nil {
		return MobAttackRange
	}
	return float32(m.def.Size) + float32(m.def.Attacks[0].Range) + 0.3
}

// mobAttackIdle is AttackAI's wait when no slot is ready (2500 ms plus
// g_ConstMobAttackSpeed, 0 at normal speed).
const mobAttackIdle = 2500 * time.Millisecond

// pickAttackSlot is CMob::AttackAI's scan: slots 3 to 1, the first one in
// use (Attackok), off its own cooldown, within its HP condition (3: HP
// 20-40%, 4: HP at most 5%, 5: HP under 20%) and in reach.
func (m *MapMob) pickAttackSlot(now time.Time, distance float32, mods *mobMods) (uint8, bool) {
	if m.def == nil {
		return 0, true
	}
	maxHP := float64(float32(m.MaxHP))
	threshold := func(f float32) uint32 { return uint32(int32(maxHP * float64(f))) }
	for slot := 2; slot >= 0; slot-- {
		a := &m.def.Attacks[slot]
		if a.OK == 0 || now.Before(m.slotReadyAt[slot]) {
			continue
		}
		switch a.OK {
		case 3:
			if m.CurrentHP > threshold(0.4) || m.CurrentHP < threshold(0.2) {
				continue
			}
		case 4:
			if m.CurrentHP > threshold(0.05) {
				continue
			}
		case 5:
			if m.CurrentHP >= threshold(0.2) {
				continue
			}
		}
		reach := float32(m.def.Size) + float32(a.Range) + 0.3
		if a.Type == 0 && mods.Disabled[0] > 0 {
			continue // CMob::Attack: m_pAttack[0].nDisable
		}
		if a.Type != 0 {
			if sk := db.MustDefs().Skill(a.Type, 100); sk != nil {
				if mods.Disabled[sk.Kind] > 0 {
					continue
				}
				if sk.TypeEffect == 2 {
					return uint8(slot), true // self heal: no reach needed
				}
				reach = float32(m.def.Size) + float32(sk.AttackRange) + 0.3
			}
		}
		if distance > reach {
			continue
		}
		return uint8(slot), true
	}
	return 0, false
}

// useSlot starts a slot's cooldown and the next attack time. A mob heal
// skill (TypeEffect 2) applies at once and returns its HP packet.
func (m *MapMob) useSlot(slot uint8, now time.Time) []byte {
	a := &m.def.Attacks[slot]
	if a.Type != 0 {
		if sk := db.MustDefs().Skill(a.Type, 100); sk != nil {
			m.slotReadyAt[slot] = now.Add(time.Duration(sk.ResetTimeSeconds) * time.Second)
			m.nextAttackAt = now.Add(mobAttackIdle + m.mods().attackDelayOffset())
			if sk.TypeEffect == 2 {
				m.CurrentHP = min(satAdd(m.CurrentHP, uint32(max(sk.HealHP, 0))), m.MaxHP)
				return buildMobCurrentHP(m.ID, m.CurrentHP)
			}
			return nil
		}
	}
	// CMob::Attack: the slot waits its own AttackTime, the mob slot 1's.
	offset := m.mods().attackDelayOffset()
	m.slotReadyAt[slot] = now.Add(time.Duration(a.TimeMS)*time.Millisecond + offset)
	m.nextAttackAt = now.Add(m.attackInterval() + offset)
	return nil
}

// movable: not stunned, asleep or stopped.
func (m *MapMob) movable() bool {
	o := m.mods()
	return o.canMove()
}

func (m *MapMob) attackInterval() time.Duration {
	if m.def == nil || m.def.Attacks[0].TimeMS == 0 {
		return MobAttackInterval
	}
	return time.Duration(m.def.Attacks[0].TimeMS) * time.Millisecond
}

func roamWalkable(mapID uint16, x, z float32, a MobArea) bool {
	return a.contains(x, z) && IsMapWalkable(mapID, x, z)
}

// Packet builders.

func buildNpcEnter(n *MapNpc) []byte {
	w := packet.New(packet.MapNpcEnter)
	w.U32(n.ID)
	w.U8(n.Type)
	w.U16(n.TypeID)
	w.F32(n.PosX)
	w.F32(n.PosY)
	w.F32(n.PosZ)
	w.U16(n.Angle)
	return w.MustFinish()
}

func buildNpcLeave(id uint32) []byte {
	w := packet.New(packet.MapNpcLeave)
	w.U32(id)
	return w.MustFinish()
}

func buildNpcMove(n *MapNpc, motion uint8) []byte {
	w := packet.New(packet.MapNpcMove)
	w.U32(n.ID)
	w.U8(motion)
	w.F32(n.PosX)
	w.F32(n.PosY)
	w.F32(n.PosZ)
	return w.MustFinish()
}

func buildMobEnter(m *MapMob, isNew bool) []byte {
	w := packet.New(packet.MobEnter)
	w.U32(m.ID)
	w.Bool(isNew)
	w.U16(m.MobID)
	w.F32(m.PosX)
	w.F32(m.PosZ)
	return w.MustFinish()
}

func buildMobMove(m *MapMob, motion uint8) []byte {
	w := packet.New(packet.MobMove)
	w.U32(m.ID)
	w.U8(motion)
	w.F32(m.PosX)
	w.F32(m.PosZ)
	return w.MustFinish()
}

func buildMobLeave(id uint32) []byte {
	w := packet.New(packet.MobLeave)
	w.U32(id)
	return w.MustFinish()
}

// buildMobTargetHP (0x0305): mob id, HP (client reader 0x0058f2d0).
func buildMobTargetHP(m *MapMob) []byte {
	w := packet.New(packet.TargetMobHpUpdate)
	w.U32(m.ID)
	w.I32(int32(min(m.CurrentHP, math.MaxInt32)))
	return w.MustFinish()
}

// buildMobState (0x0304): mob id, HP, attack speed, move speed.
func buildMobState(m *MapMob) []byte {
	w := packet.New(packet.TargetMobGetState)
	w.U32(m.ID)
	w.I32(int32(min(m.CurrentHP, math.MaxInt32)))
	w.U8(5) // attack speed: normal
	w.U8(2) // move speed: normal
	return w.MustFinish()
}

func buildCharacterMobAttack(characterID, mobID uint32, result uint8, damage uint16) []byte {
	w := packet.New(packet.CharacterMobAutoAttack)
	w.U8(result)
	w.U32(characterID)
	w.U32(mobID)
	w.U16(damage)
	w.U16(0)
	w.U16(0)
	return w.MustFinish()
}

func buildMobAttackCharacter(result uint8, mobID, characterID uint32, damage uint16) []byte {
	w := packet.New(packet.MobAttackCharacter)
	w.U8(result)
	w.U32(mobID)
	w.U32(characterID)
	w.U16(damage)
	w.U16(0)
	w.U16(0)
	return w.MustFinish()
}

func buildCharacterDeath(characterID uint32, deathType uint8, killerID uint32) []byte {
	w := packet.New(packet.CharacterDeath)
	w.U32(characterID)
	w.U8(deathType)
	w.U32(killerID)
	return w.MustFinish()
}

func buildCharacterDeathState(characterID uint32) []byte {
	w := packet.New(packet.CharacterDeathState)
	w.U32(characterID)
	w.U8(0x07)
	w.U8(0x02)
	return w.MustFinish()
}

func buildCharacterReviveInit(characterID uint32) []byte {
	w := packet.New(packet.CharacterReviveInit)
	w.U32(characterID)
	return w.MustFinish()
}

func buildCharacterResurrect(characterID uint32, x, y, z float32) []byte {
	w := packet.New(packet.CharacterResurrect)
	w.U32(characterID)
	w.Bytes([]byte{0x04, 0x46, 0xAF, 0x37, 0x01})
	w.F32(x)
	w.F32(y)
	w.F32(z)
	return w.MustFinish()
}

func buildCharacterReviveHitpoints(characterID, hp, mp, sp uint32) []byte {
	w := packet.New(packet.CharacterReviveHitpoints)
	w.U32(characterID)
	w.U32(hp)
	w.U32(mp)
	w.U32(sp)
	return w.MustFinish()
}

func buildMobDeath(mobID, killerID uint32) []byte {
	w := packet.New(packet.MobDeath)
	w.U32(mobID)
	w.U8(1)
	w.U32(killerID)
	return w.MustFinish()
}

func buildMobPeriodicDamage(mobID uint32, skillID uint16, level uint8, hp, sp, mp uint16) []byte {
	w := packet.New(packet.MobPeriodicDamage)
	w.U32(mobID)
	w.U16(skillID)
	w.U8(level)
	w.U16(hp)
	w.U16(sp)
	w.U16(mp)
	return w.MustFinish()
}

func buildMobCurrentHP(mobID, hp uint32) []byte {
	w := packet.New(packet.MobCurrentHitpoints)
	w.U32(mobID)
	w.U32(hp)
	return w.MustFinish()
}

// HealMob restores HP to a living mob (a drain's heal); returns the mob.
func HealMob(mapID uint16, id, amount uint32) (MapMob, bool) {
	s := shard(mapID)
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.entities.Mobs {
		m := &s.entities.Mobs[i]
		if m.ID == id && m.CurrentHP > 0 {
			m.CurrentHP = min(satAdd(m.CurrentHP, amount), m.MaxHP)
			return *m, true
		}
	}
	return MapMob{}, false
}
