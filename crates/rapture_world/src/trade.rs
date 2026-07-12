//! Player-to-player item/gold trade.
//!
//! Protocol reference: Aries's real opcodes are 0x0A01-0x0A0A (TP_EXC_*,
//! sdev/include/shaiya/CExchange.h + sdev/src/packet_exchange.cpp), but that
//! reference repo's incoming-packet headers are incomplete (it hooks the
//! original retail binary rather than reimplementing it, so several
//! incoming structs are undocumented even though the detour comments prove
//! the opcodes are real, e.g. "PacketExchange case 0xA06"/"case 0xA07").
//! Gaps were filled from Imgeneus's own Trade system (different client
//! version, architecture-only reference, but its field layouts line up with
//! Aries's documented structs almost exactly):
//!   0x0A01 Request       -- u32 targetId (bidirectional: also used to relay
//!                            the request to the target, and reused by the
//!                            target to answer by targeting the requester)
//!   0x0A02 Answer         -- incoming: u8 accepted (0/1); outgoing: u8
//!                            result (0=Accepted,1=Rejected)
//!   0x0A03 Start          -- outgoing only, sent to BOTH sides right after
//!                            0x0A02 Accepted: u32 partnerId + u8 status(0).
//!                            THIS is what actually opens the trade window
//!                            on the real client -- confirmed by real packet
//!                            capture (2026-07-08): sending 0x0A02 Accepted
//!                            alone, as this code used to do, never opened
//!                            it. Not documented in either reference repo.
//!   0x0A04 End            -- outgoing only, u8 status(0=success observed;
//!                            1=used for cancel/abort, NOT yet confirmed
//!                            against a real capture). Closes the trade
//!                            window; sent at the end of a successful trade
//!                            (after the final inventory/money resync) AND
//!                            on every full cancel (see send_cancel) --
//!                            without it, only whoever clicked
//!                            cancel/close ever saw their window actually
//!                            close, since their own client closes it
//!                            locally on click regardless of the server.
//!   0x0A05 Ready          -- incoming u8 has THREE distinct values (not
//!                            just a bool): 0=ready/lock, 1=un-ready (soft
//!                            toggle-off, keeps the session so it can be
//!                            re-marked ready without restarting the whole
//!                            trade), 2=close the window (a full abort --
//!                            confirmed by real capture: closing an
//!                            already-open trade window sends exactly
//!                            this). This project originally treated any
//!                            nonzero incoming value as a full abort, which
//!                            was wrong: it meant un-checking "ready" could
//!                            never be re-checked without restarting from
//!                            scratch. Outgoing: kind 1 = sender, 2 =
//!                            target, 3 = both (a full cancel/reset, u8
//!                            true); kind 1/2 + true is this project's own
//!                            extrapolation for a per-side soft un-ready
//!                            notification (NOT directly captured -- only
//!                            kind=X+false "locked" and kind=3+true "full
//!                            reset" have been observed).
//!   0x0A06 Add            -- incoming: bag,slot,count,destSlot (u8 each);
//!                            outgoing (self ack): identical shape. Real
//!                            capture shows every add/remove/money-set is
//!                            ALSO followed by a broadcast to BOTH sides of
//!                            0x0A05 (kind=3,canceled=1) + 0x0A0A
//!                            (kind=1,confirmed=0) + 0x0A0A
//!                            (kind=2,confirmed=0) -- i.e. exactly
//!                            send_cancel's packet shape, but reused here
//!                            purely as an "offer changed, both ready/
//!                            confirm indicators reset" signal, NOT a full
//!                            abort (the session stays alive). This project
//!                            used to only track that reset internally
//!                            without telling the clients; see
//!                            broadcast_unready.
//!   0x0A07 Remove         -- incoming: destSlot (u8); no dedicated outgoing
//!                            ack exists in the reference -- modeled as an
//!                            empty 0x0A09 AddDest to both sides
//!   0x0A08 Add money      -- incoming: u32 money (not additive -- sets the
//!                            offer to this amount, matching Imgeneus's
//!                            TryAddMoney). Outgoing: kind(u8, 1=sender/
//!                            2=target) + u32 money -- this project
//!                            originally sent a bare u32 with no kind byte,
//!                            which real capture (2026-07-09) showed is
//!                            wrong (5 bytes on the wire, not 4). Gold is
//!                            entirely optional and independent of items --
//!                            a trade offer defaults to 0 and can carry
//!                            money alone, items alone, or both.
//!   0x0A09 Add dest       -- outgoing only, broadcast to the OTHER side:
//!                            destSlot,type,typeId,count,quality,gems(6),
//!                            craftName(21)
//!   0x0A0A Confirm        -- u8 confirmed (0/1). Unlike Ready, there's no
//!                            evidence Confirm has a distinct "full abort"
//!                            value -- confirmed=false is treated as a soft
//!                            un-confirm (session stays alive, matching the
//!                            fix to Ready above). The original assumption
//!                            here (confirmed=false = full abort, mirroring
//!                            Imgeneus's handler_0xA0A) had the same bug as
//!                            Ready: un-checking confirm could never be
//!                            re-checked without restarting the trade.
//!
//! State machine (matches both CExchange's status/ready/confirmed fields
//! and Imgeneus's IsDecided_1/2 + IsConfirmed_1/2): request -> 0x0A03 start
//! (opens the window) -> both add items/money freely (any add/remove
//! broadcasts an "offer changed" reset to both sides, unreadying/
//! unconfirming both) -> both signal ready (0x0A05) AND confirm (0x0A0A),
//! in EITHER order (real capture showed confirm arriving before ready) ->
//! once both flags are set on both sides, DB transfer -> 0x0A04 end to both
//! -> both sides reload their inventory/money from the DB.
//!
//! Items are never actually moved out of an inventory until the trade
//! fully completes -- the offer just references (row_id, count) still
//! sitting in the owner's own inventory, matching Imgeneus's TradeItems
//! dictionary-of-references model. This means a slot/count that becomes
//! invalid between offering and confirming (used, dropped, sold elsewhere)
//! is caught by re-validating against a fresh DB read at finalize time
//! rather than trusting the cached offer.

