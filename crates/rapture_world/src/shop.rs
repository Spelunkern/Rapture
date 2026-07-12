//! Selling items to an NPC vendor. Buying is not implemented -- there is no
//! source of "which NPC sells what" data anywhere in this project (no such
//! table in `PS_GameDefs`, and the Shaiya-Aries reference only shows this
//! catalog as an in-memory client/server struct populated from data this
//! repo doesn't contain). Selling doesn't need that catalog: any NPC can
//! buy back any sellable item at its `PS_GameDefs.dbo.Items.Sell` price, so
//! it's implemented standalone.
//!
//! Wire format confirmed against a real capture (selling a type=14/typeId=1
//! item, bag=1 slot=6, count=1):
//! - Incoming `TPC_SHOP_SELL` (0x0703): `bag:u8, slot:u8, count:u8` -- only
//!   3 payload bytes (`SEND>>03 07 01 06 01`). The Shaiya-Aries header
//!   documents a trailing `npcId:u32`, but the real client doesn't send it;
//!   the target vendor must be tracked server-side some other way (not
//!   validated here at all yet -- any NPC is accepted as a sell target).
//! - Outgoing `TP_SHOP_SELL` (0x0703): `result:u8, bag:u8, slot:u8, type:u8,
//!   typeId:u8, count:u8, money:u32` (`RECV>>03 07 00 01 06 0E 01 00 6A E4
//!   F5 05` for the same sale). `count` here is the slot's REMAINING count
//!   after the sale (0 in that capture, since the whole stack of 1 was
//!   sold), not the quantity transacted. `result` has no documented enum --
//!   0 is assumed success, 1 assumed failure, matching the convention
//!   already used elsewhere in this codebase (e.g. create-character's
//!   ok/fail byte).

use anyhow::Result;
use rapture_common::db::{self, DbCharacterItem};
use rapture_common::packet::{Opcode, PacketWriter};

use crate::session::Logger;

const SELL_RESULT_OK: u8 = 0;
const SELL_RESULT_FAIL: u8 = 1;

fn build_shop_sell_result(
    result: u8,
    bag: u8,
    slot: u8,
    item_type: u8,
    type_id: u8,
    remaining_count: u8,
    money: u32,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ShopSell);
    packet.write_u8(result);
    packet.write_u8(bag);
    packet.write_u8(slot);
    packet.write_u8(item_type);
    packet.write_u8(type_id);
    packet.write_u8(remaining_count);
    packet.write_u32(money);
    packet.into_bytes()
}

/// Handles 0x0703 `TPC_SHOP_SELL`: removes (or reduces) the item at
/// `bag`/`slot` and credits the character `sell_price * count` gold, per
/// `PS_GameDefs.dbo.Items.Sell`. Returns the 0x0703 ack to send back plus
/// the character's resulting money total (unchanged on failure).
pub(crate) fn handle_shop_sell(
    db_conn: &mut db::Db,
    character_id: u32,
    current_money: u32,
    active_items: &mut Vec<DbCharacterItem>,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<(Vec<u8>, u32)>> {
    if payload.len() < 5 {
        logger.line(format!(
            "Rapture World shop sell packet too short len={}",
            payload.len()
        ));
        return Ok(None);
    }

    let bag = payload[2];
    let slot = payload[3];
    let requested_count = payload[4].max(1);

    let Some(index) = active_items
        .iter()
        .position(|item| item.bag == bag && item.slot == slot)
    else {
        logger.line(format!(
            "Rapture World rejected shop sell: empty slot bag={bag} slot={slot}"
        ));
        let packet = build_shop_sell_result(SELL_RESULT_FAIL, bag, slot, 0, 0, 0, current_money)?;
        return Ok(Some((packet, current_money)));
    };

    let item = active_items[index].clone();
    let Some(definition) = db::get_item_definition(db_conn, item.item_type, item.type_id)? else {
        logger.line(format!(
            "Rapture World rejected shop sell: no item definition for type={} type_id={}",
            item.item_type, item.type_id
        ));
        let packet = build_shop_sell_result(SELL_RESULT_FAIL, bag, slot, 0, 0, 0, current_money)?;
        return Ok(Some((packet, current_money)));
    };

    let sold_count = requested_count.min(item.count);
    let remaining_count = item.count.saturating_sub(sold_count);
    let proceeds = definition.sell_price.saturating_mul(u32::from(sold_count));

    if remaining_count == 0 {
        db::delete_character_item(db_conn, character_id, item.row_id)?;
        active_items.remove(index);
    } else {
        db::set_character_item_count(db_conn, character_id, item.row_id, remaining_count)?;
        if let Some(item) = active_items.get_mut(index) {
            item.count = remaining_count;
        }
    }

    let new_money = db::add_character_money(db_conn, character_id, proceeds)?;

    logger.line(format!(
        "Rapture World sold item row_id={} type={} type_id={} count={sold_count} proceeds={proceeds}",
        item.row_id, item.item_type, item.type_id
    ));

    let packet = build_shop_sell_result(
        SELL_RESULT_OK,
        bag,
        slot,
        item.item_type,
        item.type_id,
        remaining_count,
        new_money,
    )?;
    Ok(Some((packet, new_money)))
}
