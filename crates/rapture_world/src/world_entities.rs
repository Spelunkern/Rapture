use anyhow::Result;
use rapture_common::packet::{Opcode, PacketWriter};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub(crate) struct MapNpc {
    pub(crate) id: u32,
    pub(crate) npc_type: u8,
    pub(crate) type_id: u16,
    pub(crate) pos_x: f32,
    pub(crate) pos_y: f32,
    pub(crate) pos_z: f32,
    pub(crate) angle: u16,
    patrol_points: Vec<NpcPatrolPoint>,
    patrol_index: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NpcPatrolPoint {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) z: f32,
    pub(crate) angle: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct MapMob {
    pub(crate) id: u32,
    pub(crate) mob_id: u16,
    pub(crate) pos_x: f32,
    pub(crate) pos_z: f32,
    home_x: f32,
    home_z: f32,
    pub(crate) current_hp: u32,
    pub(crate) max_hp: u32,
    pub(crate) money_min: u32,
    pub(crate) money_max: u32,
    pub(crate) exp: u32,
    pub(crate) area: Option<MapMobArea>,
    respawn_delay: Option<Duration>,
    respawn_at: Option<Instant>,
    roam_step: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MapMobArea {
    pub(crate) lower_x: f32,
    pub(crate) lower_z: f32,
    pub(crate) upper_x: f32,
    pub(crate) upper_z: f32,
}

impl MapMobArea {
    pub(crate) fn normalized(self) -> Self {
        Self {
            lower_x: self.lower_x.min(self.upper_x),
            lower_z: self.lower_z.min(self.upper_z),
            upper_x: self.lower_x.max(self.upper_x),
            upper_z: self.lower_z.max(self.upper_z),
        }
    }

    fn clamp_x(self, x: f32) -> f32 {
        x.clamp(self.lower_x, self.upper_x)
    }

    fn clamp_z(self, z: f32) -> f32 {
        z.clamp(self.lower_z, self.upper_z)
    }

    fn contains(self, x: f32, z: f32) -> bool {
        x >= self.lower_x && x <= self.upper_x && z >= self.lower_z && z <= self.upper_z
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MapPortal {
    pub(crate) index: u8,
    pub(crate) portal_id: i32,
    pub(crate) faction_or_portal_id: i32,
    pub(crate) min_level: u16,
    pub(crate) max_level: u16,
    pub(crate) target_map_id: u16,
    pub(crate) pos_x: f32,
    pub(crate) pos_y: f32,
    pub(crate) pos_z: f32,
    pub(crate) target_x: f32,
    pub(crate) target_y: f32,
    pub(crate) target_z: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct MapSpawnArea {
    pub(crate) faction: i32,
    pub(crate) lower_x: f32,
    pub(crate) lower_y: f32,
    pub(crate) lower_z: f32,
    pub(crate) upper_x: f32,
    pub(crate) upper_y: f32,
    pub(crate) upper_z: f32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct MapEntities {
    pub(crate) npcs: Vec<MapNpc>,
    pub(crate) mobs: Vec<MapMob>,
    pub(crate) portals: Vec<MapPortal>,
    pub(crate) spawn_areas: Vec<MapSpawnArea>,
}

#[derive(Clone, Debug)]
struct MapGrid {
    map_size: u32,
    cell_size: u32,
    map_mask: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EntityVisibility {
    npc_ids: HashSet<u32>,
    mob_ids: HashSet<u32>,
}

#[derive(Clone, Debug)]
pub(crate) struct MobDamageResult {
    pub(crate) mob: MapMob,
    pub(crate) damage_hp: u16,
    pub(crate) applied_hp: u32,
    pub(crate) killed: bool,
    pub(crate) respawn_seconds: Option<u64>,
}

// Sharded per map: the outer mutex only ever guards the HashMap's shape
// (inserting a handle for a map seen for the first time), so operations on
// different maps never block each other. Movement on map A and a roam tick
// on map B can proceed fully in parallel.
fn registry() -> &'static Mutex<HashMap<u16, Arc<Mutex<MapEntities>>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u16, Arc<Mutex<MapEntities>>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn map_handle(map_id: u16) -> Arc<Mutex<MapEntities>> {
    let mut registry = lock_or_recover(registry());
    registry.entry(map_id).or_default().clone()
}

/// Recovers from a poisoned lock instead of panicking, so one thread's
/// panic while holding this lock can't cascade into every other thread
/// that touches the same shared state.
pub(crate) fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn roam_cursor() -> &'static AtomicU32 {
    static ROAM_CURSOR: AtomicU32 = AtomicU32::new(0);
    &ROAM_CURSOR
}

fn grids() -> &'static Mutex<HashMap<u16, MapGrid>> {
    static GRIDS: OnceLock<Mutex<HashMap<u16, MapGrid>>> = OnceLock::new();
    GRIDS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_npc_id() -> u32 {
    static NEXT_NPC_ID: AtomicU32 = AtomicU32::new(0x7000_0000);
    NEXT_NPC_ID.fetch_add(1, Ordering::Relaxed)
}

fn next_mob_id() -> u32 {
    static NEXT_MOB_ID: AtomicU32 = AtomicU32::new(0x7100_0000);
    NEXT_MOB_ID.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn entities_on_map(map_id: u16) -> MapEntities {
    lock_or_recover(&map_handle(map_id)).clone()
}

pub(crate) fn register_map_grid(map_id: u16, map_size: u32, cell_size: u32, map_mask: Vec<u8>) {
    let cell_size = if cell_size == 0 { 128 } else { cell_size };
    lock_or_recover(grids()).insert(
        map_id,
        MapGrid {
            map_size,
            cell_size,
            map_mask,
        },
    );
}

/// Entities within one grid cell of `(x, z)`. Unlike a naive "clone the
/// whole map then filter" pass, this clones only the entities that actually
/// pass the neighbor-cell check, so the cost scales with what's visible
/// rather than with everything on the map.
pub(crate) fn entities_near(map_id: u16, x: f32, z: f32) -> MapEntities {
    let Some(grid) = lock_or_recover(grids()).get(&map_id).cloned() else {
        return entities_on_map(map_id);
    };

    let viewer_cell = cell_for(x, z, grid.cell_size);
    let handle = map_handle(map_id);
    let entities = lock_or_recover(&handle);
    MapEntities {
        npcs: entities
            .npcs
            .iter()
            .filter(|npc| {
                is_neighbor_cell(viewer_cell, cell_for(npc.pos_x, npc.pos_z, grid.cell_size))
            })
            .cloned()
            .collect(),
        mobs: entities
            .mobs
            .iter()
            .filter(|mob| mob.current_hp > 0)
            .filter(|mob| {
                is_neighbor_cell(viewer_cell, cell_for(mob.pos_x, mob.pos_z, grid.cell_size))
            })
            .cloned()
            .collect(),
        portals: Vec::new(),
        spawn_areas: Vec::new(),
    }
}

pub(crate) fn is_map_walkable(map_id: u16, x: f32, z: f32) -> bool {
    let Some(grid) = lock_or_recover(grids()).get(&map_id).cloned() else {
        return true;
    };
    is_walkable_in_mask(grid.map_size, &grid.map_mask, x, z)
}

pub(crate) fn is_walkable_in_mask(map_size: u32, map_mask: &[u8], x: f32, z: f32) -> bool {
    if map_size == 0 || map_mask.is_empty() || !x.is_finite() || !z.is_finite() {
        return true;
    }
    if x < 0.0 || z < 0.0 {
        return false;
    }

    let x = x.floor() as u32;
    let z = z.floor() as u32;
    if x >= map_size || z >= map_size {
        return false;
    }

    let bit_index = z as usize * map_size as usize + x as usize;
    let byte_index = bit_index / 8;
    let Some(byte) = map_mask.get(byte_index) else {
        return false;
    };
    let bit = bit_index % 8;
    ((byte >> bit) & 1) == 0
}

pub(crate) fn visibility_from_entities(entities: &MapEntities) -> EntityVisibility {
    EntityVisibility {
        npc_ids: entities.npcs.iter().map(|npc| npc.id).collect(),
        mob_ids: entities.mobs.iter().map(|mob| mob.id).collect(),
    }
}

pub(crate) fn build_entity_visibility_delta(
    map_id: u16,
    x: f32,
    z: f32,
    visibility: &mut EntityVisibility,
) -> Result<Vec<(Vec<u8>, &'static str)>> {
    let entities = entities_near(map_id, x, z);
    let next = visibility_from_entities(&entities);
    let mut packets = Vec::new();

    for npc_id in visibility.npc_ids.difference(&next.npc_ids) {
        packets.push((build_npc_leave(*npc_id)?, "0x0E02 npc leave"));
    }
    for mob_id in visibility.mob_ids.difference(&next.mob_ids) {
        packets.push((build_mob_leave(*mob_id)?, "0x0602 mob leave"));
    }

    for npc in &entities.npcs {
        if !visibility.npc_ids.contains(&npc.id) {
            packets.push((build_npc_enter(npc)?, "0x0E01 npc enter delta"));
        }
    }
    for mob in &entities.mobs {
        if !visibility.mob_ids.contains(&mob.id) {
            packets.push((build_mob_enter(mob, false)?, "0x0601 mob enter delta"));
        }
    }

    *visibility = next;
    Ok(packets)
}

pub(crate) fn build_entity_visibility_clear(
    visibility: &mut EntityVisibility,
) -> Result<Vec<(Vec<u8>, &'static str)>> {
    let mut packets = Vec::new();

    for npc_id in visibility.npc_ids.drain() {
        packets.push((build_npc_leave(npc_id)?, "0x0E02 npc leave"));
    }
    for mob_id in visibility.mob_ids.drain() {
        packets.push((build_mob_leave(mob_id)?, "0x0602 mob leave"));
    }

    Ok(packets)
}

pub(crate) fn mob_by_id(map_id: u16, mob_global_id: u32) -> Option<MapMob> {
    entities_on_map(map_id)
        .mobs
        .into_iter()
        .find(|mob| mob.id == mob_global_id)
}

pub(crate) fn damage_mob(
    map_id: u16,
    mob_global_id: u32,
    damage_hp: u32,
) -> Option<MobDamageResult> {
    let handle = map_handle(map_id);
    let mut entities = lock_or_recover(&handle);
    let mob = entities
        .mobs
        .iter_mut()
        .find(|mob| mob.id == mob_global_id)?;
    if mob.current_hp == 0 {
        return None;
    }

    let packet_damage_hp = damage_hp.min(u16::MAX as u32) as u16;
    let applied_hp = damage_hp.min(mob.current_hp);
    mob.current_hp = mob.current_hp.saturating_sub(applied_hp);
    let killed = mob.current_hp == 0;
    let respawn_seconds = if killed {
        if let Some(delay) = mob.respawn_delay {
            mob.respawn_at = Some(Instant::now() + delay);
            Some(delay.as_secs())
        } else {
            None
        }
    } else {
        None
    };

    Some(MobDamageResult {
        mob: mob.clone(),
        damage_hp: packet_damage_hp,
        applied_hp,
        killed,
        respawn_seconds,
    })
}

/// Shared "can see this" radius for mob/NPC AI visibility gating and for
/// filtering every other whole-map broadcast (character move/shape/
/// equipment, combat, item drop/pickup/despawn) down to nearby players only
/// -- matching Aries's real CZone::SendView spatial-cell view range instead
/// of notifying every player on the map regardless of distance.
pub(crate) const VISIBILITY_RANGE: f32 = 180.0;

pub(crate) fn distance_xz(left_x: f32, left_z: f32, right_x: f32, right_z: f32) -> f32 {
    let dx = left_x - right_x;
    let dz = left_z - right_z;
    (dx * dx + dz * dz).sqrt()
}

/// Buckets viewer positions into grid cells so proximity checks against many
/// entities don't each have to scan every viewer on the map. Cost to build
/// is O(viewers), same as one pass of the naive approach, but every
/// `is_near_any_bucketed` call afterwards is O(populated cells) instead of
/// O(all viewers) -- a real win once players cluster (towns, hunting spots).
fn bucket_viewers(viewers: &[(f32, f32)], cell_size: u32) -> HashMap<(i32, i32), Vec<(f32, f32)>> {
    let mut buckets: HashMap<(i32, i32), Vec<(f32, f32)>> = HashMap::new();
    for &(x, z) in viewers {
        buckets
            .entry(cell_for(x, z, cell_size))
            .or_default()
            .push((x, z));
    }
    buckets
}

fn is_near_any_bucketed(
    pos: (f32, f32),
    own_cell: (i32, i32),
    buckets: &HashMap<(i32, i32), Vec<(f32, f32)>>,
    radius: f32,
) -> bool {
    let radius_sq = radius * radius;
    buckets
        .iter()
        .filter(|(cell, _)| is_neighbor_cell(own_cell, **cell))
        .flat_map(|(_, viewers)| viewers.iter())
        .any(|(x, z)| {
            let dx = pos.0 - *x;
            let dz = pos.1 - *z;
            dx * dx + dz * dz <= radius_sq
        })
}

fn grid_cell_size(map_id: u16) -> u32 {
    lock_or_recover(grids())
        .get(&map_id)
        .map(|grid| grid.cell_size)
        .unwrap_or(128)
}

pub(crate) fn tick_mob_roam_for_viewers(
    viewers_by_map: &HashMap<u16, Vec<(f32, f32)>>,
    max_moves_per_map: usize,
) -> Result<Vec<(u16, f32, f32, Vec<u8>)>> {
    let mut moves = Vec::new();
    let cursor = roam_cursor().fetch_add(1, Ordering::Relaxed);

    for (map_id, viewers) in viewers_by_map {
        // Each map's lock is acquired (and released) independently, so a
        // roam tick on one map never blocks player movement on another.
        let handle = map_handle(*map_id);
        let mut entities = lock_or_recover(&handle);
        let cell_size = grid_cell_size(*map_id);
        let buckets = bucket_viewers(viewers, cell_size);
        let mut moved_on_map = 0usize;
        let mut visited_on_map = 0usize;

        for mob in &mut entities.mobs {
            if moved_on_map >= max_moves_per_map {
                break;
            }
            let mob_cell = cell_for(mob.pos_x, mob.pos_z, cell_size);
            if mob.current_hp == 0
                || mob.area.is_none()
                || !is_near_any_bucketed((mob.pos_x, mob.pos_z), mob_cell, &buckets, VISIBILITY_RANGE)
            {
                continue;
            }
            visited_on_map = visited_on_map.saturating_add(1);
            if !(visited_on_map as u32).wrapping_add(cursor).is_multiple_of(4) {
                continue;
            }

            if mob.advance_roam(*map_id) {
                moved_on_map = moved_on_map.saturating_add(1);
                moves.push((*map_id, mob.pos_x, mob.pos_z, build_mob_move(mob, 0)?));
            }
        }
    }

    Ok(moves)
}

pub(crate) fn tick_mob_respawns_for_viewers(
    viewers_by_map: &HashMap<u16, Vec<(f32, f32)>>,
    max_respawns_per_map: usize,
) -> Result<Vec<(u16, f32, f32, Vec<u8>)>> {
    let mut enters = Vec::new();
    let now = Instant::now();

    for (map_id, viewers) in viewers_by_map {
        let handle = map_handle(*map_id);
        let mut entities = lock_or_recover(&handle);
        let cell_size = grid_cell_size(*map_id);
        let buckets = bucket_viewers(viewers, cell_size);
        let mut respawned_on_map = 0usize;

        for mob in &mut entities.mobs {
            if respawned_on_map >= max_respawns_per_map {
                break;
            }
            if mob.current_hp > 0 {
                continue;
            }
            let Some(respawn_at) = mob.respawn_at else {
                continue;
            };
            if respawn_at > now {
                continue;
            }

            let (x, z) = mob.pick_respawn_position(*map_id);
            mob.pos_x = x;
            mob.pos_z = z;
            mob.home_x = x;
            mob.home_z = z;
            mob.current_hp = mob.max_hp.max(1);
            mob.respawn_at = None;
            respawned_on_map = respawned_on_map.saturating_add(1);

            let mob_cell = cell_for(mob.pos_x, mob.pos_z, cell_size);
            if is_near_any_bucketed((mob.pos_x, mob.pos_z), mob_cell, &buckets, VISIBILITY_RANGE) {
                enters.push((*map_id, mob.pos_x, mob.pos_z, build_mob_enter(mob, true)?));
            }
        }
    }

    Ok(enters)
}

pub(crate) fn tick_npc_patrol_for_viewers(
    viewers_by_map: &HashMap<u16, Vec<(f32, f32)>>,
    max_moves_per_map: usize,
) -> Result<Vec<(u16, f32, f32, Vec<u8>)>> {
    let mut moves = Vec::new();
    let cursor = roam_cursor().fetch_add(1, Ordering::Relaxed);

    for (map_id, viewers) in viewers_by_map {
        let handle = map_handle(*map_id);
        let mut entities = lock_or_recover(&handle);
        let cell_size = grid_cell_size(*map_id);
        let buckets = bucket_viewers(viewers, cell_size);
        let mut moved_on_map = 0usize;
        let mut visited_on_map = 0usize;

        for npc in &mut entities.npcs {
            if moved_on_map >= max_moves_per_map {
                break;
            }
            let npc_cell = cell_for(npc.pos_x, npc.pos_z, cell_size);
            if !npc.has_patrol()
                || !is_near_any_bucketed((npc.pos_x, npc.pos_z), npc_cell, &buckets, VISIBILITY_RANGE)
            {
                continue;
            }
            visited_on_map = visited_on_map.saturating_add(1);
            if !(visited_on_map as u32).wrapping_add(cursor).is_multiple_of(2) {
                continue;
            }

            if npc.advance_patrol() {
                moved_on_map = moved_on_map.saturating_add(1);
                moves.push((*map_id, npc.pos_x, npc.pos_z, build_npc_move(npc, 0)?));
            }
        }
    }

    Ok(moves)
}

fn cell_for(x: f32, z: f32, cell_size: u32) -> (i32, i32) {
    let cell_size = cell_size.max(1) as f32;
    (coord_cell(x, cell_size), coord_cell(z, cell_size))
}

fn coord_cell(value: f32, cell_size: f32) -> i32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    (value.round() / cell_size).floor() as i32
}

fn is_neighbor_cell(a: (i32, i32), b: (i32, i32)) -> bool {
    let dx = (a.0 - b.0).abs();
    let dz = (a.1 - b.1).abs();
    dx + dz <= 1
}

pub(crate) fn add_entities(map_id: u16, entities: MapEntities) {
    let handle = map_handle(map_id);
    let mut map_entities = lock_or_recover(&handle);
    map_entities.npcs.extend(entities.npcs);
    map_entities.mobs.extend(entities.mobs);
    map_entities.portals.extend(entities.portals);
    map_entities.spawn_areas.extend(entities.spawn_areas);
}

pub(crate) fn find_nearest_spawn_area(
    map_id: u16,
    faction: u8,
    x: f32,
    z: f32,
) -> Option<MapSpawnArea> {
    entities_on_map(map_id)
        .spawn_areas
        .into_iter()
        .filter(|area| area.allows_faction(faction))
        .min_by(|left, right| {
            left.distance_sq_to(x, z)
                .partial_cmp(&right.distance_sq_to(x, z))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// Any spawn area for `faction` on `map_id`, with no "nearest to" ranking.
/// Used when teleporting to a different map, where the traveler's old-map
/// position has no meaningful relationship to the destination's layout.
pub(crate) fn find_any_spawn_area(map_id: u16, faction: u8) -> Option<MapSpawnArea> {
    entities_on_map(map_id)
        .spawn_areas
        .into_iter()
        .find(|area| area.allows_faction(faction))
}

/// Scans the map's walkability mask for the first walkable tile, as a last
/// resort when a GM move command's target is invalid and the map has no
/// spawn area to fall back to either. Returns None if the map has no
/// registered mask at all (nothing to scan).
pub(crate) fn find_any_walkable_position(map_id: u16) -> Option<(f32, f32)> {
    let grid = lock_or_recover(grids()).get(&map_id).cloned()?;
    if grid.map_size == 0 || grid.map_mask.is_empty() {
        return None;
    }
    for z in 0..grid.map_size {
        for x in 0..grid.map_size {
            if is_walkable_in_mask(grid.map_size, &grid.map_mask, x as f32, z as f32) {
                return Some((x as f32, z as f32));
            }
        }
    }
    None
}

pub(crate) fn portals_on_map(map_id: u16) -> Vec<MapPortal> {
    lock_or_recover(&map_handle(map_id)).portals.clone()
}

pub(crate) fn find_portal_by_index(map_id: u16, index: u8) -> Option<MapPortal> {
    portals_on_map(map_id)
        .into_iter()
        .find(|portal| portal.index == index)
}

pub(crate) fn find_portal_at(map_id: u16, x: f32, y: f32, z: f32) -> Option<MapPortal> {
    portals_on_map(map_id)
        .into_iter()
        .find(|portal| portal.contains(x, y, z))
}

impl MapPortal {
    pub(crate) fn contains(&self, x: f32, y: f32, z: f32) -> bool {
        (self.pos_x - x).abs() <= 5.0
            && (self.pos_y - y).abs() <= 5.0
            && (self.pos_z - z).abs() <= 5.0
    }

    pub(crate) fn is_open(&self) -> bool {
        self.faction_or_portal_id <= 2
    }

    pub(crate) fn allows_faction(&self, faction: u8) -> bool {
        match self.faction_or_portal_id {
            0 => true,
            1 => faction == 0,
            2 => faction == 1,
            _ => true,
        }
    }

    pub(crate) fn allows_level(&self, level: u16) -> bool {
        level >= self.min_level && level <= self.max_level
    }
}

impl MapSpawnArea {
    pub(crate) fn center(&self) -> (f32, f32, f32) {
        (
            (self.lower_x + self.upper_x) * 0.5,
            (self.lower_y + self.upper_y) * 0.5,
            (self.lower_z + self.upper_z) * 0.5,
        )
    }

    fn distance_sq_to(&self, x: f32, z: f32) -> f32 {
        let (center_x, _, center_z) = self.center();
        let dx = center_x - x;
        let dz = center_z - z;
        dx * dx + dz * dz
    }

    fn allows_faction(&self, faction: u8) -> bool {
        match self.faction {
            0 => true,
            1 => faction == 0,
            2 => faction == 1,
            _ => false,
        }
    }
}

impl MapNpc {
    fn has_patrol(&self) -> bool {
        self.patrol_points.len() > 1
    }

    fn advance_patrol(&mut self) -> bool {
        if !self.has_patrol() {
            return false;
        }

        self.patrol_index = (self.patrol_index + 1) % self.patrol_points.len();
        let point = self.patrol_points[self.patrol_index];
        if (self.pos_x - point.x).abs() < 0.1
            && (self.pos_y - point.y).abs() < 0.1
            && (self.pos_z - point.z).abs() < 0.1
        {
            return false;
        }

        self.pos_x = point.x;
        self.pos_y = point.y;
        self.pos_z = point.z;
        self.angle = point.angle;
        true
    }
}

pub(crate) fn spawn_npc(
    map_id: u16,
    npc_type: u8,
    type_id: u16,
    pos_x: f32,
    pos_y: f32,
    pos_z: f32,
    angle: u16,
) -> MapNpc {
    let npc = MapNpc {
        id: next_npc_id(),
        npc_type,
        type_id,
        pos_x,
        pos_y,
        pos_z,
        angle,
        patrol_points: Vec::new(),
        patrol_index: 0,
    };
    lock_or_recover(&map_handle(map_id)).npcs.push(npc.clone());
    npc
}

pub(crate) fn build_static_npc_with_patrol(
    npc_type: u8,
    type_id: u16,
    patrol_points: Vec<NpcPatrolPoint>,
) -> Option<MapNpc> {
    let first = patrol_points.first().copied()?;
    Some(MapNpc {
        id: next_npc_id(),
        npc_type,
        type_id,
        pos_x: first.x,
        pos_y: first.y,
        pos_z: first.z,
        angle: first.angle,
        patrol_points,
        patrol_index: 0,
    })
}

pub(crate) fn spawn_mob(map_id: u16, mob_id: u16, pos_x: f32, pos_z: f32) -> MapMob {
    let mob = MapMob {
        id: next_mob_id(),
        mob_id,
        pos_x,
        pos_z,
        home_x: pos_x,
        home_z: pos_z,
        current_hp: 100,
        max_hp: 100,
        money_min: 0,
        money_max: 0,
        exp: 0,
        area: Some(
            MapMobArea {
                lower_x: pos_x - 8.0,
                lower_z: pos_z - 8.0,
                upper_x: pos_x + 8.0,
                upper_z: pos_z + 8.0,
            }
            .normalized(),
        ),
        respawn_delay: None,
        respawn_at: None,
        roam_step: 0,
    };
    lock_or_recover(&map_handle(map_id)).mobs.push(mob.clone());
    mob
}

fn hash_u32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846ca68b);
    x ^= x >> 16;
    x
}

fn unit_from_seed(seed: u32) -> f32 {
    (hash_u32(seed) as f64 / u32::MAX as f64) as f32
}

/// Spawns `count` mobs of `mob_id` scattered randomly within `radius` of
/// `(center_x, center_z)` (uniform over the disk area, not just the angle,
/// so they don't cluster near the center). Each candidate point gets a few
/// walkability retries (same bounded-retry pattern as MapMob::advance_roam)
/// before falling back to using it anyway.
pub(crate) fn spawn_mobs_scattered(
    map_id: u16,
    mob_id: u16,
    count: u8,
    center_x: f32,
    center_z: f32,
    radius: f32,
) -> Vec<MapMob> {
    let mut mobs = Vec::with_capacity(count as usize);
    for i in 0..count {
        let base_seed = next_mob_id().wrapping_add((i as u32).wrapping_mul(747_796_405));
        let mut chosen = (center_x, center_z);
        for attempt in 0..8u32 {
            let seed = base_seed.wrapping_add(attempt.wrapping_mul(2_246_822_519));
            let angle = unit_from_seed(seed) * std::f32::consts::TAU;
            let dist = radius * unit_from_seed(seed ^ 0x9e37_79b9).sqrt();
            let x = center_x + angle.cos() * dist;
            let z = center_z + angle.sin() * dist;
            chosen = (x, z);
            if is_map_walkable(map_id, x, z) {
                break;
            }
        }
        mobs.push(spawn_mob(map_id, mob_id, chosen.0, chosen.1));
    }
    mobs
}

/// Removes every mob within `radius` of `(center_x, center_z)` on `map_id`,
/// regardless of whether it came from the map's .svmap or a GM's `mmake`.
/// Returns the removed global ids, for despawn broadcasts.
pub(crate) fn remove_mobs_in_radius(
    map_id: u16,
    center_x: f32,
    center_z: f32,
    radius: f32,
) -> Vec<u32> {
    let handle = map_handle(map_id);
    let mut entities = lock_or_recover(&handle);
    let radius_sq = radius * radius;
    let mut removed = Vec::new();
    entities.mobs.retain(|mob| {
        let dx = mob.pos_x - center_x;
        let dz = mob.pos_z - center_z;
        if dx * dx + dz * dz <= radius_sq {
            removed.push(mob.id);
            false
        } else {
            true
        }
    });
    removed
}

/// Removes every NPC within `radius` of `(center_x, center_z)` on `map_id`.
/// Returns the removed global ids, for despawn broadcasts.
pub(crate) fn remove_npcs_in_radius(
    map_id: u16,
    center_x: f32,
    center_z: f32,
    radius: f32,
) -> Vec<u32> {
    let handle = map_handle(map_id);
    let mut entities = lock_or_recover(&handle);
    let radius_sq = radius * radius;
    let mut removed = Vec::new();
    entities.npcs.retain(|npc| {
        let dx = npc.pos_x - center_x;
        let dz = npc.pos_z - center_z;
        if dx * dx + dz * dz <= radius_sq {
            removed.push(npc.id);
            false
        } else {
            true
        }
    });
    removed
}

/// Removes every mob on `map_id`, from any source (.svmap or GM-spawned).
/// Returns the removed global ids, for despawn broadcasts. Used by
/// `mobreload` to fully clear a map before respawning from .svmap.
pub(crate) fn clear_mobs_on_map(map_id: u16) -> Vec<u32> {
    let handle = map_handle(map_id);
    lock_or_recover(&handle)
        .mobs
        .drain(..)
        .map(|mob| mob.id)
        .collect()
}

/// Removes every NPC on `map_id`, from any source. Returns the removed
/// global ids. Used by `npcreload`.
pub(crate) fn clear_npcs_on_map(map_id: u16) -> Vec<u32> {
    let handle = map_handle(map_id);
    lock_or_recover(&handle)
        .npcs
        .drain(..)
        .map(|npc| npc.id)
        .collect()
}

pub(crate) fn add_mobs(map_id: u16, mobs: Vec<MapMob>) {
    lock_or_recover(&map_handle(map_id)).mobs.extend(mobs);
}

pub(crate) fn add_npcs(map_id: u16, npcs: Vec<MapNpc>) {
    lock_or_recover(&map_handle(map_id)).npcs.extend(npcs);
}

pub(crate) fn build_static_mob(
    mob_id: u16,
    pos_x: f32,
    pos_z: f32,
    max_hp: u32,
    money_min: u32,
    money_max: u32,
    exp: u32,
    area: Option<MapMobArea>,
    respawn_seconds: Option<u64>,
) -> MapMob {
    MapMob {
        id: next_mob_id(),
        mob_id,
        pos_x,
        pos_z,
        home_x: pos_x,
        home_z: pos_z,
        current_hp: max_hp.max(1),
        max_hp: max_hp.max(1),
        money_min,
        money_max,
        exp,
        area: area.map(MapMobArea::normalized),
        respawn_delay: respawn_seconds.map(Duration::from_secs),
        respawn_at: None,
        roam_step: 0,
    }
}

pub(crate) fn build_npc_enter(npc: &MapNpc) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MapNpcEnter);
    packet.write_u32(npc.id);
    packet.write_u8(npc.npc_type);
    packet.write_u16(npc.type_id);
    packet.write_f32(npc.pos_x);
    packet.write_f32(npc.pos_y);
    packet.write_f32(npc.pos_z);
    packet.write_u16(npc.angle);
    packet.into_bytes()
}

pub(crate) fn build_npc_leave(npc_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MapNpcLeave);
    packet.write_u32(npc_id);
    packet.into_bytes()
}

pub(crate) fn build_npc_move(npc: &MapNpc, motion: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MapNpcMove);
    packet.write_u32(npc.id);
    packet.write_u8(motion);
    packet.write_f32(npc.pos_x);
    packet.write_f32(npc.pos_y);
    packet.write_f32(npc.pos_z);
    packet.into_bytes()
}

pub(crate) fn build_mob_enter(mob: &MapMob, is_new: bool) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MobEnter);
    packet.write_u32(mob.id);
    packet.write_u8(u8::from(is_new));
    packet.write_u16(mob.mob_id);
    packet.write_f32(mob.pos_x);
    packet.write_f32(mob.pos_z);
    packet.into_bytes()
}

