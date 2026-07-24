//! Background threads that periodically sweep world state (map items, mob
//! roam/respawn, NPC patrol, DoT ticks) and broadcast whatever changed to
//! nearby players. Spawned once each from main()'s startup; self-contained
//! -- none of this touches a SPECIFIC connection's own locals directly
//! (the DoT sweep queues EXP grants through the same mailbox pattern used
//! elsewhere in this project instead, see ConnectedPlayer::queue_mob_kill).

use rapture_common::db;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::chat::*;
use crate::combat::*;
use crate::drops::*;
use crate::inventory::*;
use crate::map_items::*;
use crate::session::Logger;
use crate::world_entities::*;

/// How often the despawn sweep checks for expired map items. Aries doesn't
/// require this to be precise (no protocol-level timing constraint), so a
/// coarse interval is fine.
const MAP_ITEM_SWEEP_INTERVAL: Duration = Duration::from_secs(2);

/// Periodically removes map items older than MAP_ITEM_DESPAWN_AGE (60s
/// total: 30s owner-only per MAP_ITEM_OWNER_WINDOW, then 30s pickable by
/// anyone, then gone for good) and tells every player still on that map to
/// stop showing it.
pub(crate) fn run_map_item_despawn_sweep(logger: Arc<Logger>) {
    loop {
        thread::sleep(MAP_ITEM_SWEEP_INTERVAL);
        for (map_id, item) in sweep_expired() {
            logger.line(format!(
                "Rapture World despawned map_item_id={} on map={} (60s lifetime elapsed)",
                item.id, map_id
            ));
            let Ok(remove_packet) = build_map_remove_item(item.id) else {
                continue;
            };
            for player in players_on_map_near(map_id, item.pos_x, item.pos_z, VIEWPORT_RADIUS) {
                let _ = player.send_packet(&remove_packet, "0x0402 map item despawn", &logger);
            }
        }
    }
}

pub(crate) fn run_mob_roam_sweep(logger: Arc<Logger>) {
    loop {
        thread::sleep(Duration::from_secs(1));
        let players = all_players();
        if players.is_empty() {
            continue;
        }

        let viewers_by_map = viewers_by_map(&players);

        let moves = match tick_mob_roam_for_viewers(&viewers_by_map, 48) {
            Ok(moves) => moves,
            Err(error) => {
                logger.line(format!("Rapture World mob roam tick failed: {error:?}"));
                continue;
            }
        };

        // Group by map first so each map's recipient list is looked up once
        // per tick, not once per moved mob.
        let mut moves_by_map: HashMap<u16, Vec<(f32, f32, Vec<u8>)>> = HashMap::new();
        for (map_id, x, z, packet) in moves {
            moves_by_map.entry(map_id).or_default().push((x, z, packet));
        }
        for (map_id, moves) in moves_by_map {
            for (x, z, packet) in moves {
                for player in players_on_map_near(map_id, x, z, VIEWPORT_RADIUS) {
                    let _ = player.send_packet(&packet, "0x0603 mob roam move", &logger);
                }
            }
        }
    }
}

pub(crate) fn run_mob_respawn_sweep(logger: Arc<Logger>) {
    loop {
        thread::sleep(Duration::from_secs(1));
        let players = all_players();
        if players.is_empty() {
            continue;
        }

        let viewers_by_map = viewers_by_map(&players);
        let enters = match tick_mob_respawns_for_viewers(&viewers_by_map, 128) {
            Ok(enters) => enters,
            Err(error) => {
                logger.line(format!("Rapture World mob respawn tick failed: {error:?}"));
                continue;
            }
        };

        let mut enters_by_map: HashMap<u16, Vec<(f32, f32, Vec<u8>)>> = HashMap::new();
        for (map_id, x, z, packet) in enters {
            enters_by_map
                .entry(map_id)
                .or_default()
                .push((x, z, packet));
        }
        for (map_id, respawns) in enters_by_map {
            for (x, z, packet) in respawns {
                for player in players_on_map_near(map_id, x, z, VIEWPORT_RADIUS) {
                    let _ = player.send_packet(&packet, "0x0601 mob respawn", &logger);
                }
            }
        }
    }
}

pub(crate) fn run_npc_patrol_sweep(logger: Arc<Logger>) {
    loop {
        thread::sleep(Duration::from_secs(4));
        let players = all_players();
        if players.is_empty() {
            continue;
        }

        let viewers_by_map = viewers_by_map(&players);
        let moves = match tick_npc_patrol_for_viewers(&viewers_by_map, 16) {
            Ok(moves) => moves,
            Err(error) => {
                logger.line(format!("Rapture World npc patrol tick failed: {error:?}"));
                continue;
            }
        };

        let mut moves_by_map: HashMap<u16, Vec<(f32, f32, Vec<u8>)>> = HashMap::new();
        for (map_id, x, z, packet) in moves {
            moves_by_map.entry(map_id).or_default().push((x, z, packet));
        }
        for (map_id, moves) in moves_by_map {
            for (x, z, packet) in moves {
                for player in players_on_map_near(map_id, x, z, VIEWPORT_RADIUS) {
                    let _ = player.send_packet(&packet, "0x0E03 npc patrol move", &logger);
                }
            }
        }
    }
}

