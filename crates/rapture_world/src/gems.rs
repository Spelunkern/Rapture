//! Gem/lapis linking and extraction (0x0801 GemAdd, 0x0802 GemRemove, plus
//! the 0x0809/0x080A "possibility" rate/cost previews).
//!
//! Neither opcode is documented in Aries's own reversed headers (unlike
//! compose/enchant) -- this is decoded purely from real captures: a
//! success + failure pair for GemAdd, and a 1-gem-success + 3-gem-failure
//! pair for GemRemove, cross-checked against each other to pin down field
//! boundaries. See `Opcode::GemAdd`/`GemRemove`/`GemAddPossibility`/
//! `GemRemovePossibility`'s docs for the exact wire shapes.
//!
//! Project design, explicitly simplified per the user (unlike Imgeneus,
//! which rolls a real success chance, can outright destroy the target item
//! on a failed link, and charges gold for both operations):
//! - Linking and extraction ALWAYS succeed -- no chance roll at all.
//! - The target item can NEVER be destroyed.
//! - Both operations are free (no gold cost).
//! - The 0x0809/0x080A "preview" queries always answer 100% / free,
//!   regardless of whether the actual attempt would even be valid --
//!   they're just a UI preview, the real validation happens in
//!   GemAdd/GemRemove itself.
//!
//! This project only implements "general" (hammerless) extraction, which
//! removes EVERY socketed gem from the item at once. Extracting a single
//! specific gem requires a Linking Hammer item, not modeled yet (adds
//! its own eligibility/logic layer -- deferred per the user).
//!
//! Eligibility for linking:
//! - The gem is `Items.Type == GEM_ITEM_TYPE` (30) with `count > 0`.
//! - The target item has a free socket: `Items.Slot` (0-6) for the
//!   target's (Type, TypeID) minus how many of its `gems[0..Slots]` are
//!   already populated.
//! - The target doesn't already carry this exact gem TypeId (no
//!   duplicates in the same item).
//! - Level gate confirmed by the user against the canonical DB: the gem's
//!   own `Items.ReqInt` is the MINIMUM LEVEL OF THE TARGET ITEM it can
//!   link to (not a character-level check) -- `Items.ReqLevel` on the target
//!   must be >= the gem's `ReqInt`. `ReqInt == 0` means no restriction.
//!
//! A gem's stat contribution (Effect1-4/Const* columns, same as any other
//! item) is aggregated live into `db::get_equipment_stats` -- see that
//! function's per-socket loop -- so linking/extracting a gem on an
//! equipped item changes the character's effective stats immediately.

use anyhow::Result;
use rapture_common::db::{self, DbCharacter, DbCharacterItem, GEM_ITEM_TYPE};
use rapture_common::packet::{Opcode, PacketWriter};

use crate::inventory::{build_add_item, find_free_inventory_slots, find_item_index, is_valid_item_position};
use crate::session::Logger;

const GEM_RESULT_FAILURE: u8 = 0;
const GEM_RESULT_SUCCESS: u8 = 1;

/// Max gem sockets any item can have (array size backing `DbCharacterItem
/// ::gems`); also the hard ceiling `Items.Slot` is clamped to.
const MAX_GEM_SLOTS: usize = 6;

/// Fixed "100% / free" preview response shared by both 0x0809
/// (GemAddPossibility) and 0x080A (GemRemovePossibility) -- see their
/// opcode docs for the full byte-level layout, decoded by diffing two real
/// captures. Forces the project's design (always 100%, always free) while
/// replicating the two bytes that were CONSTANT (not zero) across both real
/// samples -- an earlier version zeroed everything here, including those
/// load-bearing constant bytes, which is the likely cause of a real client
/// crash on the following link/extract attempt.
fn build_gem_possibility_result(opcode: Opcode) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(opcode);
    packet.write_u8(GEM_RESULT_SUCCESS);
    for _ in 0..5 {
        packet.write_u8(0); // reserved, confirmed 0 in both real captures
    }
    packet.write_u8(0x80); // flags byte -- varied per pairing in captures (0xC0/0x80); 0x80 is a known-safe real value
    packet.write_u8(100u8 << 1); // rate percent, encoded as (percent << 1) -- confirmed via capture
    packet.write_u8(0x40); // constant in both real captures -- NOT zero
    packet.write_u32(0); // gold cost -- forced free
    packet.into_bytes()
}

