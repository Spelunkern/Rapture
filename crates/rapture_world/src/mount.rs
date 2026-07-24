//! Mount ("Vehicle") toggle -- confirmed via a real capture (`USE_VEHICLE`,
//! 0x0216): the client sends an empty-body request, the server replies
//! `ok:u8, isOnVehicle:u8` and broadcasts a shape update. Unlike Imgeneus's
//! reference implementation, this project deliberately skips the cast-time
//! "summoning" step (`USE_VEHICLE_READY`) -- mounting is instant, per the
//! user's explicit design requirement.
//!
//! Gated purely by an equipped item: bag 0, slot 13 ("Vehicle" per Aries's
//! `ItemSlot` enum -- see `inventory::is_valid_item_position`'s doc, already
//! this project's documented mount slot), `Items.Type == 42` -- confirmed
//! against this project's own `PS_GameDefs.dbo.Items` data (e.g. "Horse
//! Summon Stone", Type=42, TypeID=1), same convention Imgeneus uses.

use anyhow::Result;
use rapture_common::crypto::AesCtr;
use rapture_common::db::DbCharacterItem;
use rapture_common::packet::{Opcode, PacketWriter};
use std::net::TcpStream;

use crate::character::equipped_item;
use crate::chat::players_on_map_near;
use crate::session::{Logger, send_encrypted};
use crate::world_entities::VIEWPORT_RADIUS;

pub(crate) const MOUNT_SLOT: u8 = 13;
const MOUNT_ITEM_TYPE: u8 = 42;

/// The `CharacterMountShape` shape byte observed in a real capture of one
/// specific mount. UNCONFIRMED whether every mount category needs a
/// different value here, or if this generic byte works for all of them --
/// the user confirmed the client resolves the actual 3D model from the
/// equipped item itself (slot 13), not from this field, so this is treated
/// as a generic "you are now mounted" signal until a capture with a
/// different equipped mount proves otherwise.
const MOUNT_SHAPE: u8 = 0x19;
const UNMOUNTED_SHAPE: u8 = 0x00;

fn build_use_vehicle_response(ok: bool, is_on_vehicle: bool) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::UseVehicle);
    packet.write_u8(u8::from(ok));
    packet.write_u8(u8::from(is_on_vehicle));
    packet.into_bytes()
}

fn build_character_mount_shape(character_id: u32, shape: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterMountShape);
    packet.write_u32(character_id);
    packet.write_u8(shape);
    packet.into_bytes()
}

/// Handles a `USE_VEHICLE` (0x0216) request: toggles `*mounted`, gated by
/// having a mount item equipped in slot 13. Instant -- no cast/summon
/// delay, per the user's explicit requirement.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_use_vehicle(
    character_id: u32,
    map_id: u16,
    pos_x: f32,
    pos_z: f32,
    active_items: &[DbCharacterItem],
    mounted: &mut bool,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    let next_mounted = if *mounted {
        false
    } else {
        let has_mount =
            equipped_item(active_items, MOUNT_SLOT).is_some_and(|item| item.item_type == MOUNT_ITEM_TYPE);
        if !has_mount {
            logger.line(format!(
                "Rapture World rejected mount toggle without a mount equipped character_id={character_id}"
            ));
            if let Some(send) = aes_send.as_mut() {
                send_encrypted(
                    stream,
                    send,
                    use_expanded_send_key,
                    build_use_vehicle_response(false, false)?,
                    "0x0216 use vehicle rejected",
                    logger,
                )?;
            }
            return Ok(());
        }
        true
    };

    *mounted = next_mounted;
    logger.line(format!("Rapture World character_id={character_id} mounted={next_mounted}"));

    if let Some(send) = aes_send.as_mut() {
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_use_vehicle_response(true, next_mounted)?,
            "0x0216 use vehicle",
            logger,
        )?;
    }

    let shape = if next_mounted { MOUNT_SHAPE } else { UNMOUNTED_SHAPE };
    let shape_packet = build_character_mount_shape(character_id, shape)?;
    for player in players_on_map_near(map_id, pos_x, pos_z, VIEWPORT_RADIUS) {
        let _ = player.send_packet(&shape_packet, "0x051D character mount shape", logger);
    }

    Ok(())
}
