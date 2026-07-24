use anyhow::Result;
use rapture_common::crypto::AesCtr;
use rapture_common::db::{
    self, DbCharacter, DbCharacterItem, DbConsumableEffect, DbEquipmentStats, DbItemDefinition,
};
use rapture_common::packet::{Opcode, PacketWriter, read_le_u32};
use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::character::*;
use crate::chat::*;
use crate::protocol::*;
use crate::session::*;
use crate::world_entities::VIEWPORT_RADIUS;

#[derive(Clone, Debug)]
pub(crate) struct MapItem {
    pub(crate) id: u32,
    pub(crate) item: DbCharacterItem,
    pub(crate) money: u32,
    pub(crate) owner_id: u32,
    pub(crate) dropped_at: Instant,
    pub(crate) pos_x: f32,
    pub(crate) pos_y: f32,
    pub(crate) pos_z: f32,
}

const MONEY_ITEM_TYPE: u8 = 26;

/// How long a dropped item stays reserved for the character who dropped it
/// before anyone else on the map may pick it up.
pub(crate) const MAP_ITEM_OWNER_WINDOW: Duration = Duration::from_secs(30);

pub(crate) fn build_inventory_packets(items: &[DbCharacterItem]) -> Result<Vec<Vec<u8>>> {
    if !ENABLE_REAL_INVENTORY_PACKETS || items.is_empty() {
        return Ok(vec![build_zero_count_packet(Opcode::CharacterItems)?]);
    }

    let mut packets = Vec::new();
    for chunk in items.chunks(50) {
        let mut packet = PacketWriter::new(Opcode::CharacterItems);
        packet.write_u8(chunk.len() as u8);

        for item in chunk {
            packet.write_u8(item.bag);
            packet.write_u8(item.slot);
            packet.write_u8(item.item_type);
            packet.write_u8(item.type_id);
            packet.write_u16(item.quality);
            for gem in item.gems {
                packet.write_u8(gem);
            }
            packet.write_u8(item.count);
            packet.write_fixed_string(&item.craft_name, 21)?;
        }

        packets.push(packet.into_bytes()?);
    }

    Ok(packets)
}

/// Bag 0 (equipment) has 24 real slots per Aries's ItemSlot enum/
/// ItemSlotCount constant (shaiya/include/common/ItemSlot.h): Helmet,
/// UpperArmor, LowerArmor, Gloves, Boots, Weapon, Shield, Cloak, Necklace,
/// Ring1, Ring2, Bracelet1, Bracelet2, Vehicle, Pet, Costume, Wings (0..16),
/// plus reserved slots up to 24. Only slots 0..8 currently render in the
/// character-shape/appearance packets (build_character_shape uses an
/// 8-slot equipment array, matching the real server's EP5 struct variant
/// confirmed in sdev/src/user_shape.cpp) — jewelry/mount/pet/costume/wings
/// equip correctly (persist, stats, etc.) but do not yet change the visible
/// 3D model. A previous, incorrect assumption here capped bag 0 at 8 slots,
/// which blocked equipping jewelry/mounts entirely; do not reintroduce that.
pub(crate) fn is_valid_item_position(bag: u8, slot: u8) -> bool {
    match bag {
        0..=5 => slot < 24,
        _ => false,
    }
}

pub(crate) fn find_item_index(items: &[DbCharacterItem], bag: u8, slot: u8) -> Option<usize> {
    items
        .iter()
        .position(|item| item.bag == bag && item.slot == slot)
}

pub(crate) fn write_bag_to_bag_slot(
    packet: &mut PacketWriter,
    bag: u8,
    slot: u8,
    item: Option<&DbCharacterItem>,
) -> Result<()> {
    packet.write_u8(bag);
    packet.write_u8(slot);

    if let Some(item) = item {
        packet.write_u8(item.item_type);
        packet.write_u8(item.type_id);
        packet.write_u8(item.count);
        packet.write_u16(item.quality);
        for gem in item.gems {
            packet.write_u8(gem);
        }
        packet.write_fixed_string(&item.craft_name, 21)?;
    } else {
        packet.write_u8(0);
        packet.write_u8(0);
        packet.write_u8(0);
        packet.write_u16(0);
        for _ in 0..6 {
            packet.write_u8(0);
        }
        packet.write_fixed_string("", 21)?;
    }

    Ok(())
}

pub(crate) fn build_item_bag_to_bag_result(
    src_bag: u8,
    src_slot: u8,
    src_item: Option<&DbCharacterItem>,
    dest_bag: u8,
    dest_slot: u8,
    dest_item: Option<&DbCharacterItem>,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ItemBagToBag);
    write_bag_to_bag_slot(&mut packet, src_bag, src_slot, src_item)?;
    write_bag_to_bag_slot(&mut packet, dest_bag, dest_slot, dest_item)?;
    packet.into_bytes()
}

pub(crate) fn build_add_item(item: &DbCharacterItem) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::AddItem);
    write_bag_to_bag_slot(&mut packet, item.bag, item.slot, Some(item))?;
    packet.into_bytes()
}

pub(crate) fn build_set_money(money: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::SetMoney);
    packet.write_u32(money);
    packet.into_bytes()
}

pub(crate) fn build_add_item_error(error: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::AddItem);
    packet.write_u8(0);
    packet.write_u8(error);
    packet.into_bytes()
}

pub(crate) fn build_map_add_item(map_item: &MapItem) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MapAddItem);
    packet.write_u32(map_item.id);
    packet.write_u8(1);
    if map_item.money > 0 {
        packet.write_u8(MONEY_ITEM_TYPE);
        packet.write_u8(0);
        packet.write_u8(1);
    } else {
        packet.write_u8(map_item.item.item_type);
        packet.write_u8(map_item.item.type_id);
        packet.write_u8(map_item.item.count);
    }
    packet.write_f32(map_item.pos_x);
    packet.write_f32(map_item.pos_y);
    packet.write_f32(map_item.pos_z);
    packet.write_u32(if map_item.money > 0 {
        0
    } else {
        map_item.owner_id
    });
    packet.into_bytes()
}

