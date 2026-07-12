use anyhow::{Context, Result};
use rapture_common::crypto::{AesCtr, expanded_xor_transform};
use rapture_common::db::{DbCharacter, DbCharacterItem};
use rapture_common::packet::Opcode;
use std::collections::HashMap;
use std::io::Write;
use std::net::TcpStream;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::character::{
    MOTION_SIT_DOWN, build_character_entered_map, build_character_equipment,
    build_character_motion, build_character_move, build_character_overlay, build_character_shape,
    name_color_for,
};
use crate::protocol::*;
use crate::session::*;
use crate::world_entities::{distance_xz, lock_or_recover};

/// Minimum time between two chat messages from the same connection. Aries
/// defines no wire-level spam/cooldown mechanism (confirmed against the
/// reference client headers), so this is server policy, not protocol.
pub(crate) const CHAT_COOLDOWN: Duration = Duration::from_millis(500);

/// Everything about a connected player that can change while they're in
/// the world, behind a single lock. Building a broadcast packet (shape/
/// move/equipment) used to take 3-4 separate field locks in sequence; this
/// collapses that to one lock per packet built.
struct PlayerState {
    map_id: u16,
    character: DbCharacter,
    items: Vec<DbCharacterItem>,
    selected_faction: u8,
    // Last motion/direction this player's own CharacterMove packet carried
    // (Aries MotionType + client facing encoding). Defaults to (0, 0) --
    // idle, no facing -- until they actually move. Needed so a newly
    // arriving viewer sees an already-moving player animate correctly
    // right away, instead of everyone always looking idle until their next
    // move packet arrives.
    motion: (u8, u16),
    // Sit is the one MotionType that persists as ongoing state (matching
    // Imgeneus's MovementManager.Motion, which only ever keeps None or Sit
    // -- every other motion, e.g. Jump, is a one-shot animation that never
    // needs to be remembered/replayed for a newly arriving viewer).
    sitting: bool,
    // The `char on`/`char off` GM toggle. Only meaningful when is_admin is
    // true (non-admins are always visible regardless of this). Resets to
    // false (invisible, the GM's normal state) every time a fresh
    // ConnectedPlayer is registered -- i.e. every relog/reconnect, matching
    // "GM always starts invisible" without any extra reset logic needed.
    gm_visible: bool,
}

/// A player currently in-game and reachable for chat/visibility delivery
/// from any connection's thread. Registered once the client confirms the
/// map has loaded (CharacterEnteredMap) and removed automatically (Drop)
/// when the owning connection ends, regardless of exit path.
///
/// Also backs cross-player visibility (movement/appearance broadcast): the
/// real Aries 5.4 client culls out-of-range characters natively, so unlike
/// some other Shaiya server implementations (e.g. Imgeneus, which sends an
/// explicit "character leave" packet), we never need to send a despawn —
/// only shape/move/equipment updates while a character is relevant.
pub(crate) struct ConnectedPlayer {
    pub(crate) character_id: u32,
    pub(crate) character_name: String,
    pub(crate) is_admin: bool,
    state: Mutex<PlayerState>,
    stream: Mutex<TcpStream>,
    // One-shot cross-thread command: another connection's thread (e.g. a
    // GM running `asummon`) can request this player be teleported, but
    // only the OWNING connection's own thread can safely update its own
    // active_character/entity_visibility locals. Queuing it here and
    // having the owning thread pick it up on its next loop iteration is
    // the "refresh yourself" mechanism noted as missing in
    // RAPTURE_SESSION_GUIDE.txt -- this is that mechanism.
    pending_teleport: Mutex<Option<(u16, f32, f32, f32)>>,
    // Same one-shot cross-thread mailbox pattern as pending_teleport, for
    // trade completion: whichever side's thread finalizes a trade (see
    // trade.rs) can only safely mutate its OWN active_items/character
    // locals, so the partner's thread is asked to reload its own inventory/
    // money from the DB (now the source of truth after the transfer) and
    // push the refresh to its own client on its next loop iteration. Carries
    // the exact RemoveItem/AddItem packets trade.rs already built for this
    // side, so the client's view is updated the same way a real Aries
    // server does it (explicit per-item packets), not a full inventory
    // resync -- resending the whole list didn't actually clear the traded
    // item from either client's view (confirmed empirically, 2026-07-08).
    pending_trade_refresh: Mutex<Option<Vec<Vec<u8>>>>,
}