use anyhow::Result;
use rapture_common::db::{self, DbCharacterItem};
use rapture_common::packet::{Opcode, PacketWriter};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::chat::{ConnectedPlayer, find_player_by_id};
use crate::inventory::{build_remove_item, give_item_to_inventory, is_valid_item_position, would_fit};
use crate::session::Logger;
use crate::world_entities::lock_or_recover;

/// Trade window has 8 slots, matching Aries's `Array<ExcItem, 8>`.
pub(crate) const TRADE_SLOTS: u8 = 8;

#[derive(Clone, Debug)]
struct OfferedItem {
    row_id: u32,
    item_type: u8,
    type_id: u8,
    quality: u16,
    gems: [u8; 6],
    craft_name: String,
    dyed: bool,
    count: u8,
}

#[derive(Clone, Debug, Default)]
struct TradeOffer {
    items: [Option<OfferedItem>; 8],
    money: u32,
    ready: bool,
    confirmed: bool,
}

impl TradeOffer {
    fn unready(&mut self) {
        self.ready = false;
        self.confirmed = false;
    }
}

struct TradeSession {
    participant_a: u32,
    participant_b: u32,
    offer_a: TradeOffer,
    offer_b: TradeOffer,
}

impl TradeSession {
    fn offer_mut(&mut self, character_id: u32) -> Option<&mut TradeOffer> {
        if character_id == self.participant_a {
            Some(&mut self.offer_a)
        } else if character_id == self.participant_b {
            Some(&mut self.offer_b)
        } else {
            None
        }
    }

    fn offer(&self, character_id: u32) -> Option<&TradeOffer> {
        if character_id == self.participant_a {
            Some(&self.offer_a)
        } else if character_id == self.participant_b {
            Some(&self.offer_b)
        } else {
            None
        }
    }

    fn partner_of(&self, character_id: u32) -> Option<u32> {
        if character_id == self.participant_a {
            Some(self.participant_b)
        } else if character_id == self.participant_b {
            Some(self.participant_a)
        } else {
            None
        }
    }

    fn both_ready(&self) -> bool {
        self.offer_a.ready && self.offer_b.ready
    }

    fn both_confirmed(&self) -> bool {
        self.offer_a.confirmed && self.offer_b.confirmed
    }
}

/// character_id -> pending trade request TARGET they sent one to (used to
/// match up a reciprocal 0x0A01 from the target as acceptance, mirroring
/// Aries's ExchangeStatus::RequestSent/RequestRecv pair).
fn pending_requests() -> &'static Mutex<HashMap<u32, u32>> {
    static REQUESTS: OnceLock<Mutex<HashMap<u32, u32>>> = OnceLock::new();
    REQUESTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// character_id -> the shared session, for BOTH participants (same Arc).
fn active_sessions() -> &'static Mutex<HashMap<u32, Arc<Mutex<TradeSession>>>> {
    static SESSIONS: OnceLock<Mutex<HashMap<u32, Arc<Mutex<TradeSession>>>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn session_for(character_id: u32) -> Option<Arc<Mutex<TradeSession>>> {
    lock_or_recover(active_sessions())
        .get(&character_id)
        .cloned()
}

fn remove_session(character_id: u32, partner_id: u32) {
    let mut sessions = lock_or_recover(active_sessions());
    sessions.remove(&character_id);
    sessions.remove(&partner_id);
}

/// Called when a connection with an active trade or pending request
/// disconnects (ChatPresenceGuard::drop). Aborts anything in progress and
/// notifies the other side, if any.
pub(crate) fn cancel_for_disconnect(character_id: u32, logger: &Logger) {
    lock_or_recover(pending_requests()).remove(&character_id);
    lock_or_recover(pending_requests()).retain(|_, target| *target != character_id);

    let Some(session) = session_for(character_id) else {
        return;
    };
    let partner_id = {
        let locked = lock_or_recover(&session);
        locked.partner_of(character_id)
    };
    let Some(partner_id) = partner_id else { return };
    remove_session(character_id, partner_id);
    if let Some(partner) = find_player_by_id(partner_id) {
        let _ = send_cancel(&partner, logger);
    }
}

