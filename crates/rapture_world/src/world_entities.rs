use anyhow::Result;
use rapture_common::db::DbMobDefinition;
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
    pub(crate) level: u16,
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
    /// `Mobs.Attack1` -- basic melee attack damage before the target's
    /// defense is subtracted (see combat::apply_incoming_mob_damage).
    pub(crate) attack: u32,
    /// `Mobs.AI` -- 0 passive (retaliate only), >= 1 aggressive (see
    /// `chase_range`). See DbMobDefinition's docs.
    pub(crate) ai: u8,
    /// `Mobs.ChaseRange` (meters) -- aggro detection radius for an
    /// aggressive mob, also the leash distance past which any mob (however
    /// aggroed) gives up and returns to roaming.
    pub(crate) chase_range: f32,
    /// The character this mob is currently fighting, if any. Set either by
    /// retaliation (`set_mob_aggro`, when a player damages the mob) or by
    /// proximity (an aggressive mob picking up a nearby player in
    /// `tick_mob_combat_for_viewers`). Cleared when the target dies,
    /// disconnects, or leashes out of range.
    pub(crate) aggro_target: Option<u32>,
    /// When this mob last landed an attack, for both the
    /// `MOB_ATTACK_INTERVAL` cooldown and the brief post-swing pause in
    /// pursuit (`MOB_ATTACK_PAUSE`). `None` until it attacks for the first
    /// time.
    last_attack_at: Option<Instant>,
    /// True while the mob is walking back to its home coordinate after a
    /// chase ended (de-aggro, target death, or leash). Driven by the combat
    /// sweep at walk speed; the roam sweep leaves it alone until it arrives.
    /// Prevents the old "snap instantly back home" behavior.
    returning_home: bool,
    /// Active periodic-damage (DoT) effects on this mob (`Skills.
    /// TypeDetail == 61`) -- lives on the mob itself, not on any specific
    /// player's connection, since a mob is shared/global state that
    /// multiple players can damage and any of them (or none, if they've
    /// since disconnected) can be the one who applied a given DoT. Ticked
    /// by `tick_all_mob_dots`, a background sweep (see sweeps.rs),
    /// independent of any per-connection loop.
    pub(crate) dots: Vec<ActiveDot>,
}