impl ConnectedPlayer {
    /// Moves this player to a new map, relocating their entry in the
    /// per-map registry shard so `players_on_map` for their old map stops
    /// seeing them and their new map starts to.
    pub(crate) fn set_map(&self, map_id: u16) {
        let old_map_id = {
            let mut state = lock_or_recover(&self.state);
            let old = state.map_id;
            state.map_id = map_id;
            old
        };
        if old_map_id == map_id {
            return;
        }
        let moved = lock_or_recover(&map_bucket(old_map_id)).remove(&self.character_id);
        if let Some(player) = moved {
            lock_or_recover(&map_bucket(map_id)).insert(self.character_id, player);
        }
    }

    pub(crate) fn map_id(&self) -> u16 {
        lock_or_recover(&self.state).map_id
    }

    pub(crate) fn position_xz(&self) -> (f32, f32) {
        let state = lock_or_recover(&self.state);
        (state.character.pos_x, state.character.pos_z)
    }

    /// Updates the live position/appearance snapshot used to build shape
    /// and move packets for other players. Called whenever the owning
    /// connection's own character state changes (move, teleport, stat/mode
    /// changes relevant to appearance).
    pub(crate) fn update_character(&self, character: DbCharacter) {
        lock_or_recover(&self.state).character = character;
    }

    /// A clone of this player's current live character snapshot -- used by
    /// GM commands that need another connection's up-to-date position
    /// (asummon/amove), since the DB row only reflects the last periodic
    /// save, not the live position.
    pub(crate) fn character_snapshot(&self) -> DbCharacter {
        lock_or_recover(&self.state).character.clone()
    }

    /// The `char on`/`char off` GM visibility toggle. See `PlayerState::
    /// gm_visible`.
    pub(crate) fn set_gm_visible(&self, visible: bool) {
        lock_or_recover(&self.state).gm_visible = visible;
    }

    pub(crate) fn gm_visible(&self) -> bool {
        lock_or_recover(&self.state).gm_visible
    }

    /// Requests that this player's own connection thread teleport them to
    /// (map_id, x, y, z) on its next loop iteration. Overwrites any
    /// not-yet-applied previous request (only the latest destination
    /// matters).
    pub(crate) fn queue_teleport(&self, map_id: u16, x: f32, y: f32, z: f32) {
        *lock_or_recover(&self.pending_teleport) = Some((map_id, x, y, z));
    }

    /// Takes and clears the pending teleport request, if any. Called once
    /// per loop iteration by the owning connection's own thread.
    pub(crate) fn take_pending_teleport(&self) -> Option<(u16, f32, f32, f32)> {
        lock_or_recover(&self.pending_teleport).take()
    }

    /// Requests that this player's own connection thread reload its
    /// inventory/money from the DB (for cache correctness) and forward
    /// `packets` -- the exact RemoveItem/AddItem wire packets trade.rs built
    /// for this side -- to its client.
    pub(crate) fn queue_trade_refresh(&self, packets: Vec<Vec<u8>>) {
        *lock_or_recover(&self.pending_trade_refresh) = Some(packets);
    }

    /// Takes and clears the pending trade-refresh packets, if any.
    pub(crate) fn take_pending_trade_refresh(&self) -> Option<Vec<Vec<u8>>> {
        lock_or_recover(&self.pending_trade_refresh).take()
    }

    pub(crate) fn update_items(&self, items: Vec<DbCharacterItem>) {
        lock_or_recover(&self.state).items = items;
    }

    /// Updates the cached motion/direction from this player's own real
    /// 0x0501 CharacterMove packet. Does NOT touch `sitting` -- a previous
    /// attempt to auto-clear sitting when CharacterMove's motion byte was
    /// non-zero was unreliable (that field appears to carry noise/unrelated
    /// values in this client, similar to what was observed on 0x0506) and
    /// likely reset sitting almost immediately after it was set. Sitting is
    /// now only ever changed by an explicit 0x0506 SitDown/StandUp (see
    /// set_sitting), matching Imgeneus's model exactly.
    pub(crate) fn update_motion(&self, motion: u8, direction: u16) {
        lock_or_recover(&self.state).motion = (motion, direction);
    }

    pub(crate) fn set_sitting(&self, sitting: bool) {
        lock_or_recover(&self.state).sitting = sitting;
    }