fn build_request(target_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ExchangeRequest);
    packet.write_u32(target_id);
    packet.into_bytes()
}

fn build_answer(accepted: bool) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ExchangeAnswer);
    packet.write_u8(u8::from(!accepted));
    packet.into_bytes()
}

/// `partner_id` is the OTHER side's character id -- sent to each
/// participant right after 0x0A02 Accepted. This is what actually opens
/// the trade window on the real client (see module doc).
fn build_start(partner_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ExchangeStart);
    packet.write_u32(partner_id);
    packet.write_u8(0);
    packet.into_bytes()
}

/// `status`: 0 = success (the only value captured so far; a cancel/reject
/// status has not been observed yet).
fn build_end(status: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ExchangeEnd);
    packet.write_u8(status);
    packet.into_bytes()
}

/// `kind`: 1 = sender, 2 = target, 3 = cancel (GameExchangeReadyType).
fn build_ready(kind: u8, canceled: bool) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ExchangeReady);
    packet.write_u8(kind);
    packet.write_u8(u8::from(canceled));
    packet.into_bytes()
}

fn build_add_ack(bag: u8, slot: u8, count: u8, dest_slot: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ExchangeAdd);
    packet.write_u8(bag);
    packet.write_u8(slot);
    packet.write_u8(count);
    packet.write_u8(dest_slot);
    packet.into_bytes()
}

fn build_add_dest(dest_slot: u8, item: Option<&OfferedItem>) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ExchangeAddDest);
    packet.write_u8(dest_slot);
    match item {
        Some(item) => {
            packet.write_u8(item.item_type);
            packet.write_u8(item.type_id);
            packet.write_u8(item.count);
            packet.write_u16(item.quality);
            for gem in item.gems {
                packet.write_u8(gem);
            }
            packet.write_fixed_string(&item.craft_name, 21)?;
        }
        None => {
            packet.write_u8(0);
            packet.write_u8(0);
            packet.write_u8(0);
            packet.write_u16(0);
            for _ in 0..6 {
                packet.write_u8(0);
            }
            packet.write_fixed_string("", 21)?;
        }
    }
    packet.into_bytes()
}

/// `kind`: 1 = sender, 2 = target -- outgoing AddMoney carries this same
/// per-side identifier as Ready/Confirm (confirmed by real capture,
/// 2026-07-09: the wire packet is 5 bytes, kind+u32, not just a bare u32 as
/// this project originally sent).
fn build_add_money(kind: u8, money: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ExchangeAddMoney);
    packet.write_u8(kind);
    packet.write_u32(money);
    packet.into_bytes()
}

/// `kind`: 1 = sender, 2 = target, 3 = cancel (GameExchangeConfirmType).
fn build_confirm(kind: u8, confirmed: bool) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ExchangeConfirm);
    packet.write_u8(kind);
    packet.write_u8(u8::from(confirmed));
    packet.into_bytes()
}

/// Fully aborts the trade for one side: resets the ready/confirm
/// indicators (same 3-packet shape as an "offer changed" broadcast) AND
/// closes the window with 0x0A04 End.
///
/// The End status byte used here (1) is NOT confirmed against a real
/// capture -- only the success value (0) has been observed so far -- but
/// sending End at all is required: without it, only whichever side
/// actually clicked cancel ever saw their window close (their own client
/// closes it locally on click, independent of the server), while the OTHER
/// side's window used to just sit open forever, since the old cancel
/// packet set never included anything that told the client to close.
fn send_cancel(player: &ConnectedPlayer, logger: &Logger) -> Result<()> {
    player.send_packet(&build_ready(3, true)?, "0x0A05 exchange cancel", logger)?;
    player.send_packet(
        &build_confirm(1, false)?,
        "0x0A0A exchange cancel confirm (self)",
        logger,
    )?;
    player.send_packet(
        &build_confirm(2, false)?,
        "0x0A0A exchange cancel confirm (partner)",
        logger,
    )?;
    player.send_packet(&build_end(1)?, "0x0A04 exchange end (canceled)", logger)?;
    Ok(())
}

/// Broadcasts the "offer changed" reset signal to BOTH participants:
/// exactly the same 3 packets as `send_cancel` (0x0A05 kind=3 canceled=true
/// + 0x0A0A kind=1 confirmed=false + 0x0A0A kind=2 confirmed=false), but
/// unlike a real cancel this does NOT touch the session -- confirmed by
/// real packet capture (2026-07-08) to be sent to both sides after every
/// add/remove/money-set, resetting both clients' ready/confirm indicators
/// without closing the window. Call this BEFORE the add/remove/money's own
/// ack packets, matching the observed order.
fn broadcast_unready(player: &ConnectedPlayer, partner: &ConnectedPlayer, logger: &Logger) -> Result<()> {
    for recipient in [player, partner] {
        recipient.send_packet(
            &build_ready(3, true)?,
            "0x0A05 exchange offer changed (unready)",
            logger,
        )?;
        recipient.send_packet(
            &build_confirm(1, false)?,
            "0x0A0A exchange offer changed (unconfirm 1)",
            logger,
        )?;
        recipient.send_packet(
            &build_confirm(2, false)?,
            "0x0A0A exchange offer changed (unconfirm 2)",
            logger,
        )?;
    }
    Ok(())
}