pub(crate) fn build_mob_move(mob: &MapMob, motion: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MobMove);
    packet.write_u32(mob.id);
    packet.write_u8(motion);
    packet.write_f32(mob.pos_x);
    packet.write_f32(mob.pos_z);
    packet.into_bytes()
}

pub(crate) fn build_mob_leave(mob_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MobLeave);
    packet.write_u32(mob_id);
    packet.into_bytes()
}

pub(crate) fn build_mob_target_hp(mob: &MapMob) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::TargetMobHpUpdate);
    packet.write_u32(mob.id);
    packet.write_i32(mob.current_hp.min(i32::MAX as u32) as i32);
    packet.write_u8(5); // AttackSpeed.Normal
    packet.write_u8(2); // MoveSpeed.Normal
    packet.into_bytes()
}

pub(crate) fn build_mob_state(mob: &MapMob) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::TargetMobGetState);
    packet.write_u32(mob.id);
    packet.write_i32(mob.current_hp.min(i32::MAX as u32) as i32);
    packet.write_u8(5); // AttackSpeed.Normal
    packet.write_u8(2); // MoveSpeed.Normal
    packet.into_bytes()
}

pub(crate) fn build_character_mob_attack(
    character_id: u32,
    mob_id: u32,
    result: u8,
    damage_hp: u16,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterMobAutoAttack);
    packet.write_u8(result);
    packet.write_u32(character_id);
    packet.write_u32(mob_id);
    packet.write_u16(damage_hp);
    packet.write_u16(0);
    packet.write_u16(0);
    packet.into_bytes()
}