pub(crate) fn handle_gem_add_possibility(payload: &[u8], logger: &Logger) -> Result<Option<Vec<u8>>> {
    if payload.len() < 8 {
        logger.line("Rapture World gem add possibility packet too short");
        return Ok(None);
    }
    Ok(Some(build_gem_possibility_result(Opcode::GemAddPossibility)?))
}

pub(crate) fn handle_gem_remove_possibility(payload: &[u8], logger: &Logger) -> Result<Option<Vec<u8>>> {
    if payload.len() < 8 {
        logger.line("Rapture World gem remove possibility packet too short");
        return Ok(None);
    }
    Ok(Some(build_gem_possibility_result(Opcode::GemRemovePossibility)?))
}

#[allow(clippy::too_many_arguments)]
fn build_gem_add_result(
    result: u8,
    gem_bag: u8,
    gem_slot: u8,
    gem_remaining_count: u8,
    item_bag: u8,
    item_slot: u8,
    gem_position: u8,
    gem_type_id: u8,
    remaining_gold: u32,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::GemAdd);
    packet.write_u8(result);
    packet.write_u8(gem_bag);
    packet.write_u8(gem_slot);
    packet.write_u8(gem_remaining_count);
    packet.write_u8(item_bag);
    packet.write_u8(item_slot);
    packet.write_u8(gem_position);
    packet.write_u8(gem_type_id);
    packet.write_u32(0); // reserved
    packet.write_u32(remaining_gold);
    packet.write_u16(0); // reserved
    packet.into_bytes()
}

pub(crate) struct GemAddOutcome {
    pub(crate) response: Vec<u8>,
    /// True on success when the linked-to item is currently equipped -- the
    /// caller should refresh live stat packets.
    pub(crate) item_equipped: bool,
}

