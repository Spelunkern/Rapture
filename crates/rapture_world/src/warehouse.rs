//! Personal warehouse (account-wide item storage), separate from a
//! character's own inventory. Backed by `PS_GameData.dbo.UserStoredItems`
//! (keyed by account `UserUID`, shared across every character on the
//! account -- see `db::get_stored_items`).
//!
//! Deposit/withdraw/rearrange all reuse the client's normal 0x0204
//! `ItemBagToBag` opcode, distinguished from a plain inventory move by the
//! Aries client's `Bag::Warehouse` id (100) on either side of the move --
//! confirmed against a real capture: `SEND>>04 02 01 06 64 00` moving
//! inventory bag 1 slot 6 into warehouse (bag 100) slot 0.
//!
//! There is no "open warehouse" opcode at all -- confirmed via a real
//! capture that touching the warehouse NPC is just a plain 0x0501
//! CharacterMove (walking up to it) and that opening/closing the UI
//! generates zero network traffic. The client must already have the full
//! warehouse contents by then, which it does: `WarehouseItemList` is sent
//! proactively at login/map-enter alongside the inventory (see
//! `world_state::send_initial_character_state`), not on demand.
//!
//! Warehouse gold is intentionally NOT implemented (item storage only), per
//! an explicit decision to keep the first pass scoped to items even though
//! the retail schema has a separate `UserStoredMoney` table for it.

use anyhow::Result;
use rapture_common::db::{self, DbCharacterItem};
use rapture_common::packet::{Opcode, PacketWriter};

use crate::inventory::{build_item_bag_to_bag_result, effective_stack_limit, items_can_stack};
use crate::session::Logger;

/// Aries's `Bag` enum value for warehouse storage (`Bag.h`: `Warehouse = 100`).
pub(crate) const WAREHOUSE_BAG: u8 = 100;

/// Base warehouse capacity (120 slots). Retail doubles this to 240 via a
/// "double warehouse" skill/status flag, which isn't implemented yet.
pub(crate) const WAREHOUSE_SLOTS: u8 = 120;

pub(crate) fn is_valid_warehouse_slot(slot: u8) -> bool {
    slot < WAREHOUSE_SLOTS
}

fn find_stored_item_index(items: &[DbCharacterItem], slot: u8) -> Option<usize> {
    items.iter().position(|item| item.slot == slot)
}

/// Builds the 0x0711 `WarehouseItemList` response, sent proactively at
/// character login/map-enter (see `world_state::send_initial_character_state`)
/// alongside the inventory -- confirmed via a real capture that opening/
/// closing the warehouse UI generates no network traffic at all, so the
/// client must already have this data by the time the window opens. Gold is
/// always written as 0 (warehouse gold isn't implemented). Item wire layout
/// (slot/type/typeId/quality/gems/count/craftname) confirmed byte-for-byte
/// against a real deposit response for a known item (type 14/1).
pub(crate) fn build_warehouse_item_list(items: &[DbCharacterItem]) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::WarehouseItemList);
    packet.write_u32(0); // warehouse gold: not implemented
    packet.write_u8(items.len() as u8);

    for item in items {
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

    packet.into_bytes()
}

/// Handles a 0x0204 `ItemBagToBag` move where either side is the warehouse
/// bag (100): deposit (inventory -> warehouse), withdraw (warehouse ->
/// inventory), or a rearrange within the warehouse itself. Returns the
/// 0x0204 ack packet to send back, mirroring `inventory::handle_item_bag_to_bag`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_warehouse_move(
    db_conn: &mut db::Db,
    character_id: u32,
    account_id: u32,
    character_money: u32,
    active_items: &mut Vec<DbCharacterItem>,
    stored_items: &mut Vec<DbCharacterItem>,
    src_bag: u8,
    src_slot: u8,
    dest_bag: u8,
    dest_slot: u8,
    logger: &Logger,
) -> Result<Option<Vec<u8>>> {
    if src_bag == WAREHOUSE_BAG && !is_valid_warehouse_slot(src_slot) {
        logger.line("Rapture World rejected warehouse move with invalid source slot");
        return Ok(None);
    }
    if dest_bag == WAREHOUSE_BAG && !is_valid_warehouse_slot(dest_slot) {
        logger.line("Rapture World rejected warehouse move with invalid destination slot");
        return Ok(None);
    }

    match (src_bag == WAREHOUSE_BAG, dest_bag == WAREHOUSE_BAG) {
        (true, true) => rearrange_within_warehouse(
            db_conn,
            account_id,
            stored_items,
            src_slot,
            dest_slot,
            logger,
        ),
        (true, false) => withdraw_item(
            db_conn,
            character_id,
            account_id,
            character_money,
            active_items,
            stored_items,
            src_slot,
            dest_bag,
            dest_slot,
            logger,
        ),
        (false, true) => deposit_item(
            db_conn,
            character_id,
            account_id,
            active_items,
            stored_items,
            src_bag,
            src_slot,
            dest_slot,
            logger,
        ),
        (false, false) => {
            // Neither side is the warehouse -- not this module's concern.
            Ok(None)
        }
    }
}

