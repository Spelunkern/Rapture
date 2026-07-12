//! Background threads that periodically sweep world state (map items, mob
//! roam/respawn, NPC patrol) and broadcast whatever changed to nearby
//! players. Spawned once each from main()'s startup; self-contained --
//! none of this touches per-connection state.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::chat::*;
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
