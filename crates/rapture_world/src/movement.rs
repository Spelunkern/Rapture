//! Player movement, portals, and teleporting.
//!
//! `apply_teleport` is the single shared path for every kind of teleport
//! (GM cmove/bmove/amove, another connection's queued asummon, and both
//! portal-triggered teleports below) -- using one function for all of them
//! means "another thread teleported me" and "I teleported myself" can never
//! drift apart, and it's also what fixes the "other players don't see me
//! appear on the new map until I move again" bug for portals too, not just
//! GM teleports (the previous inline portal-teleport code in both
//! `CharacterMove` and `CharacterEnteredPortal` duplicated the position/DB/
//! self-ack part of `apply_teleport` but never called its "introduce me to
//! other viewers" step).

use anyhow::Result;
use rapture_common::crypto::AesCtr;
use rapture_common::db::{self, DbCharacter};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::character::*;
use crate::chat::*;
use crate::session::*;
use crate::world_entities::*;
use crate::world_state::*;

pub(crate) fn usable_portal(
    map_id: u16,
    x: f32,
    y: f32,
    z: f32,
    portal_index: Option<u8>,
    faction: u8,
    level: u16,
    logger: &Logger,
) -> Option<MapPortal> {
    let portal = if let Some(index) = portal_index {
        find_portal_by_index(map_id, index)
    } else {
        find_portal_at(map_id, x, y, z)
    }?;

    if !portal.is_open() {
        logger.line(format!(
            "Rapture World rejected closed portal index={} portal_id={} map={}",
            portal.index, portal.portal_id, map_id
        ));
        return None;
    }
    if !portal.allows_faction(faction) {
        logger.line(format!(
            "Rapture World rejected portal faction index={} portal_id={} map={} faction={}",
            portal.index, portal.portal_id, map_id, faction
        ));
        return None;
    }
    if !portal.allows_level(level) {
        logger.line(format!(
            "Rapture World rejected portal level index={} portal_id={} map={} level={} min={} max={}",
            portal.index, portal.portal_id, map_id, level, portal.min_level, portal.max_level
        ));
        return None;
    }

    Some(portal)
}

/// Applies a teleport to the CALLER's own character state (map/position),
/// keeps the shared ConnectedPlayer and DB row in sync, sends the caller's
/// own client the 0x020B/0x0501 pair so it plays correctly from their own
/// perspective, AND (re-)introduces the caller to every other player who
/// can currently see them on the (possibly new) map -- without this last
/// part, other players only learn about the new position/map the next time
/// the teleported character sends a genuine move packet, which is exactly
/// the "GM sees the summoned character still standing in their old spot
/// until they move" bug this fixes. Shared by cmove/bmove/amove
/// (self-teleport), the pending-teleport check in the main loop (another
/// connection, e.g. a GM's asummon, queued this for us -- see
/// ConnectedPlayer::queue_teleport), and both portal-triggered teleports
/// (handle_character_move/handle_character_entered_portal below). Using the
/// same function for all of them means "another thread teleported me" and
/// "I teleported myself" can never drift apart.
pub(crate) fn apply_teleport(
    character: &mut DbCharacter,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    is_admin: bool,
    entity_visibility: &mut EntityVisibility,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    position_dirty: &mut bool,
    last_position_save: &mut Instant,
    map_id: u16,
    x: f32,
    y: f32,
    z: f32,
    label: &str,
    logger: &Logger,
) -> Result<()> {
    character.map_id = map_id;
    character.pos_x = x;
    character.pos_y = y;
    character.pos_z = z;
    *entity_visibility = EntityVisibility::default();
    if let Some(player) = connected_player.as_ref() {
        player.set_map(map_id);
        player.update_character(character.clone());
        player.update_motion(0, 0);
    }
    db::save_character_position(
        db_conn,
        character.id,
        character.map_id,
        character.pos_x,
        character.pos_y,
        character.pos_z,
    )?;
    *position_dirty = false;
    *last_position_save = Instant::now();

    if let Some(send) = aes_send.as_mut() {
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_character_map_position(Some(character))?,
            &format!("0x020B {label}"),
            logger,
        )?;
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_character_move(Some(character), 0, 0)?,
            &format!("0x0501 {label} move"),
            logger,
        )?;
    }

    if let Some(player) = connected_player.as_ref() {
        let others = players_on_map_excluding_near(
            character.map_id,
            character.id,
            character.pos_x,
            character.pos_z,
            VIEWPORT_RADIUS,
        );
        if !others.is_empty() {
            let entered = player.entered_map_packet()?;
            let shape = player.shape_packet()?;
            let move_packet = player.move_packet()?;
            let overlay = player.overlay_packet()?;
            let sitting = player
                .is_sitting()
                .then(|| player.sit_motion_packet())
                .transpose()?;
            for other in &others {
                if !can_see_character(other.is_admin, is_admin, player.gm_visible()) {
                    continue;
                }
                other.send_packet(&entered, &format!("0x0201 {label} introduce"), logger)?;
                other.send_packet(&shape, &format!("0x0303 {label} introduce"), logger)?;
                other.send_packet(&move_packet, &format!("0x0501 {label} introduce"), logger)?;
                other.send_packet(&overlay, &format!("0x0240 {label} introduce"), logger)?;
                if let Some(sit_packet) = &sitting {
                    other.send_packet(sit_packet, &format!("0x0506 {label} introduce"), logger)?;
                }
            }
        }
    }

    // Introduces mobs/NPCs near the destination to the teleported player.
    // `entity_visibility` was just reset above, so this always computes a
    // full "everything nearby" delta -- without it, a same-map teleport
    // (cmove/bmove/asummon, or a portal whose destination is on the same
    // map) left the player seeing no mobs/NPCs at all until their next
    // real 0x0501 move triggered handle_character_move's own delta call.
    // A cross-map teleport is unaffected either way since the client's own
    // CharacterEnteredMap re-populates visibility from scratch.
    let entity_packets =
        build_entity_visibility_delta(character.map_id, character.pos_x, character.pos_z, entity_visibility)?;
    if !entity_packets.is_empty() {
        logger.line(format!(
            "Rapture World entity visibility delta character_id={} packets={} ({label})",
            character.id,
            entity_packets.len()
        ));
    }
    if let Some(send) = aes_send.as_mut() {
        for (packet, entity_label) in entity_packets {
            send_encrypted(stream, send, use_expanded_send_key, packet, entity_label, logger)?;
        }
    }

    Ok(())
}