fn rearrange_within_warehouse(
    db_conn: &mut db::Db,
    account_id: u32,
    stored_items: &mut Vec<DbCharacterItem>,
    src_slot: u8,
    dest_slot: u8,
    logger: &Logger,
) -> Result<Option<Vec<u8>>> {
    let Some(src_index) = find_stored_item_index(stored_items, src_slot) else {
        logger.line("Rapture World rejected warehouse rearrange because source slot is empty");
        let dest_item = find_stored_item_index(stored_items, dest_slot).map(|i| &stored_items[i]);
        return build_item_bag_to_bag_result(
            WAREHOUSE_BAG,
            src_slot,
            None,
            WAREHOUSE_BAG,
            dest_slot,
            dest_item,
        )
        .map(Some);
    };

    if src_slot == dest_slot {
        let item = &stored_items[src_index];
        return build_item_bag_to_bag_result(
            WAREHOUSE_BAG,
            src_slot,
            Some(item),
            WAREHOUSE_BAG,
            dest_slot,
            Some(item),
        )
        .map(Some);
    }

    let dest_index = find_stored_item_index(stored_items, dest_slot);

    if let Some(dest_index) = dest_index {
        let src_item = stored_items[src_index].clone();
        let dest_item = stored_items[dest_index].clone();

        if items_can_stack(&src_item, &dest_item) {
            if let Some(definition) =
                db::get_item_definition(db_conn, src_item.item_type, src_item.type_id)?
            {
                let max_stack = effective_stack_limit(&definition);
                if max_stack > 1 && dest_item.count < max_stack {
                    let free_space = max_stack.saturating_sub(dest_item.count);
                    let moved_count = src_item.count.min(free_space);
                    let remaining_src_count = src_item.count.saturating_sub(moved_count);
                    let new_dest_count = dest_item.count.saturating_add(moved_count);

                    db::set_stored_item_count(db_conn, account_id, dest_item.row_id, new_dest_count)?;
                    if remaining_src_count == 0 {
                        db::delete_stored_item(db_conn, account_id, src_item.row_id)?;
                        stored_items.remove(src_index);
                    } else {
                        db::set_stored_item_count(
                            db_conn,
                            account_id,
                            src_item.row_id,
                            remaining_src_count,
                        )?;
                        if let Some(item) = stored_items.get_mut(src_index) {
                            item.count = remaining_src_count;
                        }
                    }
                    if let Some(item) = stored_items
                        .iter_mut()
                        .find(|item| item.row_id == dest_item.row_id)
                    {
                        item.count = new_dest_count;
                    }

                    let src_after = stored_items
                        .iter()
                        .find(|item| item.row_id == src_item.row_id)
                        .filter(|item| item.count > 0);
                    let dest_after = stored_items.iter().find(|item| item.row_id == dest_item.row_id);
                    return build_item_bag_to_bag_result(
                        WAREHOUSE_BAG,
                        src_slot,
                        src_after,
                        WAREHOUSE_BAG,
                        dest_slot,
                        dest_after,
                    )
                    .map(Some);
                }
            }
        }

        db::swap_stored_items(
            db_conn,
            account_id,
            src_item.row_id,
            dest_slot,
            dest_item.row_id,
            src_slot,
        )?;
        stored_items[src_index].slot = dest_slot;
        stored_items[dest_index].slot = src_slot;

        build_item_bag_to_bag_result(
            WAREHOUSE_BAG,
            src_slot,
            Some(&stored_items[dest_index]),
            WAREHOUSE_BAG,
            dest_slot,
            Some(&stored_items[src_index]),
        )
        .map(Some)
    } else {
        let src_item = stored_items[src_index].clone();
        db::move_stored_item(db_conn, account_id, src_item.row_id, dest_slot)?;
        stored_items[src_index].slot = dest_slot;

        build_item_bag_to_bag_result(
            WAREHOUSE_BAG,
            src_slot,
            None,
            WAREHOUSE_BAG,
            dest_slot,
            Some(&stored_items[src_index]),
        )
        .map(Some)
    }
}