/// Handles an incoming 0x0A01 ExchangeRequest from `requester` targeting
/// `target_id`. Either starts a new pending request, or -- if `target_id`
/// already has a pending request aimed back at `requester` -- completes the
/// handshake and opens the trade window for both. Sends whatever packets
/// are needed directly; nothing to return to the caller.
pub(crate) fn handle_request(
    requester: &Arc<ConnectedPlayer>,
    target_id: u32,
    logger: &Logger,
) -> Result<()> {
    if requester.character_id == target_id {
        return Ok(());
    }
    if session_for(requester.character_id).is_some() {
        logger.line(format!(
            "Rapture World rejected exchange request character_id={} already trading",
            requester.character_id
        ));
        return Ok(());
    }
    let Some(target) = find_player_by_id(target_id) else {
        logger.line(format!(
            "Rapture World rejected exchange request target_id={target_id} not found"
        ));
        return Ok(());
    };
    if target.map_id() != requester.map_id() {
        return Ok(());
    }

    let reciprocal = lock_or_recover(pending_requests())
        .get(&target_id)
        .copied()
        == Some(requester.character_id);

    if reciprocal {
        lock_or_recover(pending_requests()).remove(&target_id);
        lock_or_recover(pending_requests()).remove(&requester.character_id);
        start_trade(requester, &target, logger)?;
    } else {
        lock_or_recover(pending_requests()).insert(requester.character_id, target_id);
        target.send_packet(
            &build_request(requester.character_id)?,
            "0x0A01 exchange request",
            logger,
        )?;
    }
    Ok(())
}

/// Handles an incoming 0x0A02 ExchangeAnswer (only ever sent by whoever is
/// on the receiving end of a still-pending request).
pub(crate) fn handle_answer(
    responder: &Arc<ConnectedPlayer>,
    accepted: bool,
    logger: &Logger,
) -> Result<()> {
    let requester_id = lock_or_recover(pending_requests())
        .iter()
        .find(|(_, target)| **target == responder.character_id)
        .map(|(requester, _)| *requester);
    let Some(requester_id) = requester_id else {
        return Ok(());
    };
    lock_or_recover(pending_requests()).remove(&requester_id);

    if !accepted {
        if let Some(requester) = find_player_by_id(requester_id) {
            requester.send_packet(&build_answer(false)?, "0x0A02 exchange rejected", logger)?;
        }
        return Ok(());
    }

    let Some(requester) = find_player_by_id(requester_id) else {
        return Ok(());
    };
    start_trade(&requester, responder, logger)
}

/// Creates the shared session for `a`/`b` and sends whatever both clients
/// need to actually open the trade window.
///
/// RESOLVED (was "STILL UNCONFIRMED" -- see RAPTURE_PROGRESS_LOG.txt):
/// sending 0x0A02 Accepted to both sides doesn't open the window on its
/// own (confirmed empirically). A follow-up experiment that also resent
/// 0x0A01 (bearing the partner's id) to both sides as a guessed "start"
/// signal made things actively worse -- it caused the client to loop,
/// re-firing spurious 0x0A01/0x0A02 packets on its own. A real packet
/// capture (2026-07-08) found the actual missing piece: a dedicated 0x0A03
/// Start packet (u32 partnerId + u8 status), sent to both sides right
/// after 0x0A02 Accepted, is what opens the window.
fn start_trade(a: &Arc<ConnectedPlayer>, b: &Arc<ConnectedPlayer>, logger: &Logger) -> Result<()> {
    let session = Arc::new(Mutex::new(TradeSession {
        participant_a: a.character_id,
        participant_b: b.character_id,
        offer_a: TradeOffer::default(),
        offer_b: TradeOffer::default(),
    }));
    let mut sessions = lock_or_recover(active_sessions());
    sessions.insert(a.character_id, Arc::clone(&session));
    sessions.insert(b.character_id, session);
    drop(sessions);

    a.send_packet(&build_answer(true)?, "0x0A02 exchange accepted", logger)?;
    b.send_packet(&build_answer(true)?, "0x0A02 exchange accepted", logger)?;
    a.send_packet(&build_start(b.character_id)?, "0x0A03 exchange start", logger)?;
    b.send_packet(&build_start(a.character_id)?, "0x0A03 exchange start", logger)?;
    logger.line(format!(
        "Rapture World exchange started between character_id={} and character_id={}",
        a.character_id, b.character_id
    ));
    Ok(())
}