/// One active periodic-damage effect on a `MapMob`. See `MapMob::dots`'s
/// doc for why this lives on the mob rather than a player's connection.
#[derive(Clone, Debug)]
pub(crate) struct ActiveDot {
    /// Whoever cast this DoT -- attributed the kill/exp/drops if a tick
    /// finishes the mob off, whether they're still online or not (see
    /// `tick_all_mob_dots`).
    pub(crate) caster_character_id: u32,
    pub(crate) skill_id: u16,
    pub(crate) skill_level: u8,
    ends_at: Instant,
    next_tick_at: Instant,
    damage_hp: u32,
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

pub(crate) fn register_map_grid(map_id: u16, map_size: u32, map_mask: Vec<u8>) {
    lock_or_recover(grids()).insert(map_id, MapGrid { map_size, map_mask });
}

/// Entities within the 5x5 visibility window of `(x, z)`. Unlike a naive
/// "clone the whole map then filter" pass, this clones only the entities
/// that actually pass the neighbor-cell check, so the cost scales with
/// what's visible rather than with everything on the map.
pub(crate) fn entities_near(map_id: u16, x: f32, z: f32) -> MapEntities {
    if !lock_or_recover(grids()).contains_key(&map_id) {
        return entities_on_map(map_id);
    }

    let viewer_cell = cell_for(x, z, VISIBILITY_CELL_SIZE);
    let handle = map_handle(map_id);
    let entities = lock_or_recover(&handle);
    MapEntities {
        npcs: entities
            .npcs
            .iter()
            .filter(|npc| {
                is_neighbor_cell(viewer_cell, cell_for(npc.pos_x, npc.pos_z, VISIBILITY_CELL_SIZE))
            })
            .cloned()
            .collect(),
        mobs: entities
            .mobs
            .iter()
            .filter(|mob| mob.current_hp > 0)
            .filter(|mob| {
                is_neighbor_cell(viewer_cell, cell_for(mob.pos_x, mob.pos_z, VISIBILITY_CELL_SIZE))
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
    // A mob resetting home after a chase is immune to all damage and can't
    // be re-aggroed (callers set aggro only after a successful damage_mob) --
    // the reset is uninterruptible until it reaches home and restores HP.
    if mob.current_hp == 0 || mob.returning_home {
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

/// A tick interval confirmed by the user for periodic damage, same as
/// periodic heal (see skillcast.rs's PERIODIC_HEAL_TICK).
const DOT_TICK: Duration = Duration::from_secs(3);

/// Starts (or refreshes, if this same caster+skill already has a DoT on
/// this mob) a periodic-damage effect. Called when a damage skill with
/// nonzero `TimeDamageHP`/etc hits a mob (see skillcast.rs) -- independent
/// of that skill's own instant `DamageHP`, if any. Only `TimeDamageHP` is
/// tracked: mobs have no SP/MP concept in this project's model, so
/// `TimeDamageSP/MP` are intentionally not passed through here.
pub(crate) fn apply_dot_to_mob(
    map_id: u16,
    mob_global_id: u32,
    caster_character_id: u32,
    skill_id: u16,
    skill_level: u8,
    damage_hp: u32,
    keep_time_seconds: u64,
) {
    let handle = map_handle(map_id);
    let mut entities = lock_or_recover(&handle);
    let Some(mob) = entities.mobs.iter_mut().find(|mob| mob.id == mob_global_id) else {
        return;
    };
    // Immune while resetting home -- refuse to attach new DoTs too.
    if mob.current_hp == 0 || mob.returning_home {
        return;
    }

    let now = Instant::now();
    mob.dots
        .retain(|dot| !(dot.caster_character_id == caster_character_id && dot.skill_id == skill_id));
    mob.dots.push(ActiveDot {
        caster_character_id,
        skill_id,
        skill_level,
        ends_at: now + Duration::from_secs(keep_time_seconds),
        next_tick_at: now + DOT_TICK,
        damage_hp,
    });
}

/// Outcome of one DoT tick, returned to the caller (a background sweep,
/// see sweeps.rs) to handle broadcasts/kill-consequences/exp OUTSIDE the
/// per-map lock this function briefly holds while applying damage.
#[derive(Clone, Debug)]
pub(crate) struct DotTickOutcome {
    pub(crate) map_id: u16,
    pub(crate) mob: MapMob,
    pub(crate) caster_character_id: u32,
    pub(crate) skill_id: u16,
    pub(crate) skill_level: u8,
    pub(crate) applied_hp: u32,
    pub(crate) killed: bool,
    pub(crate) respawn_seconds: Option<u64>,
    /// Whether this was the bonus final tick fired by the DoT expiring,
    /// as opposed to a regular scheduled tick -- same "also tick once more
    /// exactly at expiry" behavior confirmed by the user for periodic
    /// heal, mirrored here for symmetry.
    pub(crate) is_final_tick: bool,
}

/// Applies every due DoT tick (and one bonus final tick per DoT exactly
/// when it expires) across every mob on every map, removing any DoT whose
/// duration has ended. Only `damage_hp` is actually applied -- mobs have
/// no SP/MP concept in this project's model, so `TimeDamageSP/MP` are
/// stored on `ActiveDot` (for a possible future PvP use) but inert here.
/// Called once per sweep iteration (see `sweeps::run_mob_dot_sweep`),
/// independent of any per-connection loop -- a mob is shared/global state,
/// not owned by whichever connection cast the DoT on it.
pub(crate) fn tick_all_mob_dots() -> Vec<DotTickOutcome> {
    let map_ids: Vec<u16> = lock_or_recover(registry()).keys().copied().collect();
    let now = Instant::now();
    let mut outcomes = Vec::new();

    for map_id in map_ids {
        let handle = map_handle(map_id);
        let mut entities = lock_or_recover(&handle);
        for mob in entities.mobs.iter_mut() {
            // A mob walking home after a chase is "resetting" and immune to
            // all damage (see damage_mob) -- its DoTs go inert too, and are
            // cleared entirely when it reaches home and restores HP.
            if mob.current_hp == 0 || mob.dots.is_empty() || mob.returning_home {
                continue;
            }

            let mut index = 0;
            while index < mob.dots.len() {
                let is_expiring = mob.dots[index].ends_at <= now;
                let due_tick = mob.dots[index].next_tick_at <= now;

                if due_tick || is_expiring {
                    let dot = &mob.dots[index];
                    let caster_character_id = dot.caster_character_id;
                    let skill_id = dot.skill_id;
                    let skill_level = dot.skill_level;
                    let damage_hp = dot.damage_hp;

                    let applied_hp = damage_hp.min(mob.current_hp);
                    mob.current_hp = mob.current_hp.saturating_sub(applied_hp);
                    let killed = mob.current_hp == 0;
                    let respawn_seconds = if killed {
                        if let Some(delay) = mob.respawn_delay {
                            mob.respawn_at = Some(now + delay);
                            Some(delay.as_secs())
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    outcomes.push(DotTickOutcome {
                        map_id,
                        mob: mob.clone(),
                        caster_character_id,
                        skill_id,
                        skill_level,
                        applied_hp,
                        killed,
                        respawn_seconds,
                        is_final_tick: is_expiring,
                    });

                    if !is_expiring {
                        mob.dots[index].next_tick_at += DOT_TICK;
                    }

                    // A kill stops every OTHER DoT on this mob from
                    // ticking further too (nothing left to damage) --
                    // matches the natural expectation that a dead mob's
                    // remaining DoTs simply have nothing more to do.
                    if killed {
                        mob.dots.clear();
                        break;
                    }
                }

                if is_expiring {
                    if index < mob.dots.len() {
                        mob.dots.remove(index);
                    }
                } else {
                    index += 1;
                }
            }
        }
    }

    outcomes
}

/// Shared "can see this" radius for mob/NPC AI visibility gating and for
/// filtering every other whole-map broadcast (character move/shape/
/// equipment, combat, item drop/pickup/despawn) down to nearby players only
/// -- matching Aries's real CZone::SendView spatial-cell view range instead
/// of notifying every player on the map regardless of distance.
/// Fixed grid cell size for the visibility system, matching the real
/// server's `cell_x = floor(pos.x / 40)` partitioning (per a GitHub issue
/// report on the public repo) -- used purely as a broad-phase search
/// bucket (which players/entities are even worth checking), not as the
/// visibility boundary itself; the actual boundary is one of the radii
/// below, checked within the resulting 5x5 cell window (see
/// `is_neighbor_cell`).
pub(crate) const VISIBILITY_CELL_SIZE: u32 = 40;

/// General appearance/movement visibility radius (character move/shape/
/// equipment, mob/NPC spawns, item drop/pickup/despawn) -- confirmed
/// against the real server's `viewport_visibility_radius`.
pub(crate) const VIEWPORT_RADIUS: f32 = 120.0;

/// Combat-specific broadcast radius (auto-attack results, mob death,
/// drops) -- confirmed against the real server's `combat_visibility_radius`.
/// Smaller than `VIEWPORT_RADIUS` since combat feedback only needs to reach
/// players close enough to plausibly be involved, not everyone who can
/// merely see the mob.
pub(crate) const COMBAT_RADIUS: f32 = 80.0;

/// Melee distance within which a mob can land a basic attack on its target.
/// Slightly larger than a player's own auto-attack grace so a chasing mob
/// that just caught up can swing without a one-tick gap.
pub(crate) const MOB_ATTACK_RANGE: f32 = 3.0;

/// How often a mob may land a basic attack. Server policy for now (a flat
/// cadence for every mob) -- there's no confirmed per-mob attack-speed
/// column yet; the target-HP packet's `AttackSpeed.Normal` is a display
/// hint, not a real interval. Revisit if/when a real speed column is found.
pub(crate) const MOB_ATTACK_INTERVAL: Duration = Duration::from_millis(2500);

/// One chase step per combat tick (meters). With the combat sweep's
/// `MOB_COMBAT_TICK` cadence this sets the mob's chase speed -- 3.5m every
/// 500ms is ~7 m/s, close to a running player so an aggroed mob keeps pace
/// and can actually close in to melee (a slower step let players trivially
/// outrun mobs, which read as the mob "barely attacking"). Tunable.
const MOB_CHASE_STEP: f32 = 3.5;

/// Motion byte for a chasing mob's 0x0603 move -- 1 = running. Confirmed
/// via capture (roam moves send 0 = walking). The client uses this to pick
/// the mob's animation AND its move-interpolation speed.
const MOB_MOTION_RUN: u8 = 1;

/// Motion byte for a walking mob's 0x0603 move -- 0 = walking. Used for the
/// unhurried walk back home after a chase ends.
const MOB_MOTION_WALK: u8 = 0;

/// How long a mob holds position after landing a hit before resuming
/// pursuit -- a token recovery beat only. Kept well under the combat tick
/// (`MOB_COMBAT_TICK`, 500ms) so it almost never actually skips a chase
/// step: the attack tick itself already doesn't move, which is enough of a
/// visible "pause to swing". A larger value made the mob skip ~2 chase ticks
/// per hit, fall behind a running player, and end up attacking only every
/// few seconds with a stuttery pursuit.
const MOB_ATTACK_PAUSE: Duration = Duration::from_millis(200);

/// One step per combat tick while walking back home after a chase (meters) --
/// smaller than `MOB_CHASE_STEP` so the return is an unhurried walk, matching
/// the walk motion it's sent with.
const MOB_RETURN_STEP: f32 = 2.0;

/// Distance from home at which a returning mob is considered arrived and
/// snaps exactly onto its home coordinate.
const MOB_RETURN_ARRIVE: f32 = 1.0;

/// Minimum leash radius (from a mob's home) for a mob that was aggroed by
/// retaliation rather than proximity. A passive mob (`ai == 0`) can have a
/// zero `chase_range`, so leashing purely on `chase_range` would make it
/// give up the instant it's hit; this floor lets it pursue a bit first.
const MOB_RETALIATION_LEASH: f32 = 20.0;

/// Extra slack added to a mob's leash radius before it gives up and returns
/// to roaming, so a target hovering right at the edge doesn't cause rapid
/// aggro/de-aggro flapping.
const MOB_LEASH_GRACE: f32 = 8.0;

/// One player's live combat-relevant snapshot, gathered once per combat
/// sweep tick (see sweeps::run_mob_combat_sweep) and handed to
/// `tick_mob_combat_for_viewers` so mob AI can read every target's position
/// and alive state without touching any connection's own locals.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlayerCombatSnapshot {
    pub(crate) character_id: u32,
    pub(crate) map_id: u16,
    pub(crate) pos_x: f32,
    pub(crate) pos_z: f32,
    pub(crate) alive: bool,
}

/// One mob->player basic attack the combat tick decided to land. The actual
/// damage (after the target's defense) and all consequences are resolved by
/// the victim's own connection thread via combat::apply_incoming_mob_damage,
/// so this only carries who hit whom for how much raw attack.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MobAttackAction {
    pub(crate) mob_global_id: u32,
    pub(crate) mob_id: u16,
    pub(crate) attack: u32,
    pub(crate) target_character_id: u32,
}

/// Everything a single combat tick produced: attacks to route to victims'
/// mailboxes, movement packets (chase or walk-home) to broadcast to nearby
/// players, and HP-restore broadcasts for mobs that just reset at home.
#[derive(Default)]
pub(crate) struct MobCombatTick {
    pub(crate) attacks: Vec<MobAttackAction>,
    pub(crate) moves: Vec<(u16, f32, f32, Vec<u8>)>,
    pub(crate) restores: Vec<(u16, f32, f32, Vec<u8>)>,
}

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
fn bucket_viewers(viewers: &[(f32, f32)]) -> HashMap<(i32, i32), Vec<(f32, f32)>> {
    let mut buckets: HashMap<(i32, i32), Vec<(f32, f32)>> = HashMap::new();
    for &(x, z) in viewers {
        buckets
            .entry(cell_for(x, z, VISIBILITY_CELL_SIZE))
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
        let buckets = bucket_viewers(viewers);
        let mut moved_on_map = 0usize;
        let mut visited_on_map = 0usize;

        for mob in &mut entities.mobs {
            if moved_on_map >= max_moves_per_map {
                break;
            }
            let mob_cell = cell_for(mob.pos_x, mob.pos_z, VISIBILITY_CELL_SIZE);
            // A mob in combat (or walking back home after one) is driven
            // entirely by the combat sweep (`tick_mob_combat_for_viewers`,
            // which chases/attacks its target and walks it home afterward).
            // Roaming it in parallel would fight that -- yanking it back
            // toward its home area every tick -- which is the confirmed cause
            // of aggroed mobs "wandering strangely" instead of pursuing, and
            // would also teleport a returning mob. Skip it here; once it
            // finishes returning home, roam resumes.
            if mob.current_hp == 0
                || mob.area.is_none()
                || mob.aggro_target.is_some()
                || mob.returning_home
                || !is_near_any_bucketed((mob.pos_x, mob.pos_z), mob_cell, &buckets, VIEWPORT_RADIUS)
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
        let buckets = bucket_viewers(viewers);
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
            // Clear any combat state carried from its previous life so it
            // spawns fresh (idle, not mid-chase/return).
            mob.aggro_target = None;
            mob.last_attack_at = None;
            mob.returning_home = false;
            respawned_on_map = respawned_on_map.saturating_add(1);

            let mob_cell = cell_for(mob.pos_x, mob.pos_z, VISIBILITY_CELL_SIZE);
            if is_near_any_bucketed((mob.pos_x, mob.pos_z), mob_cell, &buckets, VIEWPORT_RADIUS) {
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
        let buckets = bucket_viewers(viewers);
        let mut moved_on_map = 0usize;
        let mut visited_on_map = 0usize;

        for npc in &mut entities.npcs {
            if moved_on_map >= max_moves_per_map {
                break;
            }
            let npc_cell = cell_for(npc.pos_x, npc.pos_z, VISIBILITY_CELL_SIZE);
            if !npc.has_patrol()
                || !is_near_any_bucketed((npc.pos_x, npc.pos_z), npc_cell, &buckets, VIEWPORT_RADIUS)
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

/// Marks `mob_global_id` on `map_id` as fighting `character_id` -- called
/// when that character damages the mob (auto-attack or skill), so even a
/// passive mob (`ai == 0`) fights back. Overwrites any existing target with
/// the most recent attacker (last-hit aggro), matching the simplest retail
/// behavior; no threat table yet.
pub(crate) fn set_mob_aggro(map_id: u16, mob_global_id: u32, character_id: u32) {
    let handle = map_handle(map_id);
    let mut entities = lock_or_recover(&handle);
    if let Some(mob) = entities
        .mobs
        .iter_mut()
        .find(|mob| mob.id == mob_global_id && mob.current_hp > 0)
    {
        mob.aggro_target = Some(character_id);
    }
}


/// Clears `mob_global_id`'s aggro (and its attack cooldown) -- called when
/// its target dies, so it stops swinging and walks back home (rather than
/// staying put where the kill happened).
pub(crate) fn clear_mob_aggro(map_id: u16, mob_global_id: u32) {
    let handle = map_handle(map_id);
    let mut entities = lock_or_recover(&handle);
    if let Some(mob) = entities.mobs.iter_mut().find(|mob| mob.id == mob_global_id) {
        mob.aggro_target = None;
        mob.last_attack_at = None;
        mob.returning_home = true;
    }
}

/// Advances every mob that is (or becomes) in combat by one tick: acquires
/// proximity aggro for aggressive mobs, drops leashed/dead/absent targets,
/// and either lands a basic attack (if in `MOB_ATTACK_RANGE` and off
/// cooldown) or takes one chase step toward the target. Mirrors the
/// per-map, lock-per-map structure of `tick_mob_roam_for_viewers`; the
/// caller (sweeps::run_mob_combat_sweep) routes the returned attacks to
/// victims and broadcasts the moves.
pub(crate) fn tick_mob_combat_for_viewers(
    players: &[PlayerCombatSnapshot],
    max_actions_per_map: usize,
) -> Result<MobCombatTick> {
    let mut tick = MobCombatTick::default();
    let now = Instant::now();

    let mut players_by_map: HashMap<u16, Vec<PlayerCombatSnapshot>> = HashMap::new();
    for player in players {
        players_by_map.entry(player.map_id).or_default().push(*player);
    }

    for (map_id, map_players) in &players_by_map {
        let handle = map_handle(*map_id);
        let mut entities = lock_or_recover(&handle);
        let mut actions_on_map = 0usize;

        for mob in &mut entities.mobs {
            if actions_on_map >= max_actions_per_map {
                break;
            }
            if mob.current_hp == 0 {
                mob.aggro_target = None;
                mob.returning_home = false;
                continue;
            }

            // Validate/refresh the current target: it must still be present
            // on this map, alive, and within leash range of the mob's home.
            // Losing the target starts the walk back home (never a snap).
            if let Some(target_id) = mob.aggro_target {
                let still_valid = map_players
                    .iter()
                    .find(|player| player.character_id == target_id && player.alive)
                    .map(|player| {
                        let leash = mob.chase_range.max(MOB_RETALIATION_LEASH) + MOB_LEASH_GRACE;
                        distance_xz(mob.home_x, mob.home_z, player.pos_x, player.pos_z) <= leash
                    })
                    .unwrap_or(false);
                if !still_valid {
                    mob.aggro_target = None;
                    mob.last_attack_at = None;
                    mob.returning_home = true;
                }
            }

            // Aggressive mobs pick up the nearest alive player inside their
            // detection radius when idle. NOT while returning home: that
            // reset is uninterruptible (the mob is immune and won't re-aggro
            // until it's home again), matching the immunity in damage_mob.
            if mob.aggro_target.is_none() && !mob.returning_home && mob.ai >= 1 && mob.chase_range > 0.0 {
                let mut best: Option<(u32, f32)> = None;
                for player in map_players.iter().filter(|player| player.alive) {
                    let dist = distance_xz(mob.pos_x, mob.pos_z, player.pos_x, player.pos_z);
                    if dist <= mob.chase_range && best.is_none_or(|(_, d)| dist < d) {
                        best = Some((player.character_id, dist));
                    }
                }
                if let Some((target_id, _)) = best {
                    mob.aggro_target = Some(target_id);
                }
            }

            let Some(target_id) = mob.aggro_target else {
                // No target: if a chase just ended, walk (not teleport) back
                // toward home one step at a time, at walk speed.
                if mob.returning_home {
                    let dist_home = distance_xz(mob.pos_x, mob.pos_z, mob.home_x, mob.home_z);
                    let arrived = if dist_home <= MOB_RETURN_ARRIVE {
                        mob.pos_x = mob.home_x;
                        mob.pos_z = mob.home_z;
                        true
                    } else {
                        let step = MOB_RETURN_STEP.min(dist_home);
                        let next_x = mob.pos_x + (mob.home_x - mob.pos_x) / dist_home * step;
                        let next_z = mob.pos_z + (mob.home_z - mob.pos_z) / dist_home * step;
                        if is_map_walkable(*map_id, next_x, next_z) {
                            mob.pos_x = next_x;
                            mob.pos_z = next_z;
                            false
                        } else {
                            // Blocked mid-return -- give up pathing and settle
                            // at home rather than getting stuck out in the world.
                            mob.pos_x = mob.home_x;
                            mob.pos_z = mob.home_z;
                            true
                        }
                    };
                    actions_on_map = actions_on_map.saturating_add(1);
                    tick.moves
                        .push((*map_id, mob.pos_x, mob.pos_z, build_mob_move(mob, MOB_MOTION_WALK)?));
                    if arrived {
                        // Reset complete: full HP, clear any lingering DoTs,
                        // and broadcast the restored HP so onlookers' bars
                        // reflect the reset. Then it's an ordinary idle mob
                        // again (roam resumes, damage/aggro re-enabled).
                        mob.returning_home = false;
                        mob.last_attack_at = None;
                        mob.current_hp = mob.max_hp.max(1);
                        mob.dots.clear();
                        tick.restores.push((
                            *map_id,
                            mob.pos_x,
                            mob.pos_z,
                            build_mob_current_hp(mob.id, mob.current_hp)?,
                        ));
                    }
                }
                continue;
            };
            // Chasing a live target, so it's not returning home.
            mob.returning_home = false;

            let Some(target) = map_players
                .iter()
                .find(|player| player.character_id == target_id)
            else {
                continue;
            };

            let dist = distance_xz(mob.pos_x, mob.pos_z, target.pos_x, target.pos_z);
            if dist <= MOB_ATTACK_RANGE {
                let ready = mob
                    .last_attack_at
                    .is_none_or(|at| now.duration_since(at) >= MOB_ATTACK_INTERVAL);
                if ready {
                    mob.last_attack_at = Some(now);
                    actions_on_map = actions_on_map.saturating_add(1);
                    tick.attacks.push(MobAttackAction {
                        mob_global_id: mob.id,
                        mob_id: mob.mob_id,
                        attack: mob.attack,
                        target_character_id: target_id,
                    });
                }
                continue;
            }

            // Just swung? Hold position a beat before resuming pursuit, so the
            // mob visibly pauses to attack instead of gliding while it swings.
            if mob
                .last_attack_at
                .is_some_and(|at| now.duration_since(at) < MOB_ATTACK_PAUSE)
            {
                continue;
            }

            // Out of melee range -- take one running step toward the target.
            // Not constrained to the mob's roam area (it's actively chasing),
            // only to map walkability.
            if dist > 0.001 {
                let step = MOB_CHASE_STEP.min(dist);
                let next_x = mob.pos_x + (target.pos_x - mob.pos_x) / dist * step;
                let next_z = mob.pos_z + (target.pos_z - mob.pos_z) / dist * step;
                if is_map_walkable(*map_id, next_x, next_z) {
                    mob.pos_x = next_x;
                    mob.pos_z = next_z;
                    actions_on_map = actions_on_map.saturating_add(1);
                    // Motion 1 = running, confirmed via a real capture of a
                    // mob chasing a player (0x0603 with motion byte 0x01,
                    // vs 0x00 for a walking/roam move). Besides the run
                    // animation, this drives the client to interpolate the
                    // mob toward the next position at run speed instead of
                    // walk speed.
                    tick.moves
                        .push((*map_id, mob.pos_x, mob.pos_z, build_mob_move(mob, MOB_MOTION_RUN)?));
                }
            }
        }
    }

    Ok(tick)
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

/// True 5x5 visibility window (Chebyshev distance <= 2 in both axes),
/// matching the real server's neighbor-cell check. Previously used Manhattan
/// distance (`dx + dz <= 1`), which was smaller than even a 3x3 window and
/// excluded diagonal neighbor cells entirely.
fn is_neighbor_cell(a: (i32, i32), b: (i32, i32)) -> bool {
    let dx = (a.0 - b.0).abs();
    let dz = (a.1 - b.1).abs();
    dx <= 2 && dz <= 2
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

/// Spawns a single GM-made (`mmake`) mob using its REAL definition
/// (level/max HP/money/exp, same as any mob placed by a .svmap spawn
/// area) -- the only difference from a normal mob is `respawn_delay`,
/// which is always forced to `None` here regardless of the mob's real
/// respawn timer, since this mob isn't part of any spawn area and must
/// never come back on its own after dying; it lives indefinitely until
/// killed (or `mera`'d) instead. `definition` is `None` for an unknown/
/// invalid mob_id, in which case this falls back to the same defaults
/// `get_mob_definitions`/`get_mob_definition` themselves use for a
/// missing row (level 1, 100 HP, no money/exp) rather than refusing to
/// spawn anything.
pub(crate) fn spawn_mob(
    map_id: u16,
    mob_id: u16,
    pos_x: f32,
    pos_z: f32,
    definition: Option<&DbMobDefinition>,
) -> MapMob {
    let area = MapMobArea {
        lower_x: pos_x - 8.0,
        lower_z: pos_z - 8.0,
        upper_x: pos_x + 8.0,
        upper_z: pos_z + 8.0,
    };
    let mob = build_static_mob(
        mob_id,
        definition.map_or(1, |definition| definition.level),
        pos_x,
        pos_z,
        definition.map_or(100, |definition| definition.max_hp),
        definition.map_or(0, |definition| definition.money_min),
        definition.map_or(0, |definition| definition.money_max),
        definition.map_or(0, |definition| definition.exp),
        definition.map_or(0, |definition| definition.attack),
        definition.map_or(0, |definition| definition.ai),
        definition.map_or(0.0, |definition| definition.chase_range),
        Some(area),
        None,
    );
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
/// before falling back to using it anyway. `definition` is looked up ONCE
/// by the caller (this module doesn't touch the DB itself) and reused for
/// every spawned mob rather than re-querying per mob.
pub(crate) fn spawn_mobs_scattered(
    map_id: u16,
    mob_id: u16,
    count: u8,
    center_x: f32,
    center_z: f32,
    radius: f32,
    definition: Option<&DbMobDefinition>,
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
        mobs.push(spawn_mob(map_id, mob_id, chosen.0, chosen.1, definition));
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_static_mob(
    mob_id: u16,
    level: u16,
    pos_x: f32,
    pos_z: f32,
    max_hp: u32,
    money_min: u32,
    money_max: u32,
    exp: u32,
    attack: u32,
    ai: u8,
    chase_range: f32,
    area: Option<MapMobArea>,
    respawn_seconds: Option<u64>,
) -> MapMob {
    MapMob {
        id: next_mob_id(),
        mob_id,
        level,
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
        attack,
        ai,
        chase_range,
        aggro_target: None,
        last_attack_at: None,
        returning_home: false,
        dots: Vec::new(),
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

/// Builds a 0x0605 mob->player basic-attack packet -- the mirror of
/// `build_character_mob_attack` (0x0503) with the mob as attacker. `result`
/// is 0 for a normal hit, 1 for the lethal blow (see
/// `Opcode::MobAttackCharacter`). Only `damage_hp` is ever nonzero (players
/// take HP damage from a basic attack; SP/MP slots exist on the wire but
/// stay zero).
pub(crate) fn build_mob_attack_character(
    result: u8,
    mob_id: u32,
    character_id: u32,
    damage_hp: u16,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MobAttackCharacter);
    packet.write_u8(result);
    packet.write_u32(mob_id);
    packet.write_u32(character_id);
    packet.write_u16(damage_hp);
    packet.write_u16(0);
    packet.write_u16(0);
    packet.into_bytes()
}

/// Builds a 0x0504 character-death packet (see `Opcode::CharacterDeath`).
/// `death_type` is 0x02 for a mob kill; `killer_id` is the killer's global
/// id (a mob id here).
pub(crate) fn build_character_death(
    character_id: u32,
    death_type: u8,
    killer_id: u32,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterDeath);
    packet.write_u32(character_id);
    packet.write_u8(death_type);
    packet.write_u32(killer_id);
    packet.into_bytes()
}

/// Builds the 0x051C packet the real server sends alongside 0x0504 on death.
/// The two trailing bytes (0x07, 0x02) are replicated verbatim from a real
/// capture -- their meaning isn't confirmed yet (see
/// `Opcode::CharacterDeathState`).
pub(crate) fn build_character_death_state(character_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterDeathState);
    packet.write_u32(character_id);
    packet.write_u8(0x07);
    packet.write_u8(0x02);
    packet.into_bytes()
}

/// Builds the 0x0406 packet that opens the resurrection sequence (see
/// `Opcode::CharacterReviveInit`). Payload is just the reviving character's
/// id.
pub(crate) fn build_character_revive_init(character_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterReviveInit);
    packet.write_u32(character_id);
    packet.into_bytes()
}

/// Builds the 0x0551 resurrect-reposition packet (see
/// `Opcode::CharacterResurrect`). The 5-byte blob `04 46 AF 37 01` is
/// replicated verbatim from a real capture -- its fields aren't decoded yet.
pub(crate) fn build_character_resurrect(character_id: u32, x: f32, y: f32, z: f32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterResurrect);
    packet.write_u32(character_id);
    packet.write_bytes(&[0x04, 0x46, 0xAF, 0x37, 0x01]);
    packet.write_f32(x);
    packet.write_f32(y);
    packet.write_f32(z);
    packet.into_bytes()
}

/// Builds the 0x0505 revive stat-restore packet (see
/// `Opcode::CharacterReviveHitpoints`): the reviving character's id plus the
/// HP/MP/SP to restore them to.
pub(crate) fn build_character_revive_hitpoints(
    character_id: u32,
    hp: u32,
    mp: u32,
    sp: u32,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterReviveHitpoints);
    packet.write_u32(character_id);
    packet.write_u32(hp);
    packet.write_u32(mp);
    packet.write_u32(sp);
    packet.into_bytes()
}

pub(crate) fn build_mob_death(mob_id: u32, killer_character_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MobDeath);
    packet.write_u32(mob_id);
    packet.write_u8(1);
    packet.write_u32(killer_character_id);
    packet.into_bytes()
}

/// The only `debuff_type` value observed in a real capture so far --
/// see `Opcode::MobDebuffAdd`'s doc.
pub(crate) const MOB_DEBUFF_TYPE_DOT: u8 = 0x02;

pub(crate) fn build_mob_debuff_add(debuff_type: u8, mob_id: u32, skill_id: u16, skill_level: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MobDebuffAdd);
    packet.write_u8(debuff_type);
    packet.write_u32(mob_id);
    packet.write_u16(skill_id);
    packet.write_u8(skill_level);
    packet.into_bytes()
}

pub(crate) fn build_mob_debuff_remove(debuff_type: u8, mob_id: u32, skill_id: u16, skill_level: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MobDebuffRemove);
    packet.write_u8(debuff_type);
    packet.write_u32(mob_id);
    packet.write_u16(skill_id);
    packet.write_u8(skill_level);
    packet.into_bytes()
}

pub(crate) fn build_mob_periodic_damage(
    mob_id: u32,
    skill_id: u16,
    skill_level: u8,
    damage_hp: u16,
    damage_sp: u16,
    damage_mp: u16,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MobPeriodicDamage);
    packet.write_u32(mob_id);
    packet.write_u16(skill_id);
    packet.write_u8(skill_level);
    packet.write_u16(damage_hp);
    packet.write_u16(damage_sp);
    packet.write_u16(damage_mp);
    packet.into_bytes()
}

pub(crate) fn build_mob_current_hp(mob_id: u32, current_hp: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MobCurrentHitpoints);
    packet.write_u32(mob_id);
    packet.write_u32(current_hp);
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