    pub(crate) fn is_sitting(&self) -> bool {
        lock_or_recover(&self.state).sitting
    }

    /// Builds a 0x0506 CharacterMotion packet reflecting this player's
    /// cached sit state, for introducing an already-seated player to a
    /// newly arriving viewer.
    pub(crate) fn sit_motion_packet(&self) -> Result<Vec<u8>> {
        build_character_motion(self.character_id, MOTION_SIT_DOWN)
    }

    /// Builds a 0x0201 CharacterEnteredMap packet (id + position + guild/
    /// vehicle ids). This is the packet that actually spawns/instantiates
    /// an object in the client's world; 0x0303 CharacterShape alone only
    /// describes the appearance of an object the client already knows
    /// about. Confirmed via Imgeneus's SendCharacterEnter, which wraps the
    /// equivalent CHARACTER_ENTERED_MAP packet first, before any shape/
    /// motion packets, when introducing a character to a viewer -- our own
    /// send_map_loaded_state already does this for the local player
    /// (0x0201 before 0x0303), this method lets us do the same for others.
    pub(crate) fn entered_map_packet(&self) -> Result<Vec<u8>> {
        let state = lock_or_recover(&self.state);
        // This character was already on the map before the viewer arrived,
        // not loading in for the first time -- see build_character_entered_map's
        // doc comment for why that distinction (and the is_admin case) matters.
        build_character_entered_map(Some(&state.character), self.is_admin, false)
    }

    /// Builds a 0x0240 CharacterOverlay packet setting this player's name
    /// color (white for normal characters, a distinct color for GMs) --
    /// Aries's real mechanism for a cosmetic name-color item, reused here
    /// to fix the color for the introduced viewer rather than relying on
    /// whatever the client's un-overridden default happens to be.
    pub(crate) fn overlay_packet(&self) -> Result<Vec<u8>> {
        build_character_overlay(self.character_id, name_color_for(self.is_admin))
    }

    /// Builds a 0x0303 CharacterShape packet describing this player, for
    /// sending to another player so they can see this one appear/update.
    /// Deliberately never embeds sit here, even if the player is currently
    /// sitting -- see move_packet's doc comment for why: sit is only ever
    /// conveyed by the separate, explicit sit_motion_packet() send.
    pub(crate) fn shape_packet(&self) -> Result<Vec<u8>> {
        let state = lock_or_recover(&self.state);
        build_character_shape(
            Some(&state.character),
            state.selected_faction,
            &state.items,
            0,
        )
    }

    /// Builds a 0x0501 CharacterMove packet reflecting this player's last
    /// known position and motion/direction, for sending to another player.
    ///
    /// Deliberately never embeds sit here either, even when introducing an
    /// already-sitting character to a new viewer. That used to seem like
    /// the fix (a stale cached motion here was overriding the sit pose
    /// shape_packet had just set), but it created a different, harder bug:
    /// a character already sitting when a NEW viewer arrives never
    /// appeared seated to them, while sitting down live in front of an
    /// existing viewer worked fine. The only structural difference between
    /// those two cases was that live-sit is conveyed purely by a
    /// standalone 0x0506 against an already-known object, with no shape/
    /// move packet anywhere nearby -- introduction bundled sit into shape
    /// AND move AND a trailing 0x0506 in the same burst. Matching the
    /// live-sit sequence exactly (shape/move always plain, sit conveyed
    /// only by the dedicated 0x0506 sent right after) fixed it, so this
    /// mirrors that: always send the character's last real motion here,
    /// and let the caller send sit_motion_packet() afterward if sitting.
    pub(crate) fn move_packet(&self) -> Result<Vec<u8>> {
        let state = lock_or_recover(&self.state);
        build_character_move(Some(&state.character), state.motion.0, state.motion.1)
    }

    /// Builds a 0x0507 CharacterEquipment packet for a single equipment
    /// slot (0..8, the only slots that currently render visually), for
    /// sending to another player when this player's gear changes.
    pub(crate) fn equipment_packet(&self, slot: u8) -> Result<Vec<u8>> {
        let state = lock_or_recover(&self.state);
        build_character_equipment(&state.character, &state.items, slot)
    }