#[allow(clippy::too_many_arguments)]
fn deposit_item(
    db_conn: &mut db::Db,
    character_id: u32,
    account_id: u32,
    active_items: &mut Vec<DbCharacterItem>,
    stored_items: &mut Vec<DbCharacterItem>,
    src_bag: u8,
    src_slot: u8,
    dest_slot: u8,
    logger: &Logger,
) -> Result<Option<Vec<u8>>> {
    let Some(src_index) = active_items
        .iter()
        .position(|item| item.bag == src_bag && item.slot == src_slot)
    else {
        logger.line("Rapture World rejected warehouse deposit because source slot is empty");
        let dest_item = find_stored_item_index(stored_items, dest_slot).map(|i| &stored_items[i]);
        return build_item_bag_to_bag_result(
            src_bag,
            src_slot,
            None,
            WAREHOUSE_BAG,
            dest_slot,
            dest_item,
        )
        .map(Some);
    };

    let src_item = active_items[src_index].clone();
    let dest_index = find_stored_item_index(stored_items, dest_slot);

    if let Some(dest_index) = dest_index {
        let dest_item = stored_items[dest_index].clone();
        if items_can_stack(&src_item, &dest_item) {
            if let Some(definition) =
                db::get_item_definition(db_conn, src_item.item_type, src_item.type_id)?
            {
                let max_stack = effective_stack_limit(&definition);
                if max_stack > 1 && dest_item.count < max_stack {
                    let free_space = max_stack.saturating_sub(dest_item.count);
                    let moved_count = src_item.count.min(free_space);
                    let remaining_src_count = src_item.count.saturating_sub(moved_count);
                    let new_dest_count = dest_item.count.saturating_add(moved_count);

                    db::set_stored_item_count(db_conn, account_id, dest_item.row_id, new_dest_count)?;
                    if remaining_src_count == 0 {
                        db::delete_character_item(db_conn, character_id, src_item.row_id)?;
                        active_items.remove(src_index);
                    } else {
                        db::set_character_item_count(
                            db_conn,
                            character_id,
                            src_item.row_id,
                            remaining_src_count,
                        )?;
                        if let Some(item) = active_items.get_mut(src_index) {
                            item.count = remaining_src_count;
                        }
                    }
                    if let Some(item) = stored_items
                        .iter_mut()
                        .find(|item| item.row_id == dest_item.row_id)
                    {
                        item.count = new_dest_count;
                    }

                    let src_after = active_items
                        .iter()
                        .find(|item| item.row_id == src_item.row_id)
                        .filter(|item| item.count > 0);
                    let dest_after = stored_items.iter().find(|item| item.row_id == dest_item.row_id);
                    return build_item_bag_to_bag_result(
                        src_bag,
                        src_slot,
                        src_after,
                        WAREHOUSE_BAG,
                        dest_slot,
                        dest_after,
                    )
                    .map(Some);
                }
            }
        }
        // Destination occupied by a non-stackable item: reject the move.
        logger.line("Rapture World rejected warehouse deposit into an occupied slot");
        return build_item_bag_to_bag_result(
            src_bag,
            src_slot,
            Some(&src_item),
            WAREHOUSE_BAG,
            dest_slot,
            Some(&dest_item),
        )
        .map(Some);
    }

    db::delete_character_item(db_conn, character_id, src_item.row_id)?;
    active_items.remove(src_index);
    let stored = db::create_stored_item_from_template(db_conn, account_id, &src_item, dest_slot, src_item.count)?;
    stored_items.push(stored.clone());

    logger.line(format!(
        "Rapture World deposited item row_id={} into warehouse slot={}",
        src_item.row_id, dest_slot
    ));

    build_item_bag_to_bag_result(src_bag, src_slot, None, WAREHOUSE_BAG, dest_slot, Some(&stored)).map(Some)
}

/// Withdraw responses (`GameItemBankToBagOutgoing`) carry a trailing gold
/// balance the normal bag-to-bag ack doesn't -- confirmed by a real capture
/// being 4 bytes longer than the equivalent deposit response. Leaving it
/// off entirely (rather than wrong) made the client render gold as 0 until
/// the next relog re-synced it via 0x0213. Retail charges a withdrawal
/// commission here; we don't want one, so this always echoes the
/// character's unchanged current money.
fn append_money_field(mut packet: Vec<u8>, money: u32) -> Result<Vec<u8>> {
    packet.extend_from_slice(&money.to_le_bytes());
    if packet.len() > u16::MAX as usize {
        anyhow::bail!("packet is too large");
    }
    let length = (packet.len() as u16).to_le_bytes();
    packet[0] = length[0];
    packet[1] = length[1];
    Ok(packet)
}