/// Handles 0x0A06 ExchangeAdd: offers `count` of the item at (bag, slot) in
/// the caller's OWN inventory into trade slot `dest_slot`. `items` is the
/// caller's own live inventory cache (read-only here -- nothing is actually
/// moved until the trade finalizes).
pub(crate) fn handle_add_item(
    player: &Arc<ConnectedPlayer>,
    items: &[DbCharacterItem],
    bag: u8,
    slot: u8,
    count: u8,
    dest_slot: u8,
    logger: &Logger,
) -> Result<()> {
    if dest_slot >= TRADE_SLOTS || bag == 0 || !is_valid_item_position(bag, slot) {
        return Ok(());
    }
    let Some(session) = session_for(player.character_id) else {
        return Ok(());
    };
    let Some(item) = items.iter().find(|item| item.bag == bag && item.slot == slot) else {
        logger.line("Rapture World rejected exchange add: empty source slot");
        return Ok(());
    };
    let offered_count = count.clamp(1, item.count);

    let partner = {
        let mut locked = lock_or_recover(&session);
        let Some(partner_id) = locked.partner_of(player.character_id) else {
            return Ok(());
        };
        locked.offer_a.unready();
        locked.offer_b.unready();
        let Some(offer) = locked.offer_mut(player.character_id) else {
            return Ok(());
        };
        offer.items[dest_slot as usize] = Some(OfferedItem {
            row_id: item.row_id,
            item_type: item.item_type,
            type_id: item.type_id,
            quality: item.quality,
            gems: item.gems,
            craft_name: item.craft_name.clone(),
            dyed: item.dyed,
            count: offered_count,
        });
        find_player_by_id(partner_id)
    };

    if let Some(partner) = &partner {
        broadcast_unready(player, partner, logger)?;
    }
    player.send_packet(
        &build_add_ack(bag, slot, offered_count, dest_slot)?,
        "0x0A06 exchange add (self)",
        logger,
    )?;
    if let Some(partner) = partner {
        let locked = lock_or_recover(&session);
        if let Some(offered) = locked
            .offer(player.character_id)
            .and_then(|offer| offer.items[dest_slot as usize].as_ref())
        {
            partner.send_packet(
                &build_add_dest(dest_slot, Some(offered))?,
                "0x0A09 exchange add dest",
                logger,
            )?;
        }
    }
    Ok(())
}

/// Handles 0x0A07 ExchangeRemove: clears trade slot `dest_slot` from the
/// caller's own offer.
pub(crate) fn handle_remove_item(
    player: &Arc<ConnectedPlayer>,
    dest_slot: u8,
    logger: &Logger,
) -> Result<()> {
    if dest_slot >= TRADE_SLOTS {
        return Ok(());
    }
    let Some(session) = session_for(player.character_id) else {
        return Ok(());
    };
    let partner = {
        let mut locked = lock_or_recover(&session);
        let Some(partner_id) = locked.partner_of(player.character_id) else {
            return Ok(());
        };
        locked.offer_a.unready();
        locked.offer_b.unready();
        let Some(offer) = locked.offer_mut(player.character_id) else {
            return Ok(());
        };
        offer.items[dest_slot as usize] = None;
        find_player_by_id(partner_id)
    };

    if let Some(partner) = &partner {
        broadcast_unready(player, partner, logger)?;
    }
    player.send_packet(
        &build_add_dest(dest_slot, None)?,
        "0x0A09 exchange remove (self)",
        logger,
    )?;
    if let Some(partner) = partner {
        partner.send_packet(
            &build_add_dest(dest_slot, None)?,
            "0x0A09 exchange remove (partner)",
            logger,
        )?;
    }
    Ok(())
}

/// Handles 0x0A08 ExchangeAddMoney: sets (not adds) the caller's offered
/// gold, clamped to their actual current balance.
pub(crate) fn handle_add_money(
    player: &Arc<ConnectedPlayer>,
    current_money: u32,
    requested: u32,
    logger: &Logger,
) -> Result<()> {
    let Some(session) = session_for(player.character_id) else {
        return Ok(());
    };
    let offered = requested.min(current_money);
    let partner = {
        let mut locked = lock_or_recover(&session);
        let Some(partner_id) = locked.partner_of(player.character_id) else {
            return Ok(());
        };
        locked.offer_a.unready();
        locked.offer_b.unready();
        let Some(offer) = locked.offer_mut(player.character_id) else {
            return Ok(());
        };
        offer.money = offered;
        find_player_by_id(partner_id)
    };

    if let Some(partner) = &partner {
        broadcast_unready(player, partner, logger)?;
    }
    player.send_packet(
        &build_add_money(1, offered)?,
        "0x0A08 exchange add money (self)",
        logger,
    )?;
    if let Some(partner) = partner {
        partner.send_packet(
            &build_add_money(2, offered)?,
            "0x0A08 exchange add money (partner)",
            logger,
        )?;
    }
    Ok(())
}

/// What the calling thread needs to do after a completed trade: reload its
/// own inventory/money and push the refresh to its own client. The
/// partner's side is handled via the pending_trade_refresh mailbox instead,
/// since only their own thread may touch their active_items/character.
pub(crate) struct FinalizeOutcome {
    pub(crate) items: Vec<DbCharacterItem>,
    pub(crate) money: u32,
    pub(crate) packets: Vec<Vec<u8>>,
}