pub(crate) fn build_mob_death(mob_id: u32, killer_character_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MobDeath);
    packet.write_u32(mob_id);
    packet.write_u8(1);
    packet.write_u32(killer_character_id);
    packet.into_bytes()
}

impl MapMob {
    fn pick_respawn_position(&mut self, map_id: u16) -> (f32, f32) {
        let Some(area) = self.area.map(MapMobArea::normalized) else {
            return (self.home_x, self.home_z);
        };

        self.roam_step = self.roam_step.wrapping_add(1);
        for attempt in 0..64 {
            let seed = self
                .id
                .wrapping_add(self.roam_step.wrapping_mul(97))
                .wrapping_add(attempt);
            let x_ratio = (seed % 997) as f32 / 996.0;
            let z_ratio = ((seed / 997) % 991) as f32 / 990.0;
            let x = area.lower_x + (area.upper_x - area.lower_x) * x_ratio;
            let z = area.lower_z + (area.upper_z - area.lower_z) * z_ratio;
            if is_map_walkable_for_roam(map_id, x, z, Some(area)) {
                return (x, z);
            }
        }

        let x = area.clamp_x(self.home_x);
        let z = area.clamp_z(self.home_z);
        if is_map_walkable_for_roam(map_id, x, z, Some(area)) {
            (x, z)
        } else {
            (self.home_x, self.home_z)
        }
    }