pub(crate) fn build_map_remove_item(map_item_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::MapRemoveItem);
    packet.write_u32(map_item_id);
    packet.into_bytes()
}

pub(crate) fn build_use_item_result(
    character_id: u32,
    item: Option<&DbCharacterItem>,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::UseItem);
    packet.write_u32(character_id);

    if let Some(item) = item {
        packet.write_u8(item.bag);
        packet.write_u8(item.slot);
        packet.write_u8(item.item_type);
        packet.write_u8(item.type_id);
        packet.write_u8(item.count);
    } else {
        packet.write_u8(0);
        packet.write_u8(0);
        packet.write_u8(0);
        packet.write_u8(0);
        packet.write_u8(0);
    }

    packet.into_bytes()
}

pub(crate) fn find_free_inventory_slots(items: &[DbCharacterItem]) -> Vec<(u8, u8)> {
    let mut free_slots = Vec::new();
    for bag in INVENTORY_FIRST_BAG..=INVENTORY_LAST_BAG {
        for slot in 0..INVENTORY_SLOT_COUNT {
            if find_item_index(items, bag, slot).is_none() {
                free_slots.push((bag, slot));
            }
        }
    }
    free_slots
}

pub(crate) fn effective_stack_limit(definition: &DbItemDefinition) -> u8 {
    definition.max_stack.max(1)
}

pub(crate) fn items_match_for_stack(src: &DbCharacterItem, dest: &DbCharacterItem) -> bool {
    src.item_type == dest.item_type
        && src.type_id == dest.type_id
        && src.quality == dest.quality
        && src.gems == dest.gems
        && src.craft_name == dest.craft_name
        && src.dyed == dest.dyed
}

pub(crate) fn items_can_stack(src: &DbCharacterItem, dest: &DbCharacterItem) -> bool {
    items_match_for_stack(src, dest) && dest.bag != 0
}

pub(crate) fn build_remove_item(item: &DbCharacterItem, full_remove: bool) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::RemoveItem);
    packet.write_u8(item.bag);
    packet.write_u8(item.slot);

    if full_remove {
        packet.write_u8(0);
        packet.write_u8(0);
        packet.write_u8(0);
    } else {
        packet.write_u8(item.item_type);
        packet.write_u8(item.type_id);
        packet.write_u8(item.count);
    }

    packet.into_bytes()
}