/// Handles 0x0A05 ExchangeReady. `raw_value` has three meanings (see module
/// doc): 0 = lock in the caller's current offer (and, if the partner has
/// already both readied and confirmed, finalize the trade); 1 = soft
/// un-ready -- clears just the caller's own ready flag, session stays
/// alive, so it can be re-marked ready without restarting; anything else
/// (2 observed) = a full abort, closing the window for both sides.
///
/// Real packet capture (2026-07-08) showed Ready being sent AFTER Confirm
/// in practice (the opposite of what this project originally assumed), so
/// finalize is triggered here too, symmetrically with `handle_confirm` --
/// whichever of Ready/Confirm completes the last missing flag on either
/// side finalizes, instead of hardcoding one specific order.
pub(crate) fn handle_ready(
    db_conn: &mut db::Db,
    player: &Arc<ConnectedPlayer>,
    raw_value: u8,
    logger: &Logger,
) -> Result<Option<FinalizeOutcome>> {
    let Some(session) = session_for(player.character_id) else {
        return Ok(None);
    };
    let partner_id = {
        let locked = lock_or_recover(&session);
        locked.partner_of(player.character_id)
    };
    let Some(partner_id) = partner_id else {
        return Ok(None);
    };
    let partner = find_player_by_id(partner_id);

    if raw_value >= 2 {
        remove_session(player.character_id, partner_id);
        send_cancel(player, logger)?;
        if let Some(partner) = &partner {
            send_cancel(partner, logger)?;
        }
        return Ok(None);
    }

    if raw_value == 1 {
        {
            let mut locked = lock_or_recover(&session);
            if let Some(offer) = locked.offer_mut(player.character_id) {
                offer.ready = false;
            }
        }
        player.send_packet(&build_ready(1, true)?, "0x0A05 exchange unready (self)", logger)?;
        if let Some(partner) = &partner {
            partner.send_packet(
                &build_ready(2, true)?,
                "0x0A05 exchange unready (partner)",
                logger,
            )?;
        }
        return Ok(None);
    }

    let both_done = {
        let mut locked = lock_or_recover(&session);
        if let Some(offer) = locked.offer_mut(player.character_id) {
            offer.ready = true;
        }
        locked.both_ready() && locked.both_confirmed()
    };
    player.send_packet(&build_ready(1, false)?, "0x0A05 exchange ready (self)", logger)?;
    if let Some(partner) = &partner {
        partner.send_packet(
            &build_ready(2, false)?,
            "0x0A05 exchange ready (partner)",
            logger,
        )?;
    }

    if !both_done {
        return Ok(None);
    }
    let Some(partner) = partner else {
        return Ok(None);
    };
    finalize_and_notify(db_conn, &session, player, partner_id, &partner, logger)
}

/// Handles 0x0A0A ExchangeConfirm. `confirmed = false` is a soft
/// un-confirm -- clears just the caller's own confirm flag, session stays
/// alive (see module doc for why this changed from the original "full
/// abort" assumption). `confirmed = true` marks the caller confirmed and,
/// if the partner has already both readied and confirmed, finalizes the
/// trade: every offered item/money amount is re-validated against a FRESH
/// DB read (an item could have been used/dropped/sold elsewhere since it
/// was offered), and if anything no longer checks out the whole trade is
/// aborted instead of partially completing.
///
/// No longer gated on `both_ready()` first -- see `handle_ready`'s doc
/// comment for why (real capture showed Confirm arriving before Ready).
pub(crate) fn handle_confirm(
    db_conn: &mut db::Db,
    player: &Arc<ConnectedPlayer>,
    confirmed: bool,
    logger: &Logger,
) -> Result<Option<FinalizeOutcome>> {
    let Some(session) = session_for(player.character_id) else {
        return Ok(None);
    };
    let partner_id = {
        let locked = lock_or_recover(&session);
        locked.partner_of(player.character_id)
    };
    let Some(partner_id) = partner_id else {
        return Ok(None);
    };
    let partner = find_player_by_id(partner_id);

    if !confirmed {
        {
            let mut locked = lock_or_recover(&session);
            if let Some(offer) = locked.offer_mut(player.character_id) {
                offer.confirmed = false;
            }
        }
        player.send_packet(
            &build_confirm(1, false)?,
            "0x0A0A exchange unconfirm (self)",
            logger,
        )?;
        if let Some(partner) = &partner {
            partner.send_packet(
                &build_confirm(2, false)?,
                "0x0A0A exchange unconfirm (partner)",
                logger,
            )?;
        }
        return Ok(None);
    }

    let both_done = {
        let mut locked = lock_or_recover(&session);
        if let Some(offer) = locked.offer_mut(player.character_id) {
            offer.confirmed = true;
        }
        locked.both_ready() && locked.both_confirmed()
    };

    player.send_packet(&build_confirm(1, true)?, "0x0A0A exchange confirm (self)", logger)?;
    if let Some(partner) = &partner {
        partner.send_packet(
            &build_confirm(2, true)?,
            "0x0A0A exchange confirm (partner)",
            logger,
        )?;
    }

    if !both_done {
        return Ok(None);
    }
    let Some(partner) = partner else {
        return Ok(None);
    };
    finalize_and_notify(db_conn, &session, player, partner_id, &partner, logger)
}