/// Resurrects a dead character in response to 0x0553 ResurrectRequest.
/// Moves them to the nearest faction town spawn (same respawn point `/town`
/// uses), full-heals them, and replays the real server's revive packet
/// sequence captured from the retail server: 0x0406 (revive init), the
/// standard teleport + area-visibility reload via `apply_teleport`, then
/// 0x0551 (reposition) and 0x0505 (HP/MP/SP restore). Clears the shared
/// `alive` flag last so the combat sweep can target the player again only
/// once they're fully back. Returns whether the character was actually
/// revived (false if there was no active character).
#[allow(clippy::too_many_arguments)]
pub(crate) fn resurrect_character(
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[rapture_common::db::DbCharacterItem],
    is_admin: bool,
    selected_faction: u8,
    entity_visibility: &mut EntityVisibility,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    position_dirty: &mut bool,
    last_position_save: &mut Instant,
    logger: &Logger,
) -> Result<bool> {
    let Some(character) = active_character.as_mut() else {
        return Ok(false);
    };
    let character_id = character.id;
    let map_id = character.map_id;

    // Same respawn point as `/town`: the nearest faction spawn area on the
    // current map. Falls back to the current position if the map has none.
    let (respawn_x, respawn_y, respawn_z) =
        find_nearest_spawn_area(map_id, selected_faction, character.pos_x, character.pos_z)
            .map(|area| area.center())
            .unwrap_or((character.pos_x, character.pos_y, character.pos_z));

    // Full heal: assign raw = max on all three pools (the sanctioned
    // exception to the modify-by-delta HP invariant, see
    // CharacterStats::from_character).
    character.current_hp = character.max_hp;
    character.current_mp = character.max_mp;
    character.current_sp = character.max_sp;

    // Effective (bonus-inclusive) maxes for the 0x0505 restore packet, which
    // carries the displayed values (matching the captured revive).
    let equipment_stats = db::get_equipment_stats(db_conn, active_items)?;
    let stats = CharacterStats::from_character(Some(&*character), equipment_stats);
    let (revive_hp, revive_mp, revive_sp) = (stats.max_hp, stats.max_mp, stats.max_sp);

    db::set_character_status(db_conn, character_id, character.current_hp, character.current_mp, character.current_sp)?;

    // 1) Revive-init, sent before the teleport/reload sequence.
    if let Some(send) = aes_send.as_mut() {
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_character_revive_init(character_id)?,
            "0x0406 revive init",
            logger,
        )?;
    }

    // 2) Teleport to the respawn point + area-visibility reload + introduce
    //    the now-alive character to nearby players.
    apply_teleport(
        character,
        connected_player,
        is_admin,
        entity_visibility,
        db_conn,
        stream,
        aes_send,
        use_expanded_send_key,
        position_dirty,
        last_position_save,
        map_id,
        respawn_x,
        respawn_y,
        respawn_z,
        "resurrect",
        logger,
    )?;

    // 3) Revive reposition + 4) stat restore, self-only.
    if let Some(send) = aes_send.as_mut() {
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_character_resurrect(character_id, respawn_x, respawn_y, respawn_z)?,
            "0x0551 resurrect reposition",
            logger,
        )?;
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_character_revive_hitpoints(character_id, revive_hp, revive_mp, revive_sp)?,
            "0x0505 revive hitpoints",
            logger,
        )?;
    }

    if let Some(player) = connected_player.as_ref() {
        player.set_alive(true);
    }
    logger.line(format!(
        "Rapture World resurrected character_id={character_id} map={map_id} pos=({respawn_x:.2}, {respawn_y:.2}, {respawn_z:.2}) hp={revive_hp}"
    ));
    Ok(true)
}