/// Handles 0x0801 GemAdd. Returns None only for malformed requests;
/// identifiable-but-invalid attempts (not a gem, item not socketable, no
/// free slot, duplicate gem, target level too low) answer with result 0
/// (Failure), echoing back the unchanged positions/count, nothing consumed.
/// The gold field always echoes `character.money` UNCHANGED (this project
/// never charges for linking) -- a previous version hardcoded 0 there
/// instead, which visibly zeroed the client's displayed gold even though
/// nothing was actually deducted (confirmed fixed on relog, i.e. purely a
/// display bug, not a real balance change).
pub(crate) fn handle_gem_add(
    db_conn: &mut db::Db,
    character: &DbCharacter,
    active_items: &mut Vec<DbCharacterItem>,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<GemAddOutcome>> {
    let character_id = character.id;
    if payload.len() < 8 {
        logger.line(format!("Rapture World gem add packet too short len={}", payload.len()));
        return Ok(None);
    }
    let gem_bag = payload[2];
    let gem_slot = payload[3];
    let item_bag = payload[4];
    let item_slot = payload[5];
    // payload[6..8] is hammerBag/hammerSlot -- unused (no hammer support).

    if !is_valid_item_position(gem_bag, gem_slot) || !is_valid_item_position(item_bag, item_slot) {
        logger.line("Rapture World rejected gem add with invalid positions");
        return Ok(None);
    }

    let Some(gem_index) = find_item_index(active_items, gem_bag, gem_slot) else {
        logger.line("Rapture World rejected gem add: gem slot empty");
        return Ok(None);
    };
    let Some(item_index) = find_item_index(active_items, item_bag, item_slot) else {
        logger.line("Rapture World rejected gem add: item slot empty");
        return Ok(None);
    };
    let gem = active_items[gem_index].clone();
    let item = active_items[item_index].clone();

    let fail = || -> Result<Option<GemAddOutcome>> {
        Ok(Some(GemAddOutcome {
            response: build_gem_add_result(
                GEM_RESULT_FAILURE,
                gem_bag,
                gem_slot,
                gem.count,
                item_bag,
                item_slot,
                0,
                0,
                character.money,
            )?,
            item_equipped: false,
        }))
    };

    if gem.item_type != GEM_ITEM_TYPE || gem.count == 0 {
        logger.line(format!(
            "Rapture World rejected gem add: type={} type_id={} count={} is not a usable gem",
            gem.item_type, gem.type_id, gem.count
        ));
        return fail();
    }

    let Some(target_info) = db::get_item_gem_target_info(db_conn, item.item_type, item.type_id)? else {
        logger.line("Rapture World rejected gem add: unknown target item definition");
        return fail();
    };
    let slots = usize::from(target_info.slots).min(MAX_GEM_SLOTS);
    if slots == 0 {
        logger.line(format!(
            "Rapture World rejected gem add: item type={} type_id={} has no gem sockets",
            item.item_type, item.type_id
        ));
        return fail();
    }
    if item.gems[..slots].contains(&gem.type_id) {
        logger.line(format!(
            "Rapture World rejected gem add: item already has gem type_id={}",
            gem.type_id
        ));
        return fail();
    }
    let Some(position) = item.gems[..slots].iter().position(|&socket| socket == 0) else {
        logger.line(format!(
            "Rapture World rejected gem add: no free socket (slots={slots})"
        ));
        return fail();
    };

    let req_level = db::get_gem_req_level(db_conn, gem.type_id)?.unwrap_or(0);
    if target_info.level < req_level {
        logger.line(format!(
            "Rapture World rejected gem add: target item level={} below gem's required level={req_level}",
            target_info.level
        ));
        return fail();
    }

    db::set_character_item_gem(db_conn, character_id, item.row_id, position as u8, gem.type_id)?;
    if let Some(active_item) = active_items.get_mut(item_index) {
        active_item.gems[position] = gem.type_id;
    }

    let remaining_count = gem.count.saturating_sub(1);
    db::consume_character_item(db_conn, character_id, gem.row_id, remaining_count)?;
    if let Some(index) = find_item_index(active_items, gem_bag, gem_slot) {
        if remaining_count == 0 {
            active_items.remove(index);
        } else if let Some(active_gem) = active_items.get_mut(index) {
            active_gem.count = remaining_count;
        }
    }

    logger.line(format!(
        "Rapture World gem add success character_id={character_id} gem_type_id={} item type={} type_id={} bag={item_bag} slot={item_slot} position={position} gem_remaining={remaining_count}",
        gem.type_id, item.item_type, item.type_id
    ));

    Ok(Some(GemAddOutcome {
        response: build_gem_add_result(
            GEM_RESULT_SUCCESS,
            gem_bag,
            gem_slot,
            remaining_count,
            item_bag,
            item_slot,
            position as u8,
            gem.type_id,
            character.money,
        )?,
        item_equipped: item_bag == 0,
    }))
}

#[allow(clippy::too_many_arguments)]
fn build_gem_remove_result(
    result: u8,
    item_bag: u8,
    item_slot: u8,
    removed: [u8; MAX_GEM_SLOTS],
    gem_type_ids: [u8; MAX_GEM_SLOTS],
    new_bags: [u8; MAX_GEM_SLOTS],
    new_slots: [u8; MAX_GEM_SLOTS],
    remaining_gold: u32,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::GemRemove);
    packet.write_u8(result);
    packet.write_u8(item_bag);
    packet.write_u8(item_slot);
    packet.write_u8(0xFF); // sentinel, constant per capture
    for value in removed {
        packet.write_u8(value);
    }
    for value in gem_type_ids {
        packet.write_u8(value);
    }
    for value in new_bags {
        packet.write_u8(value);
    }
    for value in new_slots {
        packet.write_u8(value);
    }
    packet.write_u32(remaining_gold);
    packet.into_bytes()
}

pub(crate) struct GemRemoveOutcome {
    pub(crate) response: Vec<u8>,
    /// True when at least one gem was actually extracted from a currently
    /// equipped item -- the caller should refresh live stat packets.
    pub(crate) item_equipped: bool,
    /// One 0x0205 AddItem packet per extracted gem, in extraction order.
    /// The 0x0802 response's own `newBag`/`newSlot` arrays were never
    /// confirmed by capture to be how the client actually PLACES multiple
    /// simultaneously-extracted items -- relying on them alone (with no
    /// AddItem at all) was the confirmed cause of extracted gems visibly
    /// stomping existing inventory items: the client independently picks
    /// "first empty slot" from its OWN stale view for each one instead,
    /// colliding when several land in the same batch. Sending the same
    /// proven AddItem packet every other item-granting feature in this
    /// project already uses (shop, item mall, drops) fixes that.
    pub(crate) add_item_packets: Vec<Vec<u8>>,
}