/// Ticks every active mob DoT (`Skills.TypeDetail == 61`, see
/// world_entities::tick_all_mob_dots) every second (a 1s sweep cadence
/// checking each DoT's own 3-second `next_tick_at`/`ends_at`, same
/// approach as the roam/respawn sweeps above), broadcasting a hit-style
/// packet for each tick and handling kill consequences (death/drops/EXP)
/// exactly like a live attack would.
///
/// EXP attribution: queued to the caster's OWN connection thread via
/// `ConnectedPlayer::queue_mob_kill` if they're still online (drained in
/// main.rs the same way `pending_teleport`/`pending_trade_refresh` are),
/// or granted directly against the DB if they've since disconnected (see
/// `grant_offline_mob_kill_exp`) -- a DoT can easily outlive the caster's
/// own session.
///
/// Packet choices confirmed via a real capture of a basic-damage-plus-DoT
/// cast: each tick broadcasts `MobPeriodicDamage` (0x0607) + a
/// `MobCurrentHitpoints` (0x0610) HP sync, and the DoT's expiry sends
/// `MobDebuffRemove` (0x030D) immediately before its final tick -- see
/// `Opcode::MobPeriodicDamage`/`MobCurrentHitpoints`/`MobDebuffRemove`'s
/// docs.
pub(crate) fn run_mob_dot_sweep(logger: Arc<Logger>) {
    let mut db_conn = match db::connect() {
        Ok(db_conn) => db_conn,
        Err(error) => {
            logger.line(format!("Rapture World dot sweep failed to connect to db: {error:?}"));
            return;
        }
    };

    loop {
        thread::sleep(Duration::from_secs(1));
        for outcome in tick_all_mob_dots() {
            if outcome.applied_hp == 0 {
                continue;
            }

            if outcome.is_final_tick {
                if let Ok(remove_packet) = build_mob_debuff_remove(
                    MOB_DEBUFF_TYPE_DOT,
                    outcome.mob.id,
                    outcome.skill_id,
                    outcome.skill_level,
                ) {
                    for player in players_on_map_near(outcome.map_id, outcome.mob.pos_x, outcome.mob.pos_z, COMBAT_RADIUS) {
                        let _ = player.send_packet(&remove_packet, "0x030D mob debuff remove", &logger);
                    }
                }
            }

            let tick_packet = match build_mob_periodic_damage(
                outcome.mob.id,
                outcome.skill_id,
                outcome.skill_level,
                outcome.applied_hp.min(u32::from(u16::MAX)) as u16,
                0,
                0,
            ) {
                Ok(packet) => packet,
                Err(error) => {
                    logger.line(format!("Rapture World dot tick packet build failed: {error:?}"));
                    continue;
                }
            };
            let recipients = players_on_map_near(outcome.map_id, outcome.mob.pos_x, outcome.mob.pos_z, COMBAT_RADIUS);
            for player in &recipients {
                let _ = player.send_packet(&tick_packet, "0x0607 mob periodic damage", &logger);
            }

            if let Ok(hp_packet) = build_mob_current_hp(outcome.mob.id, outcome.mob.current_hp) {
                for player in &recipients {
                    let _ = player.send_packet(&hp_packet, "0x0610 mob current hp", &logger);
                }
            }

            logger.line(format!(
                "Rapture World dot tick character_id={} skill_id={} level={} mob_global_id={} mob_id={} damage={}{}",
                outcome.caster_character_id,
                outcome.skill_id,
                outcome.skill_level,
                outcome.mob.id,
                outcome.mob.mob_id,
                outcome.applied_hp,
                if outcome.is_final_tick { " (final tick)" } else { "" }
            ));

            if !outcome.killed {
                continue;
            }

            logger.line(format!(
                "Rapture World dot kill character_id={} mob_global_id={} mob_id={} respawn_seconds={:?}",
                outcome.caster_character_id, outcome.mob.id, outcome.mob.mob_id, outcome.respawn_seconds
            ));

            if let Ok(death_packet) = build_mob_death(outcome.mob.id, outcome.caster_character_id) {
                for player in players_on_map_near(outcome.map_id, outcome.mob.pos_x, outcome.mob.pos_z, COMBAT_RADIUS) {
                    let _ = player.send_packet(&death_packet, "0x0606 mob death (dot)", &logger);
                }
            }

            // Best-effort drop height: unlike a live attack (which uses the
            // killer's own live Y, a real ground-height sample), a DoT kill
            // has no such reliable source -- the caster might not even be
            // nearby, or may be offline. Falls back to the caster's last-
            // known live Y if they're still online, else 0.0.
            let drop_pos_y = find_player_by_id(outcome.caster_character_id)
                .map(|player| player.character_snapshot().pos_y)
                .unwrap_or(0.0);
            match roll_mob_drops(
                outcome.map_id,
                outcome.mob.mob_id,
                outcome.mob.money_min,
                outcome.mob.money_max,
                outcome.caster_character_id,
                outcome.mob.pos_x,
                drop_pos_y,
                outcome.mob.pos_z,
                &logger,
            ) {
                Ok(drop_packets) if !drop_packets.is_empty() => {
                    let recipients = players_on_map_near(outcome.map_id, outcome.mob.pos_x, outcome.mob.pos_z, COMBAT_RADIUS);
                    for packet in drop_packets {
                        for player in &recipients {
                            let _ = player.send_packet(&packet, "0x0401 dot kill drop", &logger);
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => logger.line(format!("Rapture World dot kill drop roll failed: {error:?}")),
            }

            if outcome.mob.exp > 0 {
                if let Some(player) = find_player_by_id(outcome.caster_character_id) {
                    player.queue_mob_kill(outcome.mob.mob_id, outcome.mob.level, outcome.mob.exp);
                } else if let Err(error) = grant_offline_mob_kill_exp(
                    &mut db_conn,
                    outcome.caster_character_id,
                    outcome.mob.mob_id,
                    outcome.mob.level,
                    outcome.mob.exp,
                    &logger,
                ) {
                    logger.line(format!("Rapture World offline dot kill exp grant failed: {error:?}"));
                }
            }
        }
    }
}

/// Drives mob->player combat once per second: gathers every online
/// player's live position/alive snapshot, ticks mob AI
/// (`tick_mob_combat_for_viewers` -- aggro acquisition, chase, and attack
/// decisions), then routes each decided attack to its victim's own
/// connection thread via the `pending_damage` mailbox (so HP/death is
/// applied on a single thread, see combat::apply_incoming_mob_damage) and
/// broadcasts each chase move to nearby players. Same 1s cadence and
/// per-map structure as the roam/respawn/dot sweeps.
/// How often mob combat AI advances. Faster than the 1s roam/respawn sweeps
/// so a chasing mob moves smoothly and keeps pace with a running player
/// (see MOB_CHASE_STEP); attack cadence is separately gated by
/// MOB_ATTACK_INTERVAL, so a faster tick doesn't mean faster attacks.
const MOB_COMBAT_TICK: Duration = Duration::from_millis(500);

pub(crate) fn run_mob_combat_sweep(logger: Arc<Logger>) {
    loop {
        thread::sleep(MOB_COMBAT_TICK);
        let players = all_players();
        if players.is_empty() {
            continue;
        }

        let snapshots: Vec<PlayerCombatSnapshot> = players
            .iter()
            .map(|player| {
                let (pos_x, pos_z) = player.position_xz();
                PlayerCombatSnapshot {
                    character_id: player.character_id,
                    map_id: player.map_id(),
                    pos_x,
                    pos_z,
                    alive: player.is_alive(),
                }
            })
            .collect();

        let tick = match tick_mob_combat_for_viewers(&snapshots, 256) {
            Ok(tick) => tick,
            Err(error) => {
                logger.line(format!("Rapture World mob combat tick failed: {error:?}"));
                continue;
            }
        };

        for attack in tick.attacks {
            if let Some(player) = find_player_by_id(attack.target_character_id) {
                player.queue_damage(attack.mob_global_id, attack.mob_id, attack.attack);
            }
        }

        let mut moves_by_map: HashMap<u16, Vec<(f32, f32, Vec<u8>)>> = HashMap::new();
        for (map_id, x, z, packet) in tick.moves {
            moves_by_map.entry(map_id).or_default().push((x, z, packet));
        }
        for (map_id, moves) in moves_by_map {
            for (x, z, packet) in moves {
                for player in players_on_map_near(map_id, x, z, VIEWPORT_RADIUS) {
                    let _ = player.send_packet(&packet, "0x0603 mob chase move", &logger);
                }
            }
        }

        // HP-restore broadcasts for mobs that just reset at home.
        for (map_id, x, z, packet) in tick.restores {
            for player in players_on_map_near(map_id, x, z, COMBAT_RADIUS) {
                let _ = player.send_packet(&packet, "0x0610 mob hp restore on reset", &logger);
            }
        }
    }
}

pub(crate) fn viewers_by_map(players: &[Arc<ConnectedPlayer>]) -> HashMap<u16, Vec<(f32, f32)>> {
    let mut viewers_by_map = HashMap::<u16, Vec<(f32, f32)>>::new();
    for player in players {
        viewers_by_map
            .entry(player.map_id())
            .or_default()
            .push(player.position_xz());
    }
    viewers_by_map
}