#[allow(clippy::too_many_arguments)]
fn withdraw_item(
    db_conn: &mut db::Db,
    character_id: u32,
    account_id: u32,
    character_money: u32,
    active_items: &mut Vec<DbCharacterItem>,
    stored_items: &mut Vec<DbCharacterItem>,
    src_slot: u8,
    dest_bag: u8,
    dest_slot: u8,
    logger: &Logger,
) -> Result<Option<Vec<u8>>> {
    let Some(src_index) = find_stored_item_index(stored_items, src_slot) else {
        logger.line("Rapture World rejected warehouse withdraw because source slot is empty");
        let dest_item = active_items
            .iter()
            .find(|item| item.bag == dest_bag && item.slot == dest_slot);
        let packet = build_item_bag_to_bag_result(
            WAREHOUSE_BAG,
            src_slot,
            None,
            dest_bag,
            dest_slot,
            dest_item,
        )?;
        return append_money_field(packet, character_money).map(Some);
    };

    let src_item = stored_items[src_index].clone();
    let dest_index = active_items
        .iter()
        .position(|item| item.bag == dest_bag && item.slot == dest_slot);

    if let Some(dest_index) = dest_index {
        let dest_item = active_items[dest_index].clone();
        if items_can_stack(&src_item, &dest_item) {
            if let Some(definition) =
                db::get_item_definition(db_conn, src_item.item_type, src_item.type_id)?
            {
                let max_stack = effective_stack_limit(&definition);
                if max_stack > 1 && dest_item.count < max_stack {
                    let free_space = max_stack.saturating_sub(dest_item.count);
                    let moved_count = src_item.count.min(free_space);
                    let remaining_src_count = src_item.count.saturating_sub(moved_count);
                    let new_dest_count = dest_item.count.saturating_add(moved_count);

                    db::set_character_item_count(db_conn, character_id, dest_item.row_id, new_dest_count)?;
                    if remaining_src_count == 0 {
                        db::delete_stored_item(db_conn, account_id, src_item.row_id)?;
                        stored_items.remove(src_index);
                    } else {
                        db::set_stored_item_count(
                            db_conn,
                            account_id,
                            src_item.row_id,
                            remaining_src_count,
                        )?;
                        if let Some(item) = stored_items.get_mut(src_index) {
                            item.count = remaining_src_count;
                        }
                    }
                    if let Some(item) = active_items
                        .iter_mut()
                        .find(|item| item.row_id == dest_item.row_id)
                    {
                        item.count = new_dest_count;
                    }

                    let src_after = stored_items
                        .iter()
                        .find(|item| item.row_id == src_item.row_id)
                        .filter(|item| item.count > 0);
                    let dest_after = active_items.iter().find(|item| item.row_id == dest_item.row_id);
                    let packet = build_item_bag_to_bag_result(
                        WAREHOUSE_BAG,
                        src_slot,
                        src_after,
                        dest_bag,
                        dest_slot,
                        dest_after,
                    )?;
                    return append_money_field(packet, character_money).map(Some);
                }
            }
        }
        // Destination occupied by a non-stackable item: reject the move.
        logger.line("Rapture World rejected warehouse withdraw into an occupied slot");
        let packet = build_item_bag_to_bag_result(
            WAREHOUSE_BAG,
            src_slot,
            Some(&src_item),
            dest_bag,
            dest_slot,
            Some(&dest_item),
        )?;
        return append_money_field(packet, character_money).map(Some);
    }

    db::delete_stored_item(db_conn, account_id, src_item.row_id)?;
    stored_items.remove(src_index);
    let withdrawn = db::create_character_item_from_template(
        db_conn,
        character_id,
        &src_item,
        dest_bag,
        dest_slot,
        src_item.count,
    )?;
    active_items.push(withdrawn.clone());

    logger.line(format!(
        "Rapture World withdrew item row_id={} from warehouse slot={} to ({dest_bag},{dest_slot})",
        src_item.row_id, src_slot
    ));

    let packet =
        build_item_bag_to_bag_result(WAREHOUSE_BAG, src_slot, None, dest_bag, dest_slot, Some(&withdrawn))?;
    append_money_field(packet, character_money).map(Some)
}