    fn advance_roam(&mut self, map_id: u16) -> bool {
        let Some(area) = self.area else {
            return false;
        };
        let area = area.normalized();

        self.roam_step = self.roam_step.wrapping_add(1);
        for attempt in 0..16 {
            let seed = self
                .roam_step
                .wrapping_mul(17)
                .wrapping_add(attempt)
                .wrapping_add(self.id);
            let angle_index = (seed % 16) as f32;
            let radius = 2.0 + ((seed / 16) % 5) as f32;
            let angle = angle_index * std::f32::consts::TAU / 16.0;
            let next_x = area.clamp_x(self.home_x + angle.cos() * radius);
            let next_z = area.clamp_z(self.home_z + angle.sin() * radius);

            if is_map_walkable_for_roam(map_id, next_x, next_z, Some(area)) {
                if (self.pos_x - next_x).abs() < 0.1 && (self.pos_z - next_z).abs() < 0.1 {
                    continue;
                }
                self.pos_x = next_x;
                self.pos_z = next_z;
                return true;
            }
        }

        false
    }
}

fn is_map_walkable_for_roam(map_id: u16, x: f32, z: f32, area: Option<MapMobArea>) -> bool {
    let Some(area) = area else {
        return true;
    };
    let area = area.normalized();
    area.contains(x, z) && is_map_walkable(map_id, x, z)
}