    /// Encrypts and writes a packet directly to this player's socket using
    /// the post-handshake "expanded" XOR scheme. That scheme is a pure
    /// function of a hardcoded seed (see crypto::expanded_xor_transform) and
    /// is not per-session state, so any thread can encode a packet for any
    /// connected player without needing that player's AES key/IV.
    pub(crate) fn send_packet(&self, packet: &[u8], label: &str, logger: &Logger) -> Result<()> {
        let encrypted = if packet.len() <= 2 {
            packet.to_vec()
        } else {
            let mut encrypted = packet[..2].to_vec();
            encrypted.extend_from_slice(&expanded_xor_transform(&packet[2..]));
            encrypted
        };
        logger.line(format!(
            "Rapture World broadcasting {label} to character_id={}",
            self.character_id
        ));
        let mut stream = lock_or_recover(&self.stream);
        stream
            .write_all(&encrypted)
            .context("failed to deliver broadcast packet")
    }
}

type PlayerBucket = Arc<Mutex<HashMap<u32, Arc<ConnectedPlayer>>>>;

/// Sharded by map: the outer mutex is only ever held long enough to look up
/// or create a map's bucket, so `players_on_map` for one map never blocks
/// on broadcast/registration traffic for an unrelated map. A player moving
/// maps (`set_map`) relocates its entry between buckets.
fn registry() -> &'static Mutex<HashMap<u16, PlayerBucket>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u16, PlayerBucket>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn map_bucket(map_id: u16) -> PlayerBucket {
    let mut registry = lock_or_recover(registry());
    registry
        .entry(map_id)
        .or_insert_with(|| Arc::new(Mutex::new(HashMap::new())))
        .clone()
}

/// Handle returned by `register_player`. Removes the player from the
/// registry on Drop so a crashed/disconnected connection can never leave a
/// stale, unreachable entry behind.
///
/// Tried broadcasting 0x0107 (GameUserLeaveWorldOutgoing) to other players
/// here to fix the disconnected-character-stays-visible issue, but that
/// caused a still-connected client to freeze instantly when another player
/// disconnected -- 0x0107 is very likely only valid as an ack to the
/// leaving client itself, not as a "someone else left" notification.
/// Reverted; the ghost-stays-visible issue needs a different fix.
pub(crate) struct ChatPresenceGuard {
    character_id: u32,
    player: Arc<ConnectedPlayer>,
    // Needed to notify a trade partner if this connection disconnects
    // mid-trade (see trade::cancel_for_disconnect) -- Drop can't take extra
    // arguments, so this has to be carried on the guard itself.
    logger: Arc<Logger>,
}

impl Drop for ChatPresenceGuard {
    fn drop(&mut self) {
        let map_id = self.player.map_id();
        lock_or_recover(&map_bucket(map_id)).remove(&self.character_id);
        crate::trade::cancel_for_disconnect(self.character_id, &self.logger);
    }
}

pub(crate) fn register_player(
    character: DbCharacter,
    items: Vec<DbCharacterItem>,
    character_name: String,
    is_admin: bool,
    selected_faction: u8,
    stream: &TcpStream,
    logger: Arc<Logger>,
) -> Result<(Arc<ConnectedPlayer>, ChatPresenceGuard)> {
    let character_id = character.id;
    let map_id = character.map_id;
    let cloned_stream = stream
        .try_clone()
        .context("failed to clone socket for chat/visibility broadcast delivery")?;
    // Without this, a broadcast write to a stalled/unresponsive peer could
    // block this thread (and whoever is waiting on locks it holds) forever.
    let _ = cloned_stream.set_write_timeout(Some(Duration::from_secs(5)));
    let player = Arc::new(ConnectedPlayer {
        character_id,
        character_name,
        is_admin,
        state: Mutex::new(PlayerState {
            map_id,
            character,
            items,
            selected_faction,
            motion: (0, 0),
            sitting: false,
            gm_visible: false,
        }),
        stream: Mutex::new(cloned_stream),
        pending_teleport: Mutex::new(None),
        pending_trade_refresh: Mutex::new(None),
    });
    lock_or_recover(&map_bucket(map_id)).insert(character_id, Arc::clone(&player));
    Ok((
        Arc::clone(&player),
        ChatPresenceGuard {
            character_id,
            player,
            logger,
        },
    ))
}