/// Handles 0x0802 GemRemove (general/hammerless extraction only -- see
/// module doc). Extracts every socketed gem from the target item, creating
/// each as a new inventory item. A gem is skipped (stays socketed) if there
/// is no free inventory slot to deliver it to. Returns a Failure result iff
/// NOTHING was extracted (no gems present, or the inventory had no room for
/// any of them) -- matching a real capture of the server failing outright.
/// The gold field always echoes `character.money` UNCHANGED (extraction is
/// free in this project) -- see `handle_gem_add`'s doc for why this can't
/// just be hardcoded 0.
pub(crate) fn handle_gem_remove(
    db_conn: &mut db::Db,
    character: &DbCharacter,
    active_items: &mut Vec<DbCharacterItem>,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<GemRemoveOutcome>> {
    let character_id = character.id;
    if payload.len() < 8 {
        logger.line(format!("Rapture World gem remove packet too short len={}", payload.len()));
        return Ok(None);
    }
    let item_bag = payload[2];
    let item_slot = payload[3];
    // payload[4] shouldRemoveSpecific, payload[5] gemPosition, payload[6..8]
    // hammerBag/hammerSlot -- all unused (general extraction only).

    if !is_valid_item_position(item_bag, item_slot) {
        logger.line("Rapture World rejected gem remove with invalid position");
        return Ok(None);
    }
    let Some(item_index) = find_item_index(active_items, item_bag, item_slot) else {
        logger.line("Rapture World rejected gem remove: item slot empty");
        return Ok(None);
    };
    let item = active_items[item_index].clone();

    let mut removed = [0u8; MAX_GEM_SLOTS];
    let mut gem_type_ids = [0u8; MAX_GEM_SLOTS];
    let mut new_bags = [0u8; MAX_GEM_SLOTS];
    let mut new_slots = [0u8; MAX_GEM_SLOTS];
    let mut add_item_packets = Vec::new();
    let mut any_removed = false;

    for (position, &gem_type_id) in item.gems.iter().enumerate() {
        if gem_type_id == 0 {
            continue;
        }
        let Some(definition) = db::get_item_definition(db_conn, GEM_ITEM_TYPE, gem_type_id)? else {
            logger.line(format!(
                "Rapture World gem remove skipped unknown gem type_id={gem_type_id} at position={position}"
            ));
            continue;
        };
        // Recomputed fresh each iteration -- already reflects every gem
        // placed by an EARLIER iteration of this same loop (each one is
        // pushed into active_items immediately below), so a multi-gem
        // extraction never targets the same free slot twice.
        let Some((bag, slot)) = find_free_inventory_slots(active_items).into_iter().next() else {
            logger.line(format!(
                "Rapture World gem remove skipped position={position}: inventory full"
            ));
            continue;
        };

        db::set_character_item_gem(db_conn, character_id, item.row_id, position as u8, 0)?;
        if let Some(active_item) = active_items.get_mut(item_index) {
            active_item.gems[position] = 0;
        }
        let created = db::create_character_item(db_conn, character_id, &definition, bag, slot, 1)?;
        add_item_packets.push(build_add_item(&created)?);
        active_items.push(created);

        removed[position] = 1;
        gem_type_ids[position] = gem_type_id;
        new_bags[position] = bag;
        new_slots[position] = slot;
        any_removed = true;
    }

    if !any_removed {
        logger.line(format!(
            "Rapture World gem remove FAILED character_id={character_id} item type={} type_id={} bag={item_bag} slot={item_slot} (nothing extracted)",
            item.item_type, item.type_id
        ));
        return Ok(Some(GemRemoveOutcome {
            response: build_gem_remove_result(
                GEM_RESULT_FAILURE,
                item_bag,
                item_slot,
                removed,
                gem_type_ids,
                new_bags,
                new_slots,
                character.money,
            )?,
            item_equipped: false,
            add_item_packets: Vec::new(),
        }));
    }

    logger.line(format!(
        "Rapture World gem remove success character_id={character_id} item type={} type_id={} bag={item_bag} slot={item_slot} removed={removed:?}",
        item.item_type, item.type_id
    ));

    Ok(Some(GemRemoveOutcome {
        response: build_gem_remove_result(
            GEM_RESULT_SUCCESS,
            item_bag,
            item_slot,
            removed,
            gem_type_ids,
            new_bags,
            new_slots,
            character.money,
        )?,
        item_equipped: item_bag == 0,
        add_item_packets,
    }))
}