/// Shared finalize step for both `handle_ready` and `handle_confirm`, once
/// both sides have both readied and confirmed: validates and performs the
/// DB transfer, then -- on success -- sends 0x0A04 End(0) to both sides
/// (confirmed by real capture to be the packet that actually closes the
/// trade window) and queues the partner's own-thread refresh.
fn finalize_and_notify(
    db_conn: &mut db::Db,
    session: &Arc<Mutex<TradeSession>>,
    player: &Arc<ConnectedPlayer>,
    partner_id: u32,
    partner: &Arc<ConnectedPlayer>,
    logger: &Logger,
) -> Result<Option<FinalizeOutcome>> {
    remove_session(player.character_id, partner_id);
    let (offer_self, offer_partner) = {
        let locked = lock_or_recover(session);
        let self_offer = locked.offer(player.character_id).cloned();
        let partner_offer = locked.offer(partner_id).cloned();
        (self_offer, partner_offer)
    };
    let (Some(offer_self), Some(offer_partner)) = (offer_self, offer_partner) else {
        return Ok(None);
    };

    logger.line(format!(
        "Rapture World finalizing exchange character_id={} <-> character_id={}",
        player.character_id, partner_id
    ));

    let self_money = player.character_snapshot().money;
    let partner_money = partner.character_snapshot().money;
    let outcome = finalize_trade(
        db_conn,
        player.character_id,
        partner_id,
        self_money,
        partner_money,
        &offer_self,
        &offer_partner,
        logger,
    )?;

    match outcome {
        Some(outcome) => {
            player.send_packet(&build_end(0)?, "0x0A04 exchange end (self)", logger)?;
            partner.send_packet(&build_end(0)?, "0x0A04 exchange end (partner)", logger)?;
            partner.queue_trade_refresh(outcome.partner_packets);
            Ok(Some(FinalizeOutcome {
                items: outcome.self_items,
                money: outcome.self_money,
                packets: outcome.self_packets,
            }))
        }
        None => {
            logger.line("Rapture World exchange aborted at finalize: an offered item/amount was no longer valid");
            send_cancel(player, logger)?;
            send_cancel(partner, logger)?;
            Ok(None)
        }
    }
}

struct FinalizeTransferResult {
    self_items: Vec<DbCharacterItem>,
    self_money: u32,
    self_packets: Vec<Vec<u8>>,
    partner_packets: Vec<Vec<u8>>,
}

/// Re-validates both offers against a fresh DB read (item availability,
/// money, AND that the receiving side actually has room -- see
/// `offer_fits`), then -- only if everything still checks out -- performs
/// the actual transfer. Returns `None` (no changes made at all) if
/// anything failed validation, so a full inventory aborts the whole trade
/// instead of dropping or bouncing items.
fn finalize_trade(
    db_conn: &mut db::Db,
    id_a: u32,
    id_b: u32,
    money_a_before: u32,
    money_b_before: u32,
    offer_a: &TradeOffer,
    offer_b: &TradeOffer,
    logger: &Logger,
) -> Result<Option<FinalizeTransferResult>> {
    let mut items_a = db::get_character_items(db_conn, id_a)?;
    let mut items_b = db::get_character_items(db_conn, id_b)?;

    if !offer_valid(&items_a, offer_a, money_a_before) || !offer_valid(&items_b, offer_b, money_b_before)
    {
        logger.line("Rapture World exchange validation failed at finalize");
        return Ok(None);
    }
    if !offer_fits(db_conn, &items_b, offer_a)? || !offer_fits(db_conn, &items_a, offer_b)? {
        logger.line(
            "Rapture World exchange aborted at finalize: receiving inventory has no room",
        );
        return Ok(None);
    }

    let (a_removed, b_gained) =
        transfer_offer(db_conn, id_a, id_b, offer_a, &mut items_a, &mut items_b)?;
    let (b_removed, a_gained) =
        transfer_offer(db_conn, id_b, id_a, offer_b, &mut items_b, &mut items_a)?;

    let money_a = money_a_before
        .saturating_sub(offer_a.money)
        .saturating_add(offer_b.money)
        .min(db::MONEY_CAP);
    let money_b = money_b_before
        .saturating_sub(offer_b.money)
        .saturating_add(offer_a.money)
        .min(db::MONEY_CAP);
    db::set_character_money(db_conn, id_a, money_a)?;
    db::set_character_money(db_conn, id_b, money_b)?;

    let mut packets_a = a_removed;
    packets_a.extend(a_gained);
    let mut packets_b = b_removed;
    packets_b.extend(b_gained);

    Ok(Some(FinalizeTransferResult {
        self_items: items_a,
        self_money: money_a,
        self_packets: packets_a,
        partner_packets: packets_b,
    }))
}