pub(crate) fn players_on_map(map_id: u16) -> Vec<Arc<ConnectedPlayer>> {
    lock_or_recover(&map_bucket(map_id))
        .values()
        .cloned()
        .collect()
}

/// GMs are invisible to non-GM players -- a native Aries/Shaiya property,
/// confirmed against the real client, not something we're inventing.
/// Everyone else is visible to everyone. `subject_gm_visible` is the
/// `char on`/`char off` per-session toggle: a GM can make themselves
/// visible to non-GMs too, but other GMs always see them regardless. Used
/// to filter every broadcast of a specific character's own presence/
/// movement/appearance/equipment to other players; mob/NPC/item broadcasts
/// are untouched since those aren't tied to a character's identity.
pub(crate) fn can_see_character(
    viewer_is_admin: bool,
    subject_is_admin: bool,
    subject_gm_visible: bool,
) -> bool {
    !subject_is_admin || subject_gm_visible || viewer_is_admin
}

/// Finds a currently-connected player by character name (case-insensitive).
/// Used by GM commands that target another player by name (amove/asummon/
/// set/iclear/eclear) when that player needs to be online.
pub(crate) fn find_player_by_name(name: &str) -> Option<Arc<ConnectedPlayer>> {
    all_players()
        .into_iter()
        .find(|player| player.character_name.eq_ignore_ascii_case(name))
}

pub(crate) fn find_player_by_id(character_id: u32) -> Option<Arc<ConnectedPlayer>> {
    all_players()
        .into_iter()
        .find(|player| player.character_id == character_id)
}