pub(crate) struct UseItemPackets {
    pub(crate) use_result: Vec<u8>,
    pub(crate) remove_item: Option<Vec<u8>>,
    pub(crate) current_hitpoints: Option<Vec<u8>>,
    /// Additional self-packets to send, in order, after the three above.
    /// Used by the reset-stats stone (0x0214 + max-energy + current-hp);
    /// empty for every other item.
    pub(crate) extra_before: Vec<(Vec<u8>, &'static str)>,
}

pub(crate) fn build_cannot_use_item(character_id: u32) -> Result<UseItemPackets> {
    Ok(UseItemPackets {
        use_result: build_use_item_result(character_id, None)?,
        remove_item: None,
        current_hitpoints: None,
        extra_before: Vec::new(),
    })
}

/// PS_GameDefs.dbo.Items.ReqIg -> cooldown duration, matching the real
/// client exactly (values documented from a reversed Shaiya Episode 6
/// server: https://github.com/kurtekat/shaiya-episode-6/tree/main/sdev#cooldowns).
/// ReqIg = 0 (and the otherwise-unused 7/8/9) means no cooldown.
pub(crate) fn cooldown_ms_for_req_ig(req_ig: u8) -> u64 {
    match req_ig {
        1 => 15_000,
        2 => 20_000,
        3 => 25_000,
        4 => 30_000,
        5 => 60_000,
        6 => 120_000,
        10 => 600_000,
        11 => 2_000,
        _ => 0,
    }
}

pub(crate) fn can_apply_consumable_effect(
    item: &DbCharacterItem,
    effect: &DbConsumableEffect,
) -> bool {
    CONSUMABLE_ITEM_TYPES.contains(&item.item_type)
        && matches!(
            effect.special,
            SPECIAL_NONE | SPECIAL_PERCENT_HEALING_POTION
        )
        && (effect.hp > 0 || effect.mp > 0 || effect.sp > 0)
}

pub(crate) fn apply_consumable_recovery(
    character: &mut DbCharacter,
    stats: &CharacterStats,
    _equipment: DbEquipmentStats,
    effect: &DbConsumableEffect,
) -> CharacterStats {
    let recovered_stats = stats.with_recovery(effect);
    let bonus_hp = stats.max_hp.saturating_sub(character.max_hp);
    let bonus_mp = stats.max_mp.saturating_sub(character.max_mp);
    let bonus_sp = stats.max_sp.saturating_sub(character.max_sp);
    character.current_hp = recovered_stats
        .current_hp
        .saturating_sub(bonus_hp)
        .min(character.max_hp);
    character.current_mp = recovered_stats
        .current_mp
        .saturating_sub(bonus_mp)
        .min(character.max_mp);
    character.current_sp = recovered_stats
        .current_sp
        .saturating_sub(bonus_sp)
        .min(character.max_sp);
    recovered_stats
}

pub(crate) fn handle_use_item(
    db_conn: &mut db::Db,
    character: &mut DbCharacter,
    items: &mut Vec<DbCharacterItem>,
    // Keyed by ReqIg (cooldown group), value is when that group was last
    // triggered. Lives for the duration of the character's online session
    // only (cleared on relog/character switch by the caller), matching the
    // desired "cooldowns reset when the character session ends" behavior.
    item_cooldowns: &mut HashMap<u8, Instant>,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<UseItemPackets>> {
    if payload.len() < 4 {
        logger.line(format!(
            "Rapture World use item packet too short len={}",
            payload.len()
        ));
        return Ok(None);
    }

    let bag = payload[2];
    let slot = payload[3];
    logger.line(format!("Rapture World use item bag={bag} slot={slot}"));

    if !is_valid_item_position(bag, slot) {
        logger.line("Rapture World rejected use item with invalid position");
        return Ok(None);
    }

    let Some(item_index) = find_item_index(items, bag, slot) else {
        logger.line("Rapture World rejected use item because slot is empty");
        return build_cannot_use_item(character.id).map(Some);
    };

    let item = items[item_index].clone();
    if item.count == 0 {
        logger.line("Rapture World rejected use item because item count is zero");
        return build_cannot_use_item(character.id).map(Some);
    }

    let Some(effect) = db::get_consumable_effect(db_conn, item.item_type, item.type_id)? else {
        logger.line(format!(
            "Rapture World rejected use item type={} type_id={} because item definition was not found",
            item.item_type, item.type_id
        ));
        return build_cannot_use_item(character.id).map(Some);
    };

    // Name-change stone (Special 51): unlike recovery/appearance items, this
    // has no immediate visual/stat effect -- it just flags the character for a
    // forced rename on next login (RenameCnt=1) and consumes one unit. The
    // client shows nothing here; the rename happens at the next character
    // login (0x010E). Handled before can_apply_consumable_effect, which only
    // recognises recovery specials and would otherwise reject it.
    if effect.special == SPECIAL_CHANGE_NAME {
        if !CONSUMABLE_ITEM_TYPES.contains(&item.item_type) {
            logger.line(format!(
                "Rapture World rejected name-change item type={} type_id={} (not a consumable type)",
                item.item_type, item.type_id
            ));
            return build_cannot_use_item(character.id).map(Some);
        }

        db::set_character_pending_rename(db_conn, character.id)?;
        character.rename_pending = true;

        let remaining_count = item.count.saturating_sub(1);
        db::consume_character_item(db_conn, character.id, item.row_id, remaining_count)?;
        if remaining_count == 0 {
            items.remove(item_index);
        } else if let Some(active_item) = items.get_mut(item_index) {
            active_item.count = remaining_count;
        }

        let mut result_item = item;
        result_item.count = remaining_count;
        logger.line(format!(
            "Rapture World name-change stone used character_id={} item_type={} type_id={} remaining_count={remaining_count} (RenameCnt=1)",
            character.id, result_item.item_type, result_item.type_id
        ));

        // Only the item-consume packet, matching the capture (no 0x0206).
        return Ok(Some(UseItemPackets {
            use_result: build_use_item_result(character.id, Some(&result_item))?,
            remove_item: None,
            current_hitpoints: None,
            extra_before: Vec::new(),
        }));
    }

    // Reset-stats stone (Special 34): base stats back to BASE_STAT, all
    // invested points refunded into the available pool. HP/MP/SP columns are
    // untouched -- the stat-derived portion of the max is recomputed live and
    // pushed via 0x050B/0x0521 below (REC/WIS/DEX feed max hp/mp/sp, so those
    // drop). Handled before can_apply_consumable_effect, which would reject it.
    if effect.special == SPECIAL_RESET_STATS {
        if !CONSUMABLE_ITEM_TYPES.contains(&item.item_type) {
            logger.line(format!(
                "Rapture World rejected reset-stats item type={} type_id={} (not a consumable type)",
                item.item_type, item.type_id
            ));
            return build_cannot_use_item(character.id).map(Some);
        }

        let base = BASE_STAT as u16;
        // Reset grants a fresh level-based pool: STAT_POINTS_PER_LEVEL (5) per
        // level (level 60 -> 300, level 10 -> 50), overwriting whatever was
        // unspent -- base stats go to 10, so the whole pool is available.
        let new_stat_point = character
            .level
            .saturating_mul(STAT_POINTS_PER_LEVEL);

        character.strength = base;
        character.reaction = base;
        character.intelligence = base;
        character.wisdom = base;
        character.dexterity = base;
        character.luck = base;
        character.stat_point = new_stat_point;
        db::set_character_reset_stats(db_conn, character.id, base, new_stat_point)?;

        // Recompute derived maxes/current from the now-reset base stats.
        let equipment_stats = db::get_equipment_stats(db_conn, items)?;
        let stats = CharacterStats::from_character(Some(character), equipment_stats);

        let remaining_count = item.count.saturating_sub(1);
        db::consume_character_item(db_conn, character.id, item.row_id, remaining_count)?;
        if remaining_count == 0 {
            items.remove(item_index);
        } else if let Some(active_item) = items.get_mut(item_index) {
            active_item.count = remaining_count;
        }
        let mut result_item = item;
        result_item.count = remaining_count;

        logger.line(format!(
            "Rapture World reset stats character_id={} level={} new_stat_point={new_stat_point} max_hp={} max_mp={} max_sp={} remaining_count={remaining_count}",
            character.id, character.level, stats.max_hp, stats.max_mp, stats.max_sp
        ));

        // Reset ack, then the new maxes (0=hp,1=sp,2=mp), then the new
        // current -- maxes before current so the client doesn't clamp current
        // to the stale (higher) max.
        let extra_before = vec![
            (
                build_stats_reset(
                    new_stat_point,
                    character.strength,
                    character.reaction,
                    character.intelligence,
                    character.wisdom,
                    character.dexterity,
                    character.luck,
                )?,
                "0x0214 stats reset",
            ),
            (build_max_energy(Some(character), 0, stats.max_hp)?, "0x050B max hp (reset)"),
            (build_max_energy(Some(character), 1, stats.max_sp)?, "0x050B max sp (reset)"),
            (build_max_energy(Some(character), 2, stats.max_mp)?, "0x050B max mp (reset)"),
            (build_current_hitpoints(&stats)?, "0x0521 current hp/mp/sp (reset)"),
        ];

        return Ok(Some(UseItemPackets {
            use_result: build_use_item_result(character.id, Some(&result_item))?,
            remove_item: None,
            current_hitpoints: None,
            extra_before,
        }));
    }

    if !can_apply_consumable_effect(&item, &effect) {
        logger.line(format!(
            "Rapture World rejected use item type={} type_id={} special={} hp={} mp={} sp={}",
            item.item_type, item.type_id, effect.special, effect.hp, effect.mp, effect.sp
        ));
        return build_cannot_use_item(character.id).map(Some);
    }

    let cooldown_ms = cooldown_ms_for_req_ig(effect.req_ig);
    if cooldown_ms > 0
        && let Some(last_used_at) = item_cooldowns.get(&effect.req_ig)
    {
        let elapsed = last_used_at.elapsed();
        let cooldown = Duration::from_millis(cooldown_ms);
        if elapsed < cooldown {
            logger.line(format!(
                "Rapture World rejected use item type={} type_id={} req_ig={} still on cooldown ({}ms remaining)",
                item.item_type,
                item.type_id,
                effect.req_ig,
                (cooldown - elapsed).as_millis()
            ));
            return build_cannot_use_item(character.id).map(Some);
        }
    }

    let equipment_stats = db::get_equipment_stats(db_conn, items)?;
    let current_stats = CharacterStats::from_character(Some(character), equipment_stats);
    let recovered_stats =
        apply_consumable_recovery(character, &current_stats, equipment_stats, &effect);
    let remaining_count = item.count.saturating_sub(1);
    db::consume_character_item(db_conn, character.id, item.row_id, remaining_count)?;

    if cooldown_ms > 0 {
        item_cooldowns.insert(effect.req_ig, Instant::now());
    }

    if remaining_count == 0 {
        items.remove(item_index);
    } else if let Some(active_item) = items.get_mut(item_index) {
        active_item.count = remaining_count;
    }

    let mut result_item = item;
    result_item.count = remaining_count;
    logger.line(format!(
        "Rapture World used consumable type={} type_id={} remaining_count={} current_hp={} current_mp={} current_sp={}",
        result_item.item_type,
        result_item.type_id,
        remaining_count,
        recovered_stats.current_hp,
        recovered_stats.current_mp,
        recovered_stats.current_sp
    ));

    Ok(Some(UseItemPackets {
        use_result: build_use_item_result(character.id, Some(&result_item))?,
        remove_item: Some(build_remove_item(&result_item, remaining_count == 0)?),
        current_hitpoints: Some(build_current_hitpoints(&recovered_stats)?),
        extra_before: Vec::new(),
    }))
}

pub(crate) struct ChangeAppearancePackets {
    /// 0x050A item-consume (self only) -- carries the decremented stack.
    pub(crate) use_result: Vec<u8>,
    /// 0x0226 appearance broadcast (self + nearby).
    pub(crate) broadcast: Vec<u8>,
}

/// Handles 0x0226 ChangeAppearance: validates that (bag, slot) holds a
/// change-appearance (Special 44) or change-sex (Special 45) consumable,
/// applies the chosen hair/face/height (and sex, only for a Special-45 item)
/// to `character`, persists it, and consumes one unit. The sex byte from a
/// Special-44 item is ignored (an appearance item can never change sex, even
/// from a modified client). Returns the self item-consume packet and the
/// appearance broadcast, or None if the request is invalid.
pub(crate) fn handle_change_appearance(
    db_conn: &mut db::Db,
    character: &mut DbCharacter,
    items: &mut Vec<DbCharacterItem>,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<ChangeAppearancePackets>> {
    // [0..2] opcode, then bag, slot, hair, face, height, sex.
    if payload.len() < 8 {
        logger.line(format!(
            "Rapture World change appearance packet too short len={}",
            payload.len()
        ));
        return Ok(None);
    }
    let (bag, slot) = (payload[2], payload[3]);
    let (hair, face, height, sex) = (payload[4], payload[5], payload[6], payload[7]);

    if !is_valid_item_position(bag, slot) {
        logger.line("Rapture World rejected change appearance with invalid item position");
        return Ok(None);
    }
    let Some(item_index) = find_item_index(items, bag, slot) else {
        logger.line("Rapture World rejected change appearance because slot is empty");
        return Ok(None);
    };
    let item = items[item_index].clone();
    if item.count == 0 {
        logger.line("Rapture World rejected change appearance because item count is zero");
        return Ok(None);
    }

    let Some(effect) = db::get_consumable_effect(db_conn, item.item_type, item.type_id)? else {
        logger.line(format!(
            "Rapture World rejected change appearance type={} type_id={} (no item definition)",
            item.item_type, item.type_id
        ));
        return Ok(None);
    };

    let changes_sex = match effect.special {
        SPECIAL_CHANGE_APPEARANCE => false,
        SPECIAL_CHANGE_SEX => true,
        _ => {
            logger.line(format!(
                "Rapture World rejected change appearance type={} type_id={} special={} (not an appearance item)",
                item.item_type, item.type_id, effect.special
            ));
            return Ok(None);
        }
    };
    if !CONSUMABLE_ITEM_TYPES.contains(&item.item_type) {
        logger.line(format!(
            "Rapture World rejected change appearance type={} type_id={} (not a consumable type)",
            item.item_type, item.type_id
        ));
        return Ok(None);
    }

    // A Special-44 item never changes sex, regardless of the sex byte sent.
    // For a Special-45 item, only 0 (male) / 1 (female) are valid.
    let new_sex = if changes_sex {
        if sex > 1 {
            logger.line(format!("Rapture World rejected change sex with invalid sex value {sex}"));
            return Ok(None);
        }
        sex
    } else {
        character.gender
    };

    character.hair = hair;
    character.face = face;
    character.height = height;
    character.gender = new_sex;
    db::set_character_appearance(db_conn, character.id, hair, face, height, new_sex)?;

    let remaining_count = item.count.saturating_sub(1);
    db::consume_character_item(db_conn, character.id, item.row_id, remaining_count)?;
    if remaining_count == 0 {
        items.remove(item_index);
    } else if let Some(active_item) = items.get_mut(item_index) {
        active_item.count = remaining_count;
    }

    let mut result_item = item;
    result_item.count = remaining_count;
    logger.line(format!(
        "Rapture World change appearance character_id={} special={} hair={hair} face={face} height={height} sex={new_sex} item_type={} type_id={} remaining_count={remaining_count}",
        character.id, effect.special, result_item.item_type, result_item.type_id
    ));

    Ok(Some(ChangeAppearancePackets {
        use_result: build_use_item_result(character.id, Some(&result_item))?,
        broadcast: build_change_appearance(character.id, hair, face, height, new_sex)?,
    }))
}

pub(crate) struct DropItemPackets {
    pub(crate) remove_item: Vec<u8>,
    pub(crate) map_add_item: Vec<u8>,
    pub(crate) equipment_slot: Option<u8>,
    pub(crate) equipment_stats_changed: bool,
}

pub(crate) fn handle_drop_item(
    db_conn: &mut db::Db,
    character: &DbCharacter,
    items: &mut Vec<DbCharacterItem>,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<DropItemPackets>> {
    if payload.len() < 5 {
        logger.line(format!(
            "Rapture World drop item packet too short len={}",
            payload.len()
        ));
        return Ok(None);
    }

    let bag = payload[2];
    let slot = payload[3];
    let requested_count = payload[4];
    logger.line(format!(
        "Rapture World drop item bag={bag} slot={slot} count={requested_count}"
    ));

    if !is_valid_item_position(bag, slot) {
        logger.line("Rapture World rejected drop item with invalid position");
        return Ok(None);
    }

    let Some(item_index) = find_item_index(items, bag, slot) else {
        logger.line("Rapture World rejected drop item because slot is empty");
        return Ok(None);
    };

    let item = items[item_index].clone();
    let drop_count = if requested_count == 0 || requested_count > item.count {
        item.count
    } else {
        requested_count
    };
    if drop_count == 0 {
        logger.line("Rapture World rejected drop item because count is zero");
        return Ok(None);
    }

    let remaining_count = item.count.saturating_sub(drop_count);
    let mut dropped_item = item.clone();
    dropped_item.count = drop_count;
    dropped_item.bag = 0;
    dropped_item.slot = 0;

    let map_item_id = crate::map_items::next_item_id();
    let map_item = MapItem {
        id: map_item_id,
        item: dropped_item,
        money: 0,
        owner_id: character.id,
        dropped_at: Instant::now(),
        pos_x: character.pos_x,
        pos_y: character.pos_y,
        pos_z: character.pos_z,
    };

    let mut result_item = item.clone();
    result_item.count = remaining_count;
    if remaining_count == 0 {
        db::delete_character_item(db_conn, character.id, item.row_id)?;
        items.remove(item_index);
    } else {
        db::set_character_item_count(db_conn, character.id, item.row_id, remaining_count)?;
        if let Some(active_item) = items.get_mut(item_index) {
            active_item.count = remaining_count;
        }
    }

    let equipment_slot = (bag == 0 && slot < 8).then_some(slot);
    let map_add_item = build_map_add_item(&map_item)?;
    crate::map_items::add_item(character.map_id, map_item);

    logger.line(format!(
        "Rapture World dropped item row_id={} map_item_id={} type={} type_id={} count={} remaining={}",
        item.row_id, map_item_id, item.item_type, item.type_id, drop_count, remaining_count
    ));

    Ok(Some(DropItemPackets {
        remove_item: build_remove_item(&result_item, remaining_count == 0)?,
        map_add_item,
        equipment_slot,
        equipment_stats_changed: bag == 0,
    }))
}

pub(crate) struct PickupItemPackets {
    pub(crate) add_item_packets: Vec<Vec<u8>>,
    pub(crate) money_update: Option<Vec<u8>>,
    pub(crate) new_money: Option<u32>,
    pub(crate) map_remove_item: Option<Vec<u8>>,
    pub(crate) map_add_item: Option<Vec<u8>>,
}

pub(crate) fn find_stackable_item_index(
    items: &[DbCharacterItem],
    template: &DbCharacterItem,
    max_stack: u8,
) -> Option<usize> {
    items
        .iter()
        .position(|item| items_can_stack(template, item) && item.count < max_stack)
}

/// Dry-run counterpart to `give_item_to_inventory`: simulates placing
/// `count` of `template` into `sim_items` (same stack-first,
/// free-slot-fallback logic, no DB writes) and reports whether it would all
/// fit with zero leftover. `sim_items` is mutated in place (so a caller
/// checking multiple offered items in sequence sees each earlier one's
/// simulated slot/stack usage) -- callers that only want a yes/no answer
/// should pass a scratch clone and discard it afterward regardless of the
/// result. Used to validate BOTH sides of a trade have room for what
/// they're about to receive before committing any DB writes, so a full
/// inventory aborts the whole trade instead of silently dropping/bouncing
/// items back (see trade.rs's finalize_trade).
pub(crate) fn would_fit(
    sim_items: &mut Vec<DbCharacterItem>,
    template: &DbCharacterItem,
    count: u8,
    max_stack: u8,
) -> bool {
    let mut remaining = count;
    while remaining > 0 {
        if let Some(stack_index) = find_stackable_item_index(sim_items, template, max_stack) {
            let free_space = max_stack.saturating_sub(sim_items[stack_index].count);
            let moved = remaining.min(free_space);
            sim_items[stack_index].count = sim_items[stack_index].count.saturating_add(moved);
            remaining = remaining.saturating_sub(moved);
            continue;
        }
        let Some((bag, slot)) = find_free_inventory_slots(sim_items).into_iter().next() else {
            return false;
        };
        let stack_count = remaining.min(max_stack);
        sim_items.push(DbCharacterItem {
            row_id: 0,
            bag,
            slot,
            item_type: template.item_type,
            type_id: template.type_id,
            quality: template.quality,
            gems: template.gems,
            count: stack_count,
            craft_name: template.craft_name.clone(),
            dyed: template.dyed,
        });
        remaining = remaining.saturating_sub(stack_count);
    }
    true
}

/// Adds `count` of `template` (its own bag/slot/count are ignored) to
/// `character_id`'s inventory, stacking into an existing compatible stack
/// first and falling back to the first free slot. `items` is kept in sync
/// with every DB write. Returns the AddItem packets for whatever was
/// actually placed, and the leftover count that didn't fit anywhere
/// (0 unless the inventory is full). Shared by pickup-from-ground and
/// trade-completion, which both need identical stacking behavior.
pub(crate) fn give_item_to_inventory(
    db_conn: &mut db::Db,
    character_id: u32,
    items: &mut Vec<DbCharacterItem>,
    template: &DbCharacterItem,
    count: u8,
    max_stack: u8,
) -> Result<(Vec<Vec<u8>>, u8)> {
    let mut remaining = count;
    let mut add_item_packets = Vec::new();
    while remaining > 0 {
        if let Some(stack_index) = find_stackable_item_index(items, template, max_stack) {
            let free_space = max_stack.saturating_sub(items[stack_index].count);
            let moved_count = remaining.min(free_space);
            let new_count = items[stack_index].count.saturating_add(moved_count);
            db::set_character_item_count(
                db_conn,
                character_id,
                items[stack_index].row_id,
                new_count,
            )?;
            items[stack_index].count = new_count;
            add_item_packets.push(build_add_item(&items[stack_index])?);
            remaining = remaining.saturating_sub(moved_count);
            continue;
        }

        let Some((bag, slot)) = find_free_inventory_slots(items).into_iter().next() else {
            break;
        };
        let stack_count = remaining.min(max_stack);
        let created_item =
            db::create_character_item_from_template(db_conn, character_id, template, bag, slot, stack_count)?;
        add_item_packets.push(build_add_item(&created_item)?);
        items.push(created_item);
        remaining = remaining.saturating_sub(stack_count);
    }
    Ok((add_item_packets, remaining))
}

pub(crate) fn handle_pickup_item(
    db_conn: &mut db::Db,
    character: &DbCharacter,
    items: &mut Vec<DbCharacterItem>,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<PickupItemPackets>> {
    if payload.len() < 6 {
        logger.line(format!(
            "Rapture World pickup item packet too short len={}",
            payload.len()
        ));
        return Ok(None);
    }

    let map_item_id = read_le_u32(&payload[2..6])?;
    logger.line(format!(
        "Rapture World pickup item map_item_id={map_item_id}"
    ));

    let Some(map_item) = crate::map_items::find_item(character.map_id, map_item_id) else {
        logger.line("Rapture World rejected pickup because map item was not found");
        return Ok(Some(PickupItemPackets {
            add_item_packets: vec![build_add_item_error(0)?],
            money_update: None,
            new_money: None,
            map_remove_item: None,
            map_add_item: None,
        }));
    };

    if map_item.owner_id != character.id && map_item.dropped_at.elapsed() < MAP_ITEM_OWNER_WINDOW {
        logger.line(format!(
            "Rapture World rejected pickup because map_item_id={map_item_id} is still reserved for owner_id={}",
            map_item.owner_id
        ));
        return Ok(Some(PickupItemPackets {
            add_item_packets: vec![build_add_item_error(2)?],
            money_update: None,
            new_money: None,
            map_remove_item: None,
            map_add_item: None,
        }));
    }

    if map_item.money > 0 {
        let new_money = db::add_character_money(db_conn, character.id, map_item.money)?;
        crate::map_items::remove_item(character.map_id, map_item.id);
        logger.line(format!(
            "Rapture World picked up money map_item_id={} amount={} new_money={}",
            map_item.id, map_item.money, new_money
        ));
        // Real Aries shows the "obtained" toast purely from the 0x0205
        // AddItem packet -- there's no separate loot-notification opcode
        // (confirmed against Aries's outgoing 0200.h and mirrored by
        // Imgeneus, which sends the same AddItem packet type for money as
        // for regular items). So gold pickups need this packet too, with
        // item_type = Gold (26), not just the 0x0213 total-sync packet
        // below, or the client shows the gold silently.
        //
        // bag must NOT be 0 -- in this codebase's (and Aries's) convention
        // bag=0 means "equipped slot", so a Gold-type item there is a
        // nonsensical target the client likely discards outright. Imgeneus's
        // equivalent serializer explicitly special-cases Bag=1 for money for
        // the same reason; mirrored here.
        // The wire "count" field is a single byte (max 255), far too small
        // for a gold amount, and the toast showed a flat 0 with count
        // carrying the amount -- so the client reads the obtained quantity
        // from "quality" (u16) for a Gold-type item instead, not "count".
        // count is left at a nominal 1, matching a normal single (non-stack)
        // item.
        let gold_get_item = DbCharacterItem {
            row_id: 0,
            bag: 1,
            slot: 0,
            item_type: MONEY_ITEM_TYPE,
            type_id: 0,
            quality: map_item.money.min(u16::MAX as u32) as u16,
            gems: [0; 6],
            count: 1,
            craft_name: String::new(),
            dyed: false,
        };
        return Ok(Some(PickupItemPackets {
            add_item_packets: vec![build_add_item(&gold_get_item)?],
            money_update: Some(build_set_money(new_money)?),
            new_money: Some(new_money),
            map_remove_item: Some(build_map_remove_item(map_item.id)?),
            map_add_item: None,
        }));
    }

    let Some(definition) =
        db::get_item_definition(db_conn, map_item.item.item_type, map_item.item.type_id)?
    else {
        logger.line("Rapture World rejected pickup because item definition was not found");
        return Ok(Some(PickupItemPackets {
            add_item_packets: vec![build_add_item_error(0)?],
            money_update: None,
            new_money: None,
            map_remove_item: None,
            map_add_item: None,
        }));
    };

    let max_stack = effective_stack_limit(&definition);
    let (add_item_packets, remaining) = give_item_to_inventory(
        db_conn,
        character.id,
        items,
        &map_item.item,
        map_item.item.count,
        max_stack,
    )?;

    if add_item_packets.is_empty() {
        logger.line("Rapture World rejected pickup because inventory is full");
        return Ok(Some(PickupItemPackets {
            add_item_packets: vec![build_add_item_error(1)?],
            money_update: None,
            new_money: None,
            map_remove_item: None,
            map_add_item: None,
        }));
    }

    let map_remove_item = Some(build_map_remove_item(map_item.id)?);
    let map_add_item = if remaining == 0 {
        crate::map_items::remove_item(character.map_id, map_item.id);
        None
    } else {
        let updated = crate::map_items::set_item_count(character.map_id, map_item.id, remaining);
        updated.as_ref().map(build_map_add_item).transpose()?
    };

    logger.line(format!(
        "Rapture World picked up map_item_id={} type={} type_id={} picked={} remaining={}",
        map_item.id,
        map_item.item.item_type,
        map_item.item.type_id,
        map_item.item.count.saturating_sub(remaining),
        remaining
    ));

    Ok(Some(PickupItemPackets {
        add_item_packets,
        money_update: None,
        new_money: None,
        map_remove_item,
        map_add_item,
    }))
}

pub(crate) fn handle_item_bag_to_bag(
    db_conn: &mut db::Db,
    character_id: u32,
    items: &mut Vec<DbCharacterItem>,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<Vec<u8>>> {
    if payload.len() < 6 {
        logger.line(format!(
            "Rapture World item move packet too short len={}",
            payload.len()
        ));
        return Ok(None);
    }

    let src_bag = payload[2];
    let src_slot = payload[3];
    let dest_bag = payload[4];
    let dest_slot = payload[5];
    logger.line(format!(
        "Rapture World item move src=({src_bag},{src_slot}) dest=({dest_bag},{dest_slot})"
    ));

    if !is_valid_item_position(src_bag, src_slot) || !is_valid_item_position(dest_bag, dest_slot) {
        logger.line("Rapture World rejected item move with invalid position");
        return Ok(None);
    }

    let src_index = find_item_index(items, src_bag, src_slot);
    let dest_index = find_item_index(items, dest_bag, dest_slot);

    let Some(src_index) = src_index else {
        logger.line("Rapture World rejected item move because source slot is empty");
        let dest_item = dest_index.map(|index| &items[index]);
        return build_item_bag_to_bag_result(
            src_bag, src_slot, None, dest_bag, dest_slot, dest_item,
        )
        .map(Some);
    };

    if src_bag == dest_bag && src_slot == dest_slot {
        let item = &items[src_index];
        return build_item_bag_to_bag_result(
            src_bag,
            src_slot,
            Some(item),
            dest_bag,
            dest_slot,
            Some(item),
        )
        .map(Some);
    }

    if let Some(dest_index) = dest_index {
        let src_item = items[src_index].clone();
        let dest_item = items[dest_index].clone();

        if items_can_stack(&src_item, &dest_item) {
            let Some(definition) =
                db::get_item_definition(db_conn, src_item.item_type, src_item.type_id)?
            else {
                logger.line(
                    "Rapture World rejected stack merge because item definition was not found",
                );
                return Ok(None);
            };
            let max_stack = effective_stack_limit(&definition);

            if max_stack > 1 && dest_item.count < max_stack {
                let free_space = max_stack.saturating_sub(dest_item.count);
                let moved_count = src_item.count.min(free_space);
                let remaining_src_count = src_item.count.saturating_sub(moved_count);
                let new_dest_count = dest_item.count.saturating_add(moved_count);

                db::set_character_item_count(
                    db_conn,
                    character_id,
                    dest_item.row_id,
                    new_dest_count,
                )?;
                if remaining_src_count == 0 {
                    db::delete_character_item(db_conn, character_id, src_item.row_id)?;
                    items.remove(src_index);
                } else if let Some(item) = items.get_mut(src_index) {
                    db::set_character_item_count(
                        db_conn,
                        character_id,
                        src_item.row_id,
                        remaining_src_count,
                    )?;
                    item.count = remaining_src_count;
                }

                if let Some(item) = items
                    .iter_mut()
                    .find(|item| item.row_id == dest_item.row_id)
                {
                    item.count = new_dest_count;
                }

                let src_after = items
                    .iter()
                    .find(|item| item.row_id == src_item.row_id)
                    .filter(|item| item.count > 0);
                let dest_after = items.iter().find(|item| item.row_id == dest_item.row_id);
                logger.line(format!(
                    "Rapture World merged stack src_row_id={} dest_row_id={} moved={} src_left={} dest_count={}",
                    src_item.row_id,
                    dest_item.row_id,
                    moved_count,
                    remaining_src_count,
                    new_dest_count
                ));

                return build_item_bag_to_bag_result(
                    src_bag, src_slot, src_after, dest_bag, dest_slot, dest_after,
                )
                .map(Some);
            }
        }

        db::swap_character_items(
            db_conn,
            character_id,
            src_item.row_id,
            dest_bag,
            dest_slot,
            dest_item.row_id,
            src_bag,
            src_slot,
        )?;

        items[src_index].bag = dest_bag;
        items[src_index].slot = dest_slot;
        items[dest_index].bag = src_bag;
        items[dest_index].slot = src_slot;

        logger.line(format!(
            "Rapture World swapped items row_id={} <-> row_id={}",
            src_item.row_id, dest_item.row_id
        ));

        build_item_bag_to_bag_result(
            src_bag,
            src_slot,
            Some(&items[dest_index]),
            dest_bag,
            dest_slot,
            Some(&items[src_index]),
        )
        .map(Some)
    } else {
        let src_item = items[src_index].clone();
        db::move_character_item(db_conn, character_id, src_item.row_id, dest_bag, dest_slot)?;

        items[src_index].bag = dest_bag;
        items[src_index].slot = dest_slot;

        logger.line(format!(
            "Rapture World moved item row_id={} to ({dest_bag},{dest_slot})",
            src_item.row_id
        ));

        build_item_bag_to_bag_result(
            src_bag,
            src_slot,
            None,
            dest_bag,
            dest_slot,
            Some(&items[src_index]),
        )
        .map(Some)
    }
}

/// Full handler for 0x0204 ItemBagToBag: moves the item (via
/// `handle_item_bag_to_bag`), then sends everything that move implies --
/// the 0x0204 ack, an 0x0507 equipment update for any of slots 0-7 touched
/// (source and/or dest), a broadcast of those slots to other viewers who
/// can see this character, and a recalculated 0x0526/0x051F/0x050B stat
/// bundle if either side of the move was an equipment slot (gear affects
/// derived stats).
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_item_bag_to_bag_request(
    db_conn: &mut db::Db,
    character: &DbCharacter,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    is_admin: bool,
    active_items: &mut Vec<DbCharacterItem>,
    decrypted_payload: &[u8],
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    let (changed_equipment_slots, equipment_stats_changed) = if decrypted_payload.len() >= 6 {
        let src_bag = decrypted_payload[2];
        let src_slot = decrypted_payload[3];
        let dest_bag = decrypted_payload[4];
        let dest_slot = decrypted_payload[5];
        let mut slots = Vec::new();
        if src_bag == 0 && src_slot < 8 {
            slots.push(src_slot);
        }
        if dest_bag == 0 && dest_slot < 8 && !slots.contains(&dest_slot) {
            slots.push(dest_slot);
        }
        (slots, src_bag == 0 || dest_bag == 0)
    } else {
        (Vec::new(), false)
    };

    let Some(packet) =
        handle_item_bag_to_bag(db_conn, character.id, active_items, decrypted_payload, logger)?
    else {
        return Ok(());
    };
    let Some(send) = aes_send.as_mut() else {
        return Ok(());
    };

    send_encrypted(
        stream,
        send,
        use_expanded_send_key,
        packet,
        "0x0204 item bag-to-bag",
        logger,
    )?;

    for &slot in &changed_equipment_slots {
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_character_equipment(character, active_items, slot)?,
            &format!("0x0507 character equipment slot {slot}"),
            logger,
        )?;
    }

    if !changed_equipment_slots.is_empty() {
        if let Some(player) = connected_player.as_ref() {
            player.update_items(active_items.clone());
        }
        let others = players_on_map_excluding_near(
            character.map_id,
            character.id,
            character.pos_x,
            character.pos_z,
            VIEWPORT_RADIUS,
        );
        for &slot in &changed_equipment_slots {
            if let Some(player) = connected_player.as_ref() {
                let equipment_packet = player.equipment_packet(slot)?;
                for other in &others {
                    if !can_see_character(other.is_admin, is_admin, player.gm_visible()) {
                        continue;
                    }
                    other.send_packet(
                        &equipment_packet,
                        &format!("0x0507 character equipment slot {slot} (broadcast)"),
                        logger,
                    )?;
                }
            }
        }
    }

    if equipment_stats_changed {
        let equipment_stats = db::get_equipment_stats(db_conn, active_items)?;
        let character_stats = CharacterStats::from_character(Some(character), equipment_stats);
        logger.line(format!(
            "Rapture World refreshed equipment stats hp={} mp={} sp={} str={} dex={} rec={} int={} wis={} luc={} atk={}-{} magic={}-{} def={} resist={}",
            character_stats.max_hp,
            character_stats.max_mp,
            character_stats.max_sp,
            character_stats.strength,
            character_stats.dexterity,
            character_stats.reaction,
            character_stats.intelligence,
            character_stats.wisdom,
            character_stats.luck,
            character_stats.min_attack,
            character_stats.max_attack,
            character_stats.min_magic,
            character_stats.max_magic,
            character_stats.defense,
            character_stats.resistance
        ));
        send_character_stat_packets(
            stream,
            send,
            use_expanded_send_key,
            Some(character),
            &character_stats,
            logger,
        )?;
    }

    Ok(())
}