fn offer_valid(items: &[DbCharacterItem], offer: &TradeOffer, current_money: u32) -> bool {
    if offer.money > current_money {
        return false;
    }
    // Sums every offer slot referencing the same `row_id` before comparing
    // against the item's actual count -- checking each slot independently
    // (the previous behavior) let two trade slots each offer, say, 8 of a
    // 10-count stack (8 <= 10 passes both checks individually) while the
    // combined 16 exceeds what the sender actually has, which
    // `transfer_offer` would otherwise grant to the receiver in full.
    let mut offered_totals: Vec<(u32, u32)> = Vec::new();
    for offered in offer.items.iter().flatten() {
        let total = offered_totals
            .iter_mut()
            .find(|(row_id, _)| *row_id == offered.row_id);
        match total {
            Some((_, count)) => *count += u32::from(offered.count),
            None => offered_totals.push((offered.row_id, u32::from(offered.count))),
        }
    }
    offered_totals.iter().all(|(row_id, total_offered)| {
        items
            .iter()
            .any(|item| item.row_id == *row_id && u32::from(item.count) >= *total_offered)
    })
}

/// Dry-run: would every item in `offer` fit into `receiver_items` (stacking
/// first, free slots otherwise)? Checked for BOTH sides before any DB write
/// happens, so a receiver with a full inventory aborts the whole trade
/// instead of the previous behavior of silently bouncing items back to the
/// sender mid-transfer.
fn offer_fits(db_conn: &mut db::Db, receiver_items: &[DbCharacterItem], offer: &TradeOffer) -> Result<bool> {
    let mut sim_items = receiver_items.to_vec();
    for offered in offer.items.iter().flatten() {
        let max_stack = db::get_item_definition(db_conn, offered.item_type, offered.type_id)?
            .map(|definition| definition.max_stack.max(1))
            .unwrap_or(1);
        let template = DbCharacterItem {
            row_id: 0,
            bag: 0,
            slot: 0,
            item_type: offered.item_type,
            type_id: offered.type_id,
            quality: offered.quality,
            gems: offered.gems,
            count: offered.count,
            craft_name: offered.craft_name.clone(),
            dyed: offered.dyed,
        };
        if !would_fit(&mut sim_items, &template, offered.count, max_stack) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Moves every item in `offer` (already validated, including capacity via
/// `offer_fits`) from `from_items`/DB into `to_id`'s inventory, using the
/// same stacking logic as picking an item up off the ground. Returns the
/// wire packets needed to reflect the change: RemoveItem (or a reduced-
/// count RemoveItem) for `from_id`, AddItem for `to_id` -- matching what a
/// real Aries server sends on trade completion (confirmed by packet
/// capture, 2026-07-08), instead of the previous full-inventory-list resync
/// that didn't actually clear the traded item from either client's view.
fn transfer_offer(
    db_conn: &mut db::Db,
    from_id: u32,
    to_id: u32,
    offer: &TradeOffer,
    from_items: &mut Vec<DbCharacterItem>,
    to_items: &mut Vec<DbCharacterItem>,
) -> Result<(Vec<Vec<u8>>, Vec<Vec<u8>>)> {
    let mut from_packets = Vec::new();
    let mut to_packets = Vec::new();

    for offered in offer.items.iter().flatten() {
        let Some(index) = from_items.iter().position(|item| item.row_id == offered.row_id) else {
            continue;
        };
        // Clamped to whatever's actually left on this row at this point in
        // the loop -- defense in depth against two offer slots referencing
        // the same row_id (should already be rejected by `offer_valid`,
        // but this guarantees the receiver can never be granted more than
        // the sender actually loses, matching the "leftover" defensiveness
        // already used below for the capacity case).
        let grant_count = offered.count.min(from_items[index].count);
        let remaining_after = from_items[index].count.saturating_sub(grant_count);
        if remaining_after == 0 {
            from_packets.push(build_remove_item(&from_items[index], true)?);
            db::delete_character_item(db_conn, from_id, offered.row_id)?;
            from_items.remove(index);
        } else {
            let mut updated = from_items[index].clone();
            updated.count = remaining_after;
            from_packets.push(build_remove_item(&updated, false)?);
            db::set_character_item_count(db_conn, from_id, offered.row_id, remaining_after)?;
            from_items[index].count = remaining_after;
        }

        if grant_count == 0 {
            continue;
        }

        let template = DbCharacterItem {
            row_id: 0,
            bag: 0,
            slot: 0,
            item_type: offered.item_type,
            type_id: offered.type_id,
            quality: offered.quality,
            gems: offered.gems,
            count: grant_count,
            craft_name: offered.craft_name.clone(),
            dyed: offered.dyed,
        };
        let max_stack = db::get_item_definition(db_conn, offered.item_type, offered.type_id)?
            .map(|definition| definition.max_stack.max(1))
            .unwrap_or(1);
        let (add_packets, leftover) =
            give_item_to_inventory(db_conn, to_id, to_items, &template, grant_count, max_stack)?;
        to_packets.extend(add_packets);
        if leftover > 0 {
            // Shouldn't happen now that offer_fits pre-validates capacity,
            // but stay defensive: give back whatever didn't fit rather than
            // destroying it, and tell the sender's client about it.
            let (returned_packets, _still_left) =
                give_item_to_inventory(db_conn, from_id, from_items, &template, leftover, max_stack)?;
            from_packets.extend(returned_packets);
        }
    }
    Ok((from_packets, to_packets))
}