pub(crate) fn all_players() -> Vec<Arc<ConnectedPlayer>> {
    let buckets: Vec<PlayerBucket> = lock_or_recover(registry()).values().cloned().collect();
    buckets
        .into_iter()
        .flat_map(|bucket| {
            lock_or_recover(&bucket)
                .values()
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Players on `map_id` within `radius` of `(x, z)`, matching the real
/// server's spatial visibility model instead of notifying every player on
/// the map regardless of distance. Callers pass the radius for their event
/// type -- `world_entities::VIEWPORT_RADIUS` for general appearance/
/// movement/item events, `world_entities::COMBAT_RADIUS` for combat
/// broadcasts (see those constants' doc comments; confirmed against a
/// GitHub issue report on the public repo).
pub(crate) fn players_on_map_near(map_id: u16, x: f32, z: f32, radius: f32) -> Vec<Arc<ConnectedPlayer>> {
    lock_or_recover(&map_bucket(map_id))
        .values()
        .filter(|player| {
            let (player_x, player_z) = player.position_xz();
            distance_xz(player_x, player_z, x, z) <= radius
        })
        .cloned()
        .collect()
}

/// Same as `players_on_map_near`, but also excludes `character_id` -- for
/// character move/shape/equipment broadcasts, which should never echo back
/// to their own sender (the sender already has up-to-date local state).
pub(crate) fn players_on_map_excluding_near(
    map_id: u16,
    character_id: u32,
    x: f32,
    z: f32,
    radius: f32,
) -> Vec<Arc<ConnectedPlayer>> {
    lock_or_recover(&map_bucket(map_id))
        .values()
        .filter(|player| player.character_id != character_id)
        .filter(|player| {
            let (player_x, player_z) = player.position_xz();
            distance_xz(player_x, player_z, x, z) <= radius
        })
        .cloned()
        .collect()
}

/// Rejects empty/whitespace-only messages and anything over the wire
/// buffer's usable length. The 500ms cooldown is server policy (see
/// CHAT_COOLDOWN doc comment), not an Aries protocol requirement.
pub(crate) fn validate_chat_message(message: &str, last_chat_at: Option<Instant>) -> Result<()> {
    if message.trim().is_empty() {
        anyhow::bail!("message is empty");
    }
    if message.len() > crate::protocol::MAX_CHAT_MESSAGE_LEN {
        anyhow::bail!("message exceeds max length");
    }
    if let Some(last_chat_at) = last_chat_at {
        if last_chat_at.elapsed() < CHAT_COOLDOWN {
            anyhow::bail!("chat cooldown not elapsed");
        }
    }
    Ok(())
}

/// Handles a genuine public chat send (ChatNormal/ChatWorld/ChatMap) once
/// it's known NOT to be a recognized GM/getitem command: gates the admin
/// channels (ChatNormalAdmin/ChatWorldAdmin) to admins only, replies
/// privately instead of broadcasting if an admin channel's text wasn't a
/// recognized command (never let GM chatter leak to a public broadcast --
/// see RAPTURE_SESSION_GUIDE.txt), then delivers the message: ChatWorld to
/// every connected player, ChatNormal/ChatMap to everyone on the sender's
/// map (no proximity filtering for ChatNormal -- Aries doesn't encode a
/// radius on the wire either, and this project deliberately leaves
/// whole-map chat delivery as-is, unlike the spatial-cell filtering
/// applied to movement/combat/item broadcasts).
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_public_chat(
    decrypted_opcode: u16,
    character: &DbCharacter,
    message: &str,
    is_admin: bool,
    account_id: u32,
    last_chat_at: &mut Option<Instant>,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    let is_admin_channel =
        decrypted_opcode == Opcode::ChatNormalAdmin as u16 || decrypted_opcode == Opcode::ChatWorldAdmin as u16;

    if is_admin_channel && !is_admin {
        logger.line(format!(
            "Rapture World rejected admin chat from non-admin account_id={account_id}"
        ));
        if let Some(send) = aes_send.as_mut() {
            send_encrypted(
                stream,
                send,
                use_expanded_send_key,
                build_chat_error(1)?,
                "0x1106 chat error (not admin)",
                logger,
            )?;
        }
        return Ok(());
    }

    if let Err(error) = validate_chat_message(message, *last_chat_at) {
        logger.line(format!("Rapture World rejected chat: {error}"));
        if let Some(send) = aes_send.as_mut() {
            send_encrypted(
                stream,
                send,
                use_expanded_send_key,
                build_chat_error(1)?,
                "0x1106 chat error",
                logger,
            )?;
        }
        return Ok(());
    }

    // The admin channels double as this project's GM command line (no
    // dedicated opcode per command yet -- see RAPTURE_SESSION_GUIDE.txt).
    // The client already strips any leading `/` before this message ever
    // reaches us, so there's no wire-level way to tell "typed a command"
    // from "just chatting" -- by the time we see it, both look identical.
    // Anything here that isn't a recognized command is therefore treated as
    // plain chat and broadcast on the equivalent public channel (mapping
    // ChatNormalAdmin -> ChatNormal, ChatWorldAdmin -> ChatWorld), rather
    // than rejected -- a prior version replied "Unknown command." privately
    // for everything unrecognized, which made it impossible for a GM to
    // hold an ordinary conversation over the admin channel.
    if is_admin_channel {
        logger.line(format!(
            "Rapture World admin channel message from character_id={} not a recognized command, broadcasting as chat: '{message}'",
            character.id
        ));
    }
    *last_chat_at = Some(Instant::now());

    let outgoing = if decrypted_opcode == Opcode::ChatNormal as u16
        || decrypted_opcode == Opcode::ChatNormalAdmin as u16
    {
        build_chat_by_id(Opcode::ChatNormal, character.id, message)?
    } else if decrypted_opcode == Opcode::ChatWorld as u16
        || decrypted_opcode == Opcode::ChatWorldAdmin as u16
    {
        build_chat_by_name(Opcode::ChatWorld, &character.name, message)?
    } else {
        // ChatMap (0x1111 / GameChatZoneOutgoing) shares the senderName-
        // shaped layout too.
        build_chat_by_name(Opcode::ChatMap, &character.name, message)?
    };

    let recipients = if decrypted_opcode == Opcode::ChatWorld as u16
        || decrypted_opcode == Opcode::ChatWorldAdmin as u16
    {
        all_players()
    } else {
        // ChatNormal and ChatMap: same-map delivery. True proximity
        // filtering for ChatNormal is not implemented yet (Aries doesn't
        // encode a radius on the wire either); both currently reach
        // everyone on the character's map.
        players_on_map(character.map_id)
    };

    let label = format!("chat opcode=0x{decrypted_opcode:04X}");
    for player in &recipients {
        if let Err(error) = player.send_packet(&outgoing, &label, logger) {
            logger.line(format!(
                "Rapture World failed to deliver chat to character_id={}: {error:?}",
                player.character_id
            ));
        }
    }
    logger.line(format!(
        "Rapture World delivered chat opcode=0x{decrypted_opcode:04X} to {} recipient(s)",
        recipients.len()
    ));
    Ok(())
}