/// Handles 0x0501 CharacterMove: updates position, checks for a portal at
/// the new spot (walking onto one triggers a teleport the same way an
/// explicit 0x020A CharacterEnteredPortal does -- see
/// handle_character_entered_portal), otherwise broadcasts the move to
/// nearby players and applies any entity-visibility delta.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_character_move(
    decrypted_payload: &[u8],
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    is_admin: bool,
    selected_faction: u8,
    entity_visibility: &mut EntityVisibility,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    position_dirty: &mut bool,
    last_position_save: &mut Instant,
    logger: &Logger,
) -> Result<()> {
    let Some((motion, direction, x, y, z)) = log_character_move(decrypted_payload, logger)? else {
        return Ok(());
    };
    let Some(character) = active_character.as_mut() else {
        return Ok(());
    };
    character.pos_x = x;
    character.pos_y = y;
    character.pos_z = z;
    *position_dirty = true;

    if let Some(player) = connected_player.as_ref() {
        player.update_character(character.clone());
        player.update_motion(motion, direction);
    }
    if let Some(portal) = usable_portal(
        character.map_id,
        character.pos_x,
        character.pos_y,
        character.pos_z,
        None,
        selected_faction,
        character.level,
        logger,
    ) {
        apply_teleport(
            character,
            connected_player,
            is_admin,
            entity_visibility,
            db_conn,
            stream,
            aes_send,
            use_expanded_send_key,
            position_dirty,
            last_position_save,
            portal.target_map_id,
            portal.target_x,
            portal.target_y,
            portal.target_z,
            "portal teleport",
            logger,
        )?;
        return Ok(());
    }

    let others = players_on_map_excluding_near(
        character.map_id,
        character.id,
        character.pos_x,
        character.pos_z,
        VIEWPORT_RADIUS,
    );
    if !others.is_empty() {
        let move_packet = build_character_move(Some(character), motion, direction)?;
        let my_gm_visible = connected_player.as_ref().is_some_and(|p| p.gm_visible());
        for other in &others {
            if !can_see_character(other.is_admin, is_admin, my_gm_visible) {
                continue;
            }
            other.send_packet(&move_packet, "0x0501 character move (broadcast)", logger)?;
        }
    }

    let entity_packets = build_entity_visibility_delta(
        character.map_id,
        character.pos_x,
        character.pos_z,
        entity_visibility,
    )?;
    if !entity_packets.is_empty() {
        logger.line(format!(
            "Rapture World entity visibility delta character_id={} packets={}",
            character.id,
            entity_packets.len()
        ));
    }
    if let Some(send) = aes_send.as_mut() {
        for (packet, label) in entity_packets {
            send_encrypted(stream, send, use_expanded_send_key, packet, label, logger)?;
        }
    }

    if last_position_save.elapsed() >= Duration::from_secs(5) {
        db::save_character_position(
            db_conn,
            character.id,
            character.map_id,
            character.pos_x,
            character.pos_y,
            character.pos_z,
        )?;
        logger.line(format!(
            "Rapture World saved character position id={} map={} pos=({:.2}, {:.2}, {:.2})",
            character.id, character.map_id, character.pos_x, character.pos_y, character.pos_z
        ));
        *position_dirty = false;
        *last_position_save = Instant::now();
    }
    Ok(())
}

/// Handles 0x020A CharacterEnteredPortal (explicit portal use, as opposed
/// to simply walking onto one -- see handle_character_move).
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_character_entered_portal(
    portal_index: u8,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    is_admin: bool,
    selected_faction: u8,
    entity_visibility: &mut EntityVisibility,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    position_dirty: &mut bool,
    last_position_save: &mut Instant,
    logger: &Logger,
) -> Result<()> {
    let Some(character) = active_character.as_mut() else {
        logger.line("Rapture World rejected portal without active character");
        return Ok(());
    };

    let Some(portal) = usable_portal(
        character.map_id,
        character.pos_x,
        character.pos_y,
        character.pos_z,
        Some(portal_index),
        selected_faction,
        character.level,
        logger,
    ) else {
        return Ok(());
    };

    apply_teleport(
        character,
        connected_player,
        is_admin,
        entity_visibility,
        db_conn,
        stream,
        aes_send,
        use_expanded_send_key,
        position_dirty,
        last_position_save,
        portal.target_map_id,
        portal.target_x,
        portal.target_y,
        portal.target_z,
        "explicit portal teleport",
        logger,
    )
}
