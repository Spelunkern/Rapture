use anyhow::Result;
use rapture_common::crypto::AesCtr;
use rapture_common::db::{DbCharacter, DbCharacterItem, DbCharacterSkill, DbQuickSlot};
use rapture_common::packet::{Opcode, read_le_u16};
use std::net::TcpStream;

use crate::character::*;
use crate::inventory::*;
use crate::protocol::*;
use crate::session::*;
use crate::skills::*;
use crate::world_entities::*;

pub(crate) fn send_initial_character_state(
    stream: &mut TcpStream,
    aes: &mut AesCtr,
    use_expanded_send_key: &mut bool,
    selected_faction: u8,
    character: Option<&DbCharacter>,
    stats: &CharacterStats,
    items: &[DbCharacterItem],
    skills: &[DbCharacterSkill],
    quick_slots: &[DbQuickSlot],
    logger: &Logger,
) -> Result<()> {
    let before_inventory = [
        (build_world_day()?, "0x0404 world day"),
        (
            build_character_details(character, stats)?,
            "0x0105 character details",
        ),
        (build_kill_count_update(0, 0)?, "0x020E kills"),
        (build_kill_count_update(1, 0)?, "0x020E deaths"),
        (build_kill_count_update(2, 0)?, "0x020E victories"),
        (build_kill_count_update(3, 0)?, "0x020E defeats"),
        (
            build_character_additional_stats(stats)?,
            "0x0526 additional stats",
        ),
        (build_max_hp_mp_sp(character, stats)?, "0x051F max hp/mp/sp"),
        (
            build_max_energy(character, 0, stats.max_hp)?,
            "0x050B max hp",
        ),
        (
            build_max_energy(character, 1, stats.max_sp)?,
            "0x050B max sp",
        ),
        (
            build_max_energy(character, 2, stats.max_mp)?,
            "0x050B max mp",
        ),
        (build_current_hitpoints(stats)?, "0x0521 current hp/mp/sp"),
        (
            build_set_money(character.map_or(0, |character| character.money))?,
            "0x0213 set money",
        ),
    ];

    for (packet, label) in before_inventory {
        send_encrypted(stream, aes, *use_expanded_send_key, packet, label, logger)?;
    }

    if !ENABLE_REAL_INVENTORY_PACKETS && !items.is_empty() {
        logger.line(format!(
            "Rapture World inventory item send disabled for Shaiya 5.4 safety ({} loaded)",
            items.len()
        ));
    }

    for (index, packet) in build_inventory_packets(items)?.into_iter().enumerate() {
        send_encrypted(
            stream,
            aes,
            *use_expanded_send_key,
            packet,
            &format!("0x0106 inventory chunk {}", index + 1),
            logger,
        )?;
    }

    let after_inventory = [
        (build_character_skills(skills)?, "0x0108 skills"),
        (
            build_zero_count_packet(Opcode::QuestList)?,
            "0x0901 open quests",
        ),
        (
            build_zero_count_packet(Opcode::QuestFinishedList)?,
            "0x0906 finished quests",
        ),
        (
            build_zero_count_packet(Opcode::CharacterActiveBuffs)?,
            "0x010A active buffs",
        ),
        (build_bless_init(selected_faction)?, "0x0211 bless"),
        (
            build_zero_count_packet(Opcode::BankItemList)?,
            "0xB101 bank",
        ),
        (build_empty_warehouse()?, "0x0711 warehouse"),
        (
            build_zero_count_packet(Opcode::TeleportSavePositionList)?,
            "0x0225 saved teleports",
        ),
        (build_account_points()?, "0x2601 account points"),
        (build_character_skill_bar(quick_slots)?, "0x010B skill bar"),
    ];

    for (packet, label) in after_inventory {
        send_encrypted(stream, aes, *use_expanded_send_key, packet, label, logger)?;
    }

    *use_expanded_send_key = true;
    logger.line("Rapture World send encryption switched to expanded XOR key");

    Ok(())
}

pub(crate) fn send_map_loaded_state(
    stream: &mut TcpStream,
    aes: &mut AesCtr,
    use_expanded_send_key: bool,
    selected_faction: u8,
    character: Option<&DbCharacter>,
    items: &[DbCharacterItem],
    map_items: &[MapItem],
    map_entities: &MapEntities,
    logger: &Logger,
) -> Result<()> {
    let packets = [
        (
            build_character_map_position(character)?,
            "0x020B set map position",
        ),
        (
            build_character_entered_map(character)?,
            "0x0201 entered map",
        ),
        (
            build_character_shape(character, selected_faction, items, 0)?,
            "0x0303 character shape",
        ),
        (
            build_character_move(character, 0, 0)?,
            "0x0501 character move",
        ),
    ];

    for (packet, label) in packets {
        send_encrypted(stream, aes, use_expanded_send_key, packet, label, logger)?;
    }

    for map_item in map_items {
        send_encrypted(
            stream,
            aes,
            use_expanded_send_key,
            build_map_add_item(map_item)?,
            "0x0401 map add item",
            logger,
        )?;
    }

    for npc in &map_entities.npcs {
        send_encrypted(
            stream,
            aes,
            use_expanded_send_key,
            build_npc_enter(npc)?,
            "0x0E01 npc enter",
            logger,
        )?;
    }

    for mob in &map_entities.mobs {
        send_encrypted(
            stream,
            aes,
            use_expanded_send_key,
            build_mob_enter(mob, false)?,
            "0x0601 mob enter",
            logger,
        )?;
    }

    Ok(())
}

/// GameCharMoveIncoming (0x501, shaiya/include/network/game/incoming/0500.h):
/// opcode + motion:u8 + direction:u16 + x,y,z:f32. Motion/direction must be
/// relayed as-is to other players (see build_character_move) -- previously
/// this function discarded them as three meaningless single bytes
/// ("move_a/move_b/move_c"), which broke cross-player movement: other
/// players always appeared idle-then-teleporting with the wrong facing,
/// because broadcasts always sent motion=0/direction=0 instead of the
/// mover's real running motion and facing.
pub(crate) fn log_character_move(
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<(u8, u16, f32, f32, f32)>> {
    if payload.len() < 17 {
        logger.line(format!(
            "Rapture World character move packet too short len={}",
            payload.len()
        ));
        return Ok(None);
    }

    let motion = payload[2];
    let direction = read_le_u16(&payload[3..5])?;
    let x = read_le_f32(&payload[5..9])?;
    let y = read_le_f32(&payload[9..13])?;
    let z = read_le_f32(&payload[13..17])?;
    if LOG_MOVEMENT_PACKETS {
        logger.line(format!(
            "Rapture World character move motion={motion} direction={direction} pos=({x:.2}, {y:.2}, {z:.2})"
        ));
    }
    Ok(Some((motion, direction, x, y, z)))
}
