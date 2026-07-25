use anyhow::{Context, Result};
use rapture_common::crypto::{AesCtr, derive_world_aes_iv};
use rapture_common::db::{
    self, DbCharacter, DbCharacterItem, DbCharacterSkill, DbQuickSlot, NewCharacter,
};
use rapture_common::hex::to_hex;
use rapture_common::packet::{Opcode, read_fixed_ascii, read_le_u16, read_le_u32};
use std::collections::HashMap;
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

mod character;
mod chat;
mod combat;
mod commands;
mod compose;
mod enchant;
mod gems;
mod drops;
mod experience;
mod inventory;
mod itemmall;
mod map_items;
mod mount;
mod movement;
mod protocol;
mod session;
mod shop;
mod skillcast;
mod skills;
mod svmap;
mod statpoints;
mod sweeps;
mod trade;
mod warehouse;
mod world_entities;
mod world_state;

use character::*;
use chat::*;
use combat::*;
use commands::*;
use compose::*;
use enchant::*;
use gems::*;
use drops::*;
use experience::*;
use inventory::*;
use itemmall::*;
use map_items::*;
use mount::*;
use movement::*;
use protocol::*;
use session::*;
use shop::*;
use skillcast::*;
use skills::*;
use svmap::*;
use statpoints::*;
use sweeps::*;
use trade::*;
use warehouse::*;
use world_entities::*;
use world_state::*;

static NEXT_CONNECTION_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

#[derive(Clone, Copy, Debug)]
struct AutoAttackState {
    target_mob_id: u32,
}

/// Centralized GM permission gate. Returns true if the action is allowed.
/// If not, logs the rejection, sends the 0xF501 admin-command-error ack
/// (Aries's `GameAdminCmdErrorOutgoing`, errorType = the rejected opcode),
/// and the caller should `continue` the dispatch loop. Every GM opcode
/// handler should start with `if !require_admin(...) { continue; }` instead
/// of hand-rolling the check -- Imgeneus repeats the raw boolean check in
/// ~40 separate handlers; this keeps it in one place.
fn require_admin(
    is_admin: bool,
    opcode: u16,
    account_id: u32,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<bool> {
    if is_admin {
        return Ok(true);
    }
    logger.line(format!(
        "Rapture World rejected admin command 0x{opcode:04X} from non-admin account_id={account_id}"
    ));
    if let Some(send) = aes_send.as_mut() {
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_gm_command_error(opcode)?,
            "0xF501 admin command rejected",
            logger,
        )?;
    }
    Ok(false)
}

/// Shared guard for chat-text GM commands (mmake/mera/nmake/nera/
/// mobreload/npcreload and friends): rejects privately if the account
/// isn't admin, and bails out if there's no active character to act
/// through. Returns true if the caller should proceed; false means the
/// caller should `continue` its dispatch loop, no further action needed.
fn require_gm_command(
    is_admin: bool,
    account_id: u32,
    active_character: &Option<DbCharacter>,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<bool> {
    if !is_admin {
        logger.line(format!(
            "Rapture World rejected GM command from non-admin account_id={account_id}"
        ));
        if let Some(send) = aes_send.as_mut() {
            send_encrypted(
                stream,
                send,
                use_expanded_send_key,
                build_chat_normal(active_character.as_ref().map_or(0, |c| c.id), "Admin only.")?,
                "0x1101 admin-only rejection",
                logger,
            )?;
        }
        return Ok(false);
    }
    if active_character.is_none() {
        logger.line("Rapture World rejected GM command without active character");
        return Ok(false);
    }
    Ok(true)
}

fn handle_client(mut stream: TcpStream, logger_arc: Arc<Logger>) -> Result<()> {
    let logger = &*logger_arc;
    // Every connection is its own thread, so tag all of this thread's log
    // lines with a connection id -- with multiple accounts connected at
    // once, log lines from different connections interleave in the shared
    // file, which is otherwise indistinguishable from real corruption.
    let connection_id = NEXT_CONNECTION_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    set_connection_tag(format!("conn{connection_id}"));
    // Bounds how long the main loop can block in stream.read() so cross-
    // thread work queued on this player's ConnectedPlayer (right now just
    // queue_teleport, e.g. from another connection's `asummon`) and local
    // periodic work (autoattack swings) gets picked up promptly even if
    // this client isn't otherwise sending
    // anything. This is NOT a liveness/disconnect check -- that's still
    // owned entirely by TCP keepalive (apply_keepalive below); a timeout
    // here always just means "no data yet, check pending work, read
    // again". Conflating the two was the earlier read-timeout bug (see
    // RAPTURE_SESSION_GUIDE.txt) -- the fix there was to stop using a
    // timeout as a disconnect signal, not to stop using one at all.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let mut db_conn = db::connect()?;
    let mut aes_recv: Option<AesCtr> = None;
    let mut aes_send: Option<AesCtr> = None;
    let mut use_expanded_send_key = false;
    let mut account_id = 0u32;
    let mut selected_faction = FACTION_NONE;
    let mut max_mode = MAX_MODE_ULTIMATE;
    // Loaded from the account row at handshake (Users_Master.Admin /
    // AdminLevel -- real retail columns, no schema change needed). Stays
    // fixed for the lifetime of this connection; relog doesn't reload it
    // since it's the same account throughout.
    let mut is_admin = false;
    let mut active_character: Option<DbCharacter> = None;
    let mut active_items: Vec<DbCharacterItem> = Vec::new();
    // All non-deleted characters on the account, for the character-select
    // list (0x0101). Distinct from `active_character`, which is only the
    // one currently selected/being played.
    #[allow(unused_assignments)]
    let mut active_characters: Vec<(DbCharacter, Vec<DbCharacterItem>)> = Vec::new();
    let mut active_skills: Vec<DbCharacterSkill> = Vec::new();
    let mut active_quick_slots: Vec<DbQuickSlot> = Vec::new();
    // Account-wide warehouse contents (shared across every character on the
    // account), loaded on character select alongside the rest of the
    // session state -- see warehouse.rs.
    let mut active_stored_items: Vec<DbCharacterItem> = Vec::new();
    // Item Mall / cash shop point balance (Users_Master.Point), also
    // account-wide -- loaded alongside active_stored_items.
    let mut active_account_points: u32 = 0;
    // Consumable cooldowns (keyed by PS_GameDefs.dbo.Items.ReqIg), scoped to
    // the currently active character's online session: cleared whenever a
    // character is selected or created, so relogging or switching character
    // always starts with cooldowns reset, matching the desired behavior.
    let mut item_cooldowns: HashMap<u8, Instant> = HashMap::new();
    // Skill cooldowns (ResetTime), keyed by SkillID -- same reset-on-relog
    // scoping as item_cooldowns above.
    let mut skill_cooldowns: HashMap<u16, Instant> = HashMap::new();
    // Active buffs on this connection's own character (self-only, see
    // skillcast.rs) -- covers both stat buffs and periodic heal-over-time
    // effects (the same kind of object, see ActiveBuff's doc). Same
    // reset-on-relog scoping as the cooldown maps above; ticked/expired
    // every loop iteration by `process_active_buffs`.
    let mut active_buffs: Vec<ActiveBuff> = Vec::new();
    // Whether this connection's character is currently mounted (see
    // mount.rs) -- ephemeral, session-only state, not persisted: the
    // underlying mount item stays equipped in slot 13 regardless, same
    // reset-on-relog scoping as the cooldown maps above.
    let mut mounted = false;
    // A skill cast currently in progress (`ReadyTime` > 0), if any --
    // resolved by `process_pending_cast` once its delay elapses, and
    // cancelled by movement (see the `CharacterMove` handling below).
    let mut pending_cast: Option<PendingCast> = None;
    let mut entity_visibility = EntityVisibility::default();
    let mut position_dirty = false;
    let mut last_position_save = Instant::now();
    // Passive HP/MP/SP regen cadence (see character::process_passive_regen)
    // -- one tracker per connection, same reset-on-connect scoping as
    // last_position_save.
    let mut last_regen_at = Instant::now();
    let mut recv_buffer = Vec::<u8>::new();
    // Held once the handshake identifies the account; dropped automatically
    // when this function returns (any exit path), releasing the account for
    // another login. Never read again after being set, only kept alive for
    // its Drop side effect.
    #[allow(unused_assignments, unused_variables)]
    let mut account_guard: Option<AccountSessionGuard> = None;
    // Registered once the client confirms the map loaded, so other
    // connections can broadcast chat to this player. Dropped automatically
    // on any exit path, unregistering the player.
    #[allow(unused_assignments, unused_variables)]
    let mut chat_presence: Option<ChatPresenceGuard> = None;
    let mut connected_player: Option<Arc<ConnectedPlayer>> = None;
    let mut last_chat_at: Option<Instant> = None;
    let mut last_auto_attack_at: Option<Instant> = None;
    let mut active_auto_attack: Option<AutoAttackState> = None;
    // Set true when a mob's blow kills this character; gates movement/attack/
    // skill input until a resurrection clears it (see the death-gating
    // checks below and the `/town` revive escape hatch). Mirrored to the
    // shared ConnectedPlayer::alive flag so the combat sweep also stops
    // targeting this player.
    let mut player_dead = false;

    logger.line("Rapture World client connected");

    loop {
        let mut buffer = [0u8; 2048];
        let read = match stream.read(&mut buffer) {
            Ok(read) => Some(read),
            // The 300ms read timeout set above firing is expected and
            // routine -- it just means no data arrived in that window, NOT
            // that the client is gone (TCP keepalive owns that). Fall
            // through to the pending-teleport check below, then go back to
            // reading, instead of treating this like EOF/an error.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                None
            }
            Err(error) => return Err(error.into()),
        };

        // Cross-thread teleport requests (e.g. another connection's GM
        // running `asummon`) land here via ConnectedPlayer::queue_teleport
        // -- only this, the owning thread, may safely touch
        // active_character/entity_visibility, so it's picked up and applied
        // once per loop wakeup rather than from the requesting thread.
        let pending_teleport = connected_player
            .as_ref()
            .and_then(|player| player.take_pending_teleport());
        if let Some((map_id, x, y, z)) = pending_teleport {
            if let Some(character) = active_character.as_mut() {
                apply_teleport(
                    character,
                    &connected_player,
                    is_admin,
                    &mut entity_visibility,
                    &mut db_conn,
                    &mut stream,
                    &mut aes_send,
                    use_expanded_send_key,
                    &mut position_dirty,
                    &mut last_position_save,
                    map_id,
                    x,
                    y,
                    z,
                    "asummon",
                    logger,
                )?;
                logger.line(format!(
                    "Rapture World applied queued teleport character_id={} map={} pos=({:.2}, {:.2}, {:.2})",
                    character.id, character.map_id, character.pos_x, character.pos_y, character.pos_z
                ));
            }
        }

        // Cross-thread trade-completion refresh: the OTHER side of a trade
        // this connection just completed (see trade.rs) already had its
        // items/money changed directly in the DB by that other thread --
        // only this, the owning thread, may safely touch active_items/
        // active_character, so it reloads them here and pushes the refresh
        // to its own client. Same mailbox pattern as pending_teleport above.
        let trade_refresh_packets = connected_player
            .as_ref()
            .and_then(|player| player.take_pending_trade_refresh());
        if let Some(trade_packets) = trade_refresh_packets {
            if let Some(character) = active_character.as_mut() {
                active_items = db::get_character_items(&mut db_conn, character.id)?;
                if let Some(fresh) = db::get_character_by_id(&mut db_conn, account_id, character.id)?
                {
                    character.money = fresh.money;
                }
                if let Some(player) = connected_player.as_ref() {
                    player.update_items(active_items.clone());
                    player.update_character(character.clone());
                }
                if let Some(send) = aes_send.as_mut() {
                    for packet in trade_packets {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            packet,
                            "0x0206/0x0205 trade refresh item",
                            logger,
                        )?;
                    }
                    send_encrypted(
                        &mut stream,
                        send,
                        use_expanded_send_key,
                        build_set_money(character.money)?,
                        "0x0213 trade refresh money",
                        logger,
                    )?;
                }
                logger.line(format!(
                    "Rapture World applied trade refresh character_id={} money={}",
                    character.id, character.money
                ));
            }
        }

        // Cross-thread mob-kill EXP grant: a DoT tick (background sweep,
        // see sweeps::run_mob_dot_sweep) that finished off a mob this
        // character DoT'd queues the kill here instead of touching
        // active_character directly from that thread -- same mailbox
        // pattern as pending_teleport/pending_trade_refresh above.
        let pending_mob_kills = connected_player
            .as_ref()
            .map(|player| player.take_pending_mob_kills())
            .unwrap_or_default();
        for (mob_id, mob_level, mob_exp) in pending_mob_kills {
            grant_mob_kill_exp(
                mob_id,
                mob_level,
                mob_exp,
                &mut active_character,
                &connected_player,
                &active_items,
                &mut db_conn,
                &mut stream,
                &mut aes_send,
                use_expanded_send_key,
                logger,
            )?;
        }

        // Cross-thread incoming mob melee damage: the combat sweep
        // (sweeps::run_mob_combat_sweep) decided a mob hit this character but
        // can't touch its HP -- it queues each hit here and this, the owning
        // thread, applies it (combat::apply_incoming_mob_damage), keeping HP
        // authority on one thread. Same mailbox pattern as the kills above.
        let pending_damage = connected_player
            .as_ref()
            .map(|player| player.take_pending_damage())
            .unwrap_or_default();
        for (mob_global_id, mob_id, raw_attack) in pending_damage {
            if player_dead {
                break;
            }
            if apply_incoming_mob_damage(
                mob_global_id,
                mob_id,
                raw_attack,
                &mut active_character,
                &connected_player,
                &active_items,
                &mut db_conn,
                &mut stream,
                &mut aes_send,
                use_expanded_send_key,
                logger,
            )? {
                // Died: stop any in-progress auto-attack and enter the
                // dead-input-gated state until resurrection.
                player_dead = true;
                active_auto_attack = None;
                last_auto_attack_at = None;
                pending_cast = None;
            }
        }

        // Active buff maintenance (stat buffs + periodic heal-over-time
        // ticks, see skillcast.rs) -- checked every loop wakeup, same
        // reasoning as the pending-teleport/trade-refresh checks above, so
        // a 3-second heal cadence is kept even while the client is
        // actively sending other packets, not just during an idle
        // read-timeout wakeup.
        process_active_buffs(
            &mut active_buffs,
            &mut active_character,
            &connected_player,
            &active_items,
            &mut db_conn,
            &mut stream,
            &mut aes_send,
            use_expanded_send_key,
            logger,
        )?;

        // Passive HP/MP/SP regen (5% of max every 10s, unconditional --
        // see character::process_passive_regen) -- same per-iteration
        // cadence reasoning as the buff ticks above.
        process_passive_regen(
            &mut last_regen_at,
            player_dead,
            &mut active_character,
            &connected_player,
            &active_items,
            &mut db_conn,
            &mut stream,
            &mut aes_send,
            use_expanded_send_key,
            logger,
        )?;

        // A delayed skill cast (`ReadyTime` > 0) whose time has come --
        // same per-iteration cadence reasoning as the buff ticks above.
        process_pending_cast(
            &mut pending_cast,
            &mut active_character,
            &connected_player,
            &active_items,
            &active_skills,
            &mut skill_cooldowns,
            &mut active_buffs,
            &mut db_conn,
            &mut stream,
            &mut aes_send,
            use_expanded_send_key,
            logger,
        )?;

        let Some(read) = read else {
            if let Some(auto_attack) = active_auto_attack {
                if !process_auto_attack_swing(
                    auto_attack.target_mob_id,
                    mounted,
                    &mut active_character,
                    &connected_player,
                    &active_items,
                    &mut db_conn,
                    &mut stream,
                    &mut aes_send,
                    use_expanded_send_key,
                    &mut last_auto_attack_at,
                    logger,
                )? {
                    active_auto_attack = None;
                }
            }
            continue;
        };
        if read == 0 {
            if position_dirty {
                if let Some(character) = active_character.as_ref() {
                    db::save_character_position(
                        &mut db_conn,
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
                }
            }
            if let Some(character) = active_character.as_ref() {
                persist_active_buffs(&mut db_conn, character.id, &active_buffs)?;
            }
            logger.line("Rapture World client disconnected");
            return Ok(());
        }

        recv_buffer.extend_from_slice(&buffer[..read]);
        if LOG_RAW_PACKETS {
            logger.line(format!(
                "Rapture World received {read} bytes: {}",
                to_hex(&buffer[..read])
            ));
        }

        let mut frames_in_read = 0usize;
        loop {
            if recv_buffer.len() < 2 {
                break;
            }

            let frame_length = read_le_u16(&recv_buffer[0..2])? as usize;
            if frame_length < 4 {
                anyhow::bail!("invalid world frame length={frame_length}");
            }
            if recv_buffer.len() < frame_length {
                logger.line(format!(
                    "Rapture World waiting for complete frame length={} buffered={}",
                    frame_length,
                    recv_buffer.len()
                ));
                break;
            }

            let bytes = recv_buffer[..frame_length].to_vec();
            recv_buffer.drain(..frame_length);
            frames_in_read += 1;
            if frames_in_read == 2 {
                logger.line("Rapture World processing multiple frames from one TCP read");
            }

            let length = read_le_u16(&bytes[0..2])?;
            let opcode = read_le_u16(&bytes[2..4])?;
            if aes_recv.is_none() || LOG_RAW_PACKETS {
                logger.line(format!(
                    "Rapture World frame length={length} opcode=0x{opcode:04X}"
                ));
            }

            if aes_recv.is_none() && opcode == Opcode::GameHandshake as u16 && bytes.len() >= 24 {
                let user_id = read_le_u32(&bytes[4..8])?;
                let identity: [u8; 16] = bytes[8..24].try_into()?;
                logger.line(format!(
                    "Rapture World handshake user_id={user_id} identity={}",
                    to_hex(&identity)
                ));

                let Some(session) = load_world_session(user_id)? else {
                    logger.line(format!(
                        "Rapture World session file not found for account_id={user_id}"
                    ));
                    continue;
                };

                if identity != session.identity {
                    logger.line(format!(
                        "Rapture World session identity mismatch, saved={}",
                        to_hex(&session.identity)
                    ));
                    continue;
                }

                account_id = session.account_id;
                let Some(guard) = AccountSessionGuard::try_acquire(account_id, logger) else {
                    continue;
                };
                #[allow(unused_assignments)]
                {
                    account_guard = Some(guard);
                }

                if let Some(account) = db::get_account_by_id(&mut db_conn, account_id)? {
                    selected_faction = if is_valid_account_faction(account.faction) {
                        account.faction
                    } else {
                        FACTION_NONE
                    };
                    max_mode = account.max_grow;
                    is_admin = account.is_admin;
                    logger.line(format!(
                        "Rapture World loaded account id={} username='{}' faction={} max_mode={} is_admin={}",
                        account.id, account.username, selected_faction, max_mode, is_admin
                    ));
                } else {
                    selected_faction = FACTION_NONE;
                    max_mode = MAX_MODE_ULTIMATE;
                    is_admin = false;
                    logger.line(format!(
                        "Rapture World account id={account_id} not found; using faction=None"
                    ));
                }
                active_characters = db::get_characters_for_account(&mut db_conn, account_id)?
                    .into_iter()
                    .map(|character| {
                        let items = db::get_character_items(&mut db_conn, character.id)?;
                        Ok((character, items))
                    })
                    .collect::<Result<Vec<_>>>()?;
                logger.line(format!(
                    "Rapture World session account_id={account_id} characters_loaded={}",
                    active_characters.len()
                ));

                let world_iv = derive_world_aes_iv(&session.login_iv);
                logger.line(format!("Rapture World AES key: {}", to_hex(&session.key)));
                logger.line(format!(
                    "Rapture World AES login iv: {}",
                    to_hex(&session.login_iv)
                ));
                logger.line(format!(
                    "Rapture World AES hashed iv: {}",
                    to_hex(&world_iv)
                ));

                aes_recv = Some(AesCtr::new(&session.key, &world_iv));
                aes_send = Some(AesCtr::new(&session.key, &world_iv));

                if let Some(send) = aes_send.as_mut() {
                    send_encrypted(
                        &mut stream,
                        send,
                        use_expanded_send_key,
                        build_world_handshake()?,
                        "0xA301 handshake ok",
                        logger,
                    )?;
                    if selected_faction != FACTION_NONE {
                        send_character_list(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            &active_characters,
                            logger,
                        )?;
                    }
                    send_encrypted(
                        &mut stream,
                        send,
                        use_expanded_send_key,
                        build_account_faction(selected_faction, max_mode)?,
                        "0x0109 account faction",
                        logger,
                    )?;
                }
            } else if let Some(recv) = aes_recv.as_mut() {
                let decrypted_payload = recv.transform(&bytes[2..]);
                if LOG_RAW_PACKETS {
                    logger.line(format!(
                        "Rapture World decrypted payload: {}",
                        to_hex(&decrypted_payload)
                    ));
                }

                if decrypted_payload.len() < 2 {
                    continue;
                }

                let decrypted_opcode = read_le_u16(&decrypted_payload[0..2])?;
                if decrypted_opcode != Opcode::CharacterMove as u16 || LOG_MOVEMENT_PACKETS {
                    logger.line(format!(
                        "Rapture World decrypted opcode=0x{decrypted_opcode:04X}"
                    ));
                }

                // While dead, ignore world-interaction input (movement,
                // attacks, skills, item/vehicle use) until the player revives
                // via 0x0553 ResurrectRequest (handled below, never gated,
                // which clears `player_dead`). The client is expected to stop
                // sending the gated packets once it processes the death
                // packets, but a few in-flight ones can still arrive, and a
                // modified client shouldn't be able to act from the grave.
                if player_dead
                    && matches!(
                        decrypted_opcode,
                        x if x == Opcode::CharacterMove as u16
                            || x == Opcode::CharacterMobAutoAttack as u16
                            || x == Opcode::SkillUse as u16
                            || x == Opcode::SkillUseNoTarget as u16
                            || x == Opcode::UseItem as u16
                            || x == Opcode::ChangeAppearance as u16
                            || x == Opcode::UseVehicle as u16,
                    )
                {
                    logger.line(format!(
                        "Rapture World ignored opcode=0x{decrypted_opcode:04X} while dead character_id={}",
                        active_character.as_ref().map_or(0, |c| c.id)
                    ));
                    continue;
                }

                if decrypted_opcode == Opcode::AccountFaction as u16 && decrypted_payload.len() >= 3
                {
                    let requested_faction = decrypted_payload[2];
                    if !is_valid_account_faction(requested_faction) {
                        logger.line(format!(
                            "Rapture World ignored invalid account faction={requested_faction}"
                        ));
                        continue;
                    }

                    selected_faction = requested_faction;
                    logger.line(format!(
                        "Rapture World account faction selected={selected_faction}"
                    ));
                    if account_id != 0 {
                        db::update_account_faction(&mut db_conn, account_id, selected_faction)?;
                    }

                    if let Some(send) = aes_send.as_mut() {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            build_account_faction(selected_faction, max_mode)?,
                            "0x0109 account faction selected",
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::CreateCharacter as u16
                    && decrypted_payload.len() >= 10
                {
                    let new_character = NewCharacter {
                        slot: decrypted_payload[2],
                        race: decrypted_payload[3],
                        mode: decrypted_payload[4],
                        hair: decrypted_payload[5],
                        face: decrypted_payload[6],
                        height: decrypted_payload[7],
                        character_class: decrypted_payload[8],
                        gender: decrypted_payload[9],
                        name: read_fixed_ascii(&decrypted_payload[10..]),
                        map_id: START_MAP_ID,
                        pos_x: SPAWN_X,
                        pos_y: SPAWN_Y,
                        pos_z: SPAWN_Z,
                    };

                    logger.line(format!(
                    "Rapture World create character slot={} race={} mode={} class={} gender={} name='{}'",
                    new_character.slot,
                    new_character.race,
                    new_character.mode,
                    new_character.character_class,
                    new_character.gender,
                    new_character.name
                ));

                    if new_character.slot >= MAX_CHARACTER_SLOTS {
                        logger.line(format!(
                            "Rapture World rejected create character: slot={} out of range (max {MAX_CHARACTER_SLOTS})",
                            new_character.slot
                        ));
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_create_character_result(false)?,
                                "0x0102 create character rejected (slot out of range)",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if db::get_character_by_slot(&mut db_conn, account_id, new_character.slot)?
                        .is_some()
                    {
                        logger.line(format!(
                            "Rapture World rejected create character: slot={} already occupied",
                            new_character.slot
                        ));
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_create_character_result(false)?,
                                "0x0102 create character rejected (slot occupied)",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    // Reject a duplicate name. This client has no explicit
                    // name-check packet (unlike EP8/Imgeneus) -- the create
                    // simply fails with result 1, which the client shows as an
                    // in-game "name already in use" notice (confirmed via
                    // capture: `02 01 01`).
                    if db::character_name_taken(&mut db_conn, &new_character.name)? {
                        logger.line(format!(
                            "Rapture World rejected create character: name '{}' already in use",
                            new_character.name
                        ));
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_create_character_result(false)?,
                                "0x0102 create character rejected (name in use)",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    active_character = Some(db::create_character(
                        &mut db_conn,
                        account_id,
                        &new_character,
                    )?);
                    entity_visibility = EntityVisibility::default();
                    active_items = if let Some(character) = active_character.as_ref() {
                        db::get_character_items(&mut db_conn, character.id)?
                    } else {
                        Vec::new()
                    };
                    active_skills.clear();
                    active_quick_slots.clear();
                    item_cooldowns.clear();
                    skill_cooldowns.clear();
                    logger.line(format!(
                        "Rapture World loaded {} character items",
                        active_items.len()
                    ));

                    // Refresh the full character-select list so the new
                    // character (and every other existing character on the
                    // account) shows up, instead of only the one just
                    // created.
                    active_characters = db::get_characters_for_account(&mut db_conn, account_id)?
                        .into_iter()
                        .map(|character| {
                            let items = db::get_character_items(&mut db_conn, character.id)?;
                            Ok((character, items))
                        })
                        .collect::<Result<Vec<_>>>()?;

                    if let Some(send) = aes_send.as_mut() {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            build_create_character_result(true)?,
                            "0x0102 create character ok",
                            logger,
                        )?;
                        send_character_list(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            &active_characters,
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::DeleteCharacter as u16
                    && decrypted_payload.len() >= 6
                {
                    // Soft-delete into the 2h pending-deletion window. Purely
                    // a selection-screen operation (no active character to
                    // touch): the character stays listed but flagged, and the
                    // client greys it out from the ok result alone -- the real
                    // server sends no re-list here (per capture), so neither do
                    // we.
                    let character_id = read_le_u32(&decrypted_payload[2..6])?;
                    let ok = db::set_character_pending_delete(&mut db_conn, account_id, character_id)?;
                    logger.line(format!("Rapture World delete character id={character_id} ok={ok}"));
                    if let Some(send) = aes_send.as_mut() {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            build_delete_character_result(ok, character_id)?,
                            "0x0103 delete character",
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::RestoreCharacter as u16
                    && decrypted_payload.len() >= 6
                {
                    // Restore from the pending-deletion window (fails if the
                    // 2h window already elapsed -- the DB update matches no row
                    // and `ok` is false).
                    let character_id = read_le_u32(&decrypted_payload[2..6])?;
                    let ok = db::restore_character(&mut db_conn, account_id, character_id)?;
                    logger.line(format!("Rapture World restore character id={character_id} ok={ok}"));
                    if let Some(send) = aes_send.as_mut() {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            build_restore_character_result(ok, character_id)?,
                            "0x010F restore character",
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::RenameCharacter as u16
                    && decrypted_payload.len() >= 6
                {
                    // Commit a forced rename at login. The name field is a
                    // null-padded fixed string after the charId.
                    let character_id = read_le_u32(&decrypted_payload[2..6])?;
                    let new_name = read_fixed_ascii(&decrypted_payload[6..]);
                    // Reject an empty name or one already in use (active or
                    // reserved by a pending-delete character). rename_character
                    // additionally enforces account ownership + that the rename
                    // was actually pending, so a client can't rename for free.
                    let ok = if new_name.trim().is_empty()
                        || db::character_name_taken(&mut db_conn, &new_name)?
                    {
                        false
                    } else {
                        db::rename_character(&mut db_conn, account_id, character_id, &new_name)?
                    };
                    logger.line(format!(
                        "Rapture World rename character id={character_id} name='{new_name}' ok={ok}"
                    ));
                    if let Some(send) = aes_send.as_mut() {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            build_rename_character_result(ok)?,
                            "0x010E rename character",
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::SelectCharacter as u16
                    && decrypted_payload.len() >= 6
                {
                    let character_id = read_le_u32(&decrypted_payload[2..6])?;
                    logger.line(format!("Rapture World select character id={character_id}"));
                    active_character =
                        db::get_character_by_id(&mut db_conn, account_id, character_id)?;
                    entity_visibility = EntityVisibility::default();
                    if let Some(character) = active_character.as_ref() {
                        set_connection_tag(format!("conn{connection_id}/{}", character.name));
                    }
                    active_items = if active_character.is_some() {
                        db::get_character_items(&mut db_conn, character_id)?
                    } else {
                        Vec::new()
                    };
                    active_skills = if active_character.is_some() {
                        db::get_character_skills(&mut db_conn, character_id)?
                    } else {
                        Vec::new()
                    };
                    // Rebuild the in-memory cooldown map from whatever's
                    // still remaining in the DB, so a skill cast in an
                    // earlier session still gates casting after a relog
                    // instead of coming back fully ready.
                    skill_cooldowns.clear();
                    for skill in &active_skills {
                        if skill.delay > 0 {
                            skill_cooldowns.insert(
                                skill.skill_id,
                                Instant::now() + Duration::from_secs(skill.delay as u64),
                            );
                        }
                    }
                    // Restores buffs left running from an earlier session --
                    // frozen while offline (unlike skill cooldowns), so this
                    // just resumes each one's remaining seconds from where
                    // it was snapshotted at the last logout/disconnect.
                    active_buffs = if active_character.is_some() {
                        restore_active_buffs(&mut db_conn, character_id, logger)?
                    } else {
                        Vec::new()
                    };
                    active_quick_slots = if active_character.is_some() {
                        db::get_character_quick_slots(&mut db_conn, character_id)?
                    } else {
                        Vec::new()
                    };
                    // Ephemeral, not persisted -- always starts unmounted on
                    // select, same as pending_cast's reset-on-relog scoping.
                    mounted = false;
                    active_stored_items = match active_character.as_ref() {
                        Some(character) => db::get_stored_items(&mut db_conn, character.account_id)?,
                        None => Vec::new(),
                    };
                    logger.line(format!(
                        "Rapture World loaded {} warehouse items",
                        active_stored_items.len()
                    ));
                    active_account_points = match active_character.as_ref() {
                        Some(character) => db::get_account_by_id(&mut db_conn, character.account_id)?
                            .map(|account| account.point)
                            .unwrap_or(0),
                        None => 0,
                    };
                    item_cooldowns.clear();
                    logger.line(format!(
                        "Rapture World loaded {} character items",
                        active_items.len()
                    ));
                    logger.line(format!(
                        "Rapture World loaded {} character skills",
                        active_skills.len()
                    ));
                    logger.line(format!(
                        "Rapture World loaded {} quick slots",
                        active_quick_slots.len()
                    ));
                    let equipment_stats = equipment_stats_with_buffs(
                        db::get_equipment_stats(&mut db_conn, &active_items)?,
                        &active_buffs,
                    );
                    let character_stats =
                        CharacterStats::from_character(active_character.as_ref(), equipment_stats);
                    logger.line(format!(
                    "Rapture World calculated equipment stats hp={} mp={} sp={} str={} dex={} rec={} int={} wis={} luc={} atk={}-{} magic={}-{} def={} resist={}",
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

                    if let Some(send) = aes_send.as_mut() {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            build_select_character_result(true, character_id)?,
                            "0x0104 select character ok",
                            logger,
                        )?;
                        send_initial_character_state(
                            &mut stream,
                            send,
                            &mut use_expanded_send_key,
                            selected_faction,
                            active_character.as_ref(),
                            &character_stats,
                            &active_items,
                            &active_skills,
                            &active_quick_slots,
                            &active_stored_items,
                            active_account_points,
                            &active_buffs,
                            logger,
                        )?;
                        // Item-mall gift box (0x2606) -- pushed with the rest
                        // of the initial state; the client has no request
                        // opcode for it. Delivered gifts sent while this
                        // account was offline show up here.
                        if let Some(character) = active_character.as_ref() {
                            let list = build_gift_box_list(&mut db_conn, character.account_id)?;
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                list,
                                "0x2606 gift box list",
                                logger,
                            )?;
                        }
                    }
                } else if decrypted_opcode == Opcode::GmCommandGetItem as u16 {
                    if !require_admin(
                        is_admin,
                        decrypted_opcode,
                        account_id,
                        &mut stream,
                        &mut aes_send,
                        use_expanded_send_key,
                        logger,
                    )? {
                        continue;
                    }
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected GM getitem without active character");
                        continue;
                    };
                    let character = character.clone();

                    let result = match parse_gm_getitem_command(&decrypted_payload) {
                        Ok((item_type, type_id, count)) => handle_getitem_request(
                            &mut db_conn,
                            &character,
                            &mut active_items,
                            item_type,
                            type_id,
                            count,
                            logger,
                        )?,
                        Err(error) => {
                            logger.line(format!("Rapture World rejected GM getitem: {error:?}"));
                            GetItemCommandResult {
                                add_item_packets: Vec::new(),
                                message: error.to_string(),
                                created_total: 0,
                            }
                        }
                    };

                    if let Some(send) = aes_send.as_mut() {
                        for (index, packet) in result.add_item_packets.into_iter().enumerate() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                packet,
                                &format!("0x0205 GM add item {}", index + 1),
                                logger,
                            )?;
                        }

                        let gm_result = if result.created_total > 0 {
                            build_gm_command_success()?
                        } else {
                            build_gm_command_error(Opcode::GmCommandGetItem as u16)?
                        };
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            gm_result,
                            "0xF501 GM command result",
                            logger,
                        )?;
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            build_chat_normal(character.id, &result.message)?,
                            "0x1101 GM command message",
                            logger,
                        )?;
                    }
                } else if is_supported_chat_opcode(decrypted_opcode) {
                    let message = match read_chat_message(&decrypted_payload) {
                        Ok(message) => message,
                        Err(error) => {
                            logger.line(format!("Rapture World rejected chat packet: {error:?}"));
                            continue;
                        }
                    };
                    logger.line(format!(
                        "Rapture World chat opcode=0x{decrypted_opcode:04X} message='{message}'"
                    ));

                    if let Some(command) = parse_cmove_command(&message) {
                        if !is_admin {
                            logger.line(format!(
                                "Rapture World rejected cmove command from non-admin account_id={account_id}"
                            ));
                            if let Some(send) = aes_send.as_mut() {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    build_chat_normal(
                                        active_character.as_ref().map_or(0, |c| c.id),
                                        "Admin only.",
                                    )?,
                                    "0x1101 admin-only rejection",
                                    logger,
                                )?;
                            }
                            continue;
                        }
                        let (character_id, response) = match command {
                            Ok((map_id, target)) => {
                                let Some(character) = active_character.as_mut() else {
                                    logger.line(
                                        "Rapture World rejected cmove without active character",
                                    );
                                    continue;
                                };
                                let response = execute_cmove(
                                    character,
                                    map_id,
                                    target,
                                    &connected_player,
                                    is_admin,
                                    selected_faction,
                                    &mut entity_visibility,
                                    &mut db_conn,
                                    &mut stream,
                                    &mut aes_send,
                                    use_expanded_send_key,
                                    &mut position_dirty,
                                    &mut last_position_save,
                                    logger,
                                )?;
                                (character.id, response)
                            }
                            Err(error) => {
                                let Some(character) = active_character.as_ref() else {
                                    logger.line(
                                        "Rapture World rejected cmove command without active character",
                                    );
                                    continue;
                                };
                                (character.id, error.to_string())
                            }
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character_id, &response)?,
                                "0x1101 cmove command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_bmove_command(&message) {
                        if !is_admin {
                            logger.line(format!(
                                "Rapture World rejected bmove command from non-admin account_id={account_id}"
                            ));
                            if let Some(send) = aes_send.as_mut() {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    build_chat_normal(
                                        active_character.as_ref().map_or(0, |c| c.id),
                                        "Admin only.",
                                    )?,
                                    "0x1101 admin-only rejection",
                                    logger,
                                )?;
                            }
                            continue;
                        }
                        let (character_id, response) = match command {
                            Ok((x, y, z)) => {
                                let Some(character) = active_character.as_mut() else {
                                    logger.line(
                                        "Rapture World rejected bmove without active character",
                                    );
                                    continue;
                                };
                                let response = execute_bmove(
                                    character,
                                    x,
                                    y,
                                    z,
                                    &connected_player,
                                    is_admin,
                                    selected_faction,
                                    &mut entity_visibility,
                                    &mut db_conn,
                                    &mut stream,
                                    &mut aes_send,
                                    use_expanded_send_key,
                                    &mut position_dirty,
                                    &mut last_position_save,
                                    logger,
                                )?;
                                (character.id, response)
                            }
                            Err(error) => {
                                let Some(character) = active_character.as_ref() else {
                                    logger.line(
                                        "Rapture World rejected bmove command without active character",
                                    );
                                    continue;
                                };
                                (character.id, error.to_string())
                            }
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character_id, &response)?,
                                "0x1101 bmove command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if parse_town_command(&message) {
                        let Some(character) = active_character.as_mut() else {
                            logger.line(
                                "Rapture World rejected town command without active character",
                            );
                            continue;
                        };

                        let (packets, response) = execute_town(
                            character,
                            &connected_player,
                            selected_faction,
                            &mut entity_visibility,
                            &mut db_conn,
                            &mut position_dirty,
                            &mut last_position_save,
                            logger,
                        )?;

                        if let Some(send) = aes_send.as_mut() {
                            for (packet, label) in packets {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    packet,
                                    label,
                                    logger,
                                )?;
                            }
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 town command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_mmake_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let character = active_character.as_ref().unwrap().clone();

                        let response = match command {
                            Ok((mob_id, count, radius)) => {
                                execute_mmake(&character, mob_id, count, radius, &mut db_conn, logger)?
                            }
                            Err(error) => error.to_string(),
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 mmake command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_mera_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let character = active_character.as_ref().unwrap().clone();

                        let response = match command {
                            Ok(radius) => execute_mera(&character, radius, logger)?,
                            Err(error) => error.to_string(),
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 mera command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_nmake_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let character = active_character.as_ref().unwrap().clone();

                        let response = match command {
                            Ok((npc_type, type_id)) => {
                                execute_nmake(&character, npc_type, type_id, logger)?
                            }
                            Err(error) => error.to_string(),
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 nmake command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_nera_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let character = active_character.as_ref().unwrap().clone();

                        let response = match command {
                            Ok(radius) => execute_nera(&character, radius, logger)?,
                            Err(error) => error.to_string(),
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 nera command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_mobreload_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let character = active_character.as_ref().unwrap().clone();

                        let response = match command {
                            Ok(map_id) => {
                                execute_mobreload(&character, map_id, logger, &logger_arc)?
                            }
                            Err(error) => error.to_string(),
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 mobreload command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_npcreload_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let character = active_character.as_ref().unwrap().clone();

                        let response = match command {
                            Ok(map_id) => {
                                execute_npcreload(&character, map_id, logger, &logger_arc)?
                            }
                            Err(error) => error.to_string(),
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 npcreload command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_char_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let character = active_character.as_ref().unwrap().clone();

                        let response = match command {
                            Ok(visible) => execute_char_visibility(
                                &character,
                                &connected_player,
                                visible,
                                logger,
                            )?,
                            Err(error) => error.to_string(),
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 char command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_set_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let character = active_character.as_ref().unwrap().clone();

                        let response = match command {
                            Ok((charname, attribute, amount)) => execute_set(
                                &character,
                                &mut active_character,
                                &connected_player,
                                &active_items,
                                &mut db_conn,
                                &mut stream,
                                &mut aes_send,
                                use_expanded_send_key,
                                charname,
                                attribute,
                                amount,
                                &mut active_account_points,
                                logger,
                            )?,
                            Err(error) => error.to_string(),
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 set command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_amove_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let (character_id, response) = match command {
                            Ok(charname) => execute_amove(
                                &mut active_character,
                                charname,
                                &connected_player,
                                is_admin,
                                &mut entity_visibility,
                                &mut db_conn,
                                &mut stream,
                                &mut aes_send,
                                use_expanded_send_key,
                                &mut position_dirty,
                                &mut last_position_save,
                                logger,
                            )?,
                            Err(error) => {
                                let Some(character) = active_character.as_ref() else {
                                    continue;
                                };
                                (character.id, error.to_string())
                            }
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character_id, &response)?,
                                "0x1101 amove command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_asummon_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let character = active_character.as_ref().unwrap().clone();

                        // Queues the teleport on the target's own ConnectedPlayer
                        // instead of injecting packets/mutating their state from
                        // this thread: their own connection picks it up on its
                        // next loop iteration (see the pending-teleport check
                        // right after stream.read() below) and runs it through
                        // the exact same apply_teleport() a self /bmove or
                        // /cmove does, so their own active_character/
                        // entity_visibility locals stay correct -- no more stale
                        // state until relog (the limitation this used to have).
                        let response = match command {
                            Ok(charname) => execute_asummon(&character, &charname, logger)?,
                            Err(error) => error.to_string(),
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 asummon command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_iclear_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let character = active_character.as_ref().unwrap().clone();

                        let response = match command {
                            Ok(charname) => execute_iclear(
                                &character,
                                &connected_player,
                                &mut active_items,
                                &mut db_conn,
                                &mut stream,
                                &mut aes_send,
                                use_expanded_send_key,
                                charname,
                                logger,
                            )?,
                            Err(error) => error.to_string(),
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 iclear command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_eclear_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let character = active_character.as_ref().unwrap().clone();

                        let response = match command {
                            Ok(charname) => execute_eclear(
                                &character,
                                &connected_player,
                                is_admin,
                                &mut active_items,
                                &mut db_conn,
                                &mut stream,
                                &mut aes_send,
                                use_expanded_send_key,
                                charname,
                                logger,
                            )?,
                            Err(error) => error.to_string(),
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 eclear command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(command) = parse_cure_command(&message) {
                        if !require_gm_command(
                            is_admin,
                            account_id,
                            &active_character,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )? {
                            continue;
                        }
                        let character = active_character.as_ref().unwrap().clone();

                        let response = match command {
                            Ok(charname) => execute_cure(
                                &character,
                                &mut active_character,
                                &connected_player,
                                &active_items,
                                &mut db_conn,
                                &mut stream,
                                &mut aes_send,
                                use_expanded_send_key,
                                charname,
                                logger,
                            )?,
                            Err(error) => error.to_string(),
                        };

                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &response)?,
                                "0x1101 cure command result",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected chat without active character");
                        continue;
                    };
                    let character = character.clone();

                    if parse_getitem_command(&message).is_some() && !is_admin {
                        logger.line(format!(
                            "Rapture World rejected getitem command from non-admin account_id={account_id}"
                        ));
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, "Admin only.")?,
                                "0x1101 admin-only rejection",
                                logger,
                            )?;
                        }
                        continue;
                    }

                    if let Some(result) = handle_getitem_command(
                        &mut db_conn,
                        &character,
                        &mut active_items,
                        &message,
                        logger,
                    )? {
                        if let Some(send) = aes_send.as_mut() {
                            for (index, packet) in result.add_item_packets.into_iter().enumerate() {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    packet,
                                    &format!("0x0205 add item {}", index + 1),
                                    logger,
                                )?;
                            }
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_chat_normal(character.id, &result.message)?,
                                "0x1101 command result",
                                logger,
                            )?;
                        }
                    } else {
                        handle_public_chat(
                            decrypted_opcode,
                            &character,
                            &message,
                            is_admin,
                            account_id,
                            &mut last_chat_at,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::UseItem as u16 {
                    let Some(character) = active_character.as_mut() else {
                        logger.line("Rapture World rejected use item without active character");
                        continue;
                    };

                    if let Some(packets) = handle_use_item(
                        &mut db_conn,
                        character,
                        &mut active_items,
                        &mut item_cooldowns,
                        &decrypted_payload,
                        logger,
                    )? {
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                packets.use_result,
                                "0x050A use item",
                                logger,
                            )?;
                            if let Some(remove_item) = packets.remove_item {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    remove_item,
                                    "0x0206 remove item",
                                    logger,
                                )?;
                            }
                            if let Some(current_hitpoints) = packets.current_hitpoints {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    current_hitpoints,
                                    "0x0521 current hp/mp/sp",
                                    logger,
                                )?;
                            }
                            // Extra ordered packets (reset-stats stone: 0x0214
                            // + max-energy + current-hp).
                            for (packet, label) in packets.extra_before {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    packet,
                                    label,
                                    logger,
                                )?;
                            }
                        }
                    }
                } else if decrypted_opcode == Opcode::ChangeAppearance as u16 {
                    let Some(character) = active_character.as_mut() else {
                        logger.line("Rapture World rejected change appearance without active character");
                        continue;
                    };

                    if let Some(packets) = handle_change_appearance(
                        &mut db_conn,
                        character,
                        &mut active_items,
                        &decrypted_payload,
                        logger,
                    )? {
                        // Refresh the live snapshot so a later-arriving viewer's
                        // 0x0303 shape reflects the new look/sex.
                        if let Some(player) = connected_player.as_ref() {
                            player.update_character(character.clone());
                        }
                        // Item consume goes to self only.
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                packets.use_result,
                                "0x050A use item (change appearance)",
                                logger,
                            )?;
                        }
                        // Appearance broadcast reaches self + everyone nearby so
                        // the model updates live (self is included in the loop,
                        // matching the motion-broadcast pattern).
                        let my_gm_visible =
                            connected_player.as_ref().is_some_and(|p| p.gm_visible());
                        for player in players_on_map_near(
                            character.map_id,
                            character.pos_x,
                            character.pos_z,
                            VIEWPORT_RADIUS,
                        ) {
                            if player.character_id != character.id
                                && !can_see_character(player.is_admin, is_admin, my_gm_visible)
                            {
                                continue;
                            }
                            player.send_packet(&packets.broadcast, "0x0226 change appearance", logger)?;
                        }
                    }
                } else if decrypted_opcode == Opcode::EnchantAdd as u16 {
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected enchant add without active character");
                        continue;
                    };
                    let character = character.clone();

                    if let Some(outcome) = handle_enchant_add(
                        &mut db_conn,
                        &character,
                        &mut active_items,
                        &decrypted_payload,
                        logger,
                    )? {
                        // Sync the deducted-money field on the working
                        // character copy so later packets in this same
                        // connection (e.g. a subsequent enchant) see the
                        // right balance.
                        if let Some(active) = active_character.as_mut() {
                            active.money = outcome.new_money;
                            if let Some(player) = connected_player.as_ref() {
                                player.update_character(active.clone());
                            }
                        }
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                outcome.response,
                                "0x0805 enchant add result",
                                logger,
                            )?;
                            if outcome.item_equipped {
                                let slot = decrypted_payload.get(8).copied().unwrap_or(0);
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    build_character_equipment(&character, &active_items, slot)?,
                                    &format!("0x0507 character equipment slot {slot} (enchant)"),
                                    logger,
                                )?;
                                if let Some(player) = connected_player.as_ref() {
                                    player.update_items(active_items.clone());
                                    let equipment_packet = player.equipment_packet(slot)?;
                                    for other in players_on_map_excluding_near(
                                        character.map_id,
                                        character.id,
                                        character.pos_x,
                                        character.pos_z,
                                        VIEWPORT_RADIUS,
                                    ) {
                                        if !can_see_character(other.is_admin, is_admin, player.gm_visible()) {
                                            continue;
                                        }
                                        other.send_packet(
                                            &equipment_packet,
                                            &format!("0x0507 character equipment slot {slot} (enchant broadcast)"),
                                            logger,
                                        )?;
                                    }
                                }
                            }
                        }
                    }
                } else if decrypted_opcode == Opcode::GemAddPossibility as u16 {
                    if let Some(response) = handle_gem_add_possibility(&decrypted_payload, logger)?
                        && let Some(send) = aes_send.as_mut()
                    {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            response,
                            "0x0809 gem add possibility",
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::GemRemovePossibility as u16 {
                    if let Some(response) = handle_gem_remove_possibility(&decrypted_payload, logger)?
                        && let Some(send) = aes_send.as_mut()
                    {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            response,
                            "0x080A gem remove possibility",
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::GemAdd as u16 {
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected gem add without active character");
                        continue;
                    };
                    let character = character.clone();

                    // Effective stats BEFORE the link, to diff afterwards --
                    // mirrors ItemCompose's confirmed-safe pattern (a full
                    // stat-bundle resend crashed the client there; selective
                    // 0x050B updates for changed pools didn't). Not
                    // independently confirmed for gems specifically (no
                    // capture of the client's post-link expectations), but
                    // it's the same "item mutation changes live stats" class
                    // of operation, so reusing the known-safe approach here
                    // is the lower-risk choice.
                    let old_stats = CharacterStats::from_character(
                        active_character.as_ref(),
                        equipment_stats_with_buffs(
                            db::get_equipment_stats(&mut db_conn, &active_items)?,
                            &active_buffs,
                        ),
                    );

                    if let Some(outcome) = handle_gem_add(
                        &mut db_conn,
                        &character,
                        &mut active_items,
                        &decrypted_payload,
                        logger,
                    )? {
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                outcome.response,
                                "0x0801 gem add result",
                                logger,
                            )?;
                            if outcome.item_equipped {
                                let new_stats = CharacterStats::from_character(
                                    active_character.as_ref(),
                                    equipment_stats_with_buffs(
                                        db::get_equipment_stats(&mut db_conn, &active_items)?,
                                        &active_buffs,
                                    ),
                                );
                                for (hitpoint_type, old_max, new_max) in [
                                    (0u8, old_stats.max_hp, new_stats.max_hp),
                                    (1u8, old_stats.max_sp, new_stats.max_sp),
                                    (2u8, old_stats.max_mp, new_stats.max_mp),
                                ] {
                                    if old_max != new_max {
                                        send_encrypted(
                                            &mut stream,
                                            send,
                                            use_expanded_send_key,
                                            build_max_energy(active_character.as_ref(), hitpoint_type, new_max)?,
                                            "0x050B max energy (gem add)",
                                            logger,
                                        )?;
                                    }
                                }
                            }
                        }
                        if let Some(player) = connected_player.as_ref() {
                            player.update_items(active_items.clone());
                        }
                    }
                } else if decrypted_opcode == Opcode::GemRemove as u16 {
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected gem remove without active character");
                        continue;
                    };
                    let character = character.clone();

                    let old_stats = CharacterStats::from_character(
                        active_character.as_ref(),
                        equipment_stats_with_buffs(
                            db::get_equipment_stats(&mut db_conn, &active_items)?,
                            &active_buffs,
                        ),
                    );

                    if let Some(outcome) = handle_gem_remove(
                        &mut db_conn,
                        &character,
                        &mut active_items,
                        &decrypted_payload,
                        logger,
                    )? {
                        if let Some(send) = aes_send.as_mut() {
                            for (index, packet) in outcome.add_item_packets.into_iter().enumerate() {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    packet,
                                    &format!("0x0205 gem remove add item {}", index + 1),
                                    logger,
                                )?;
                            }
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                outcome.response,
                                "0x0802 gem remove result",
                                logger,
                            )?;
                            if outcome.item_equipped {
                                let new_stats = CharacterStats::from_character(
                                    active_character.as_ref(),
                                    equipment_stats_with_buffs(
                                        db::get_equipment_stats(&mut db_conn, &active_items)?,
                                        &active_buffs,
                                    ),
                                );
                                for (hitpoint_type, old_max, new_max) in [
                                    (0u8, old_stats.max_hp, new_stats.max_hp),
                                    (1u8, old_stats.max_sp, new_stats.max_sp),
                                    (2u8, old_stats.max_mp, new_stats.max_mp),
                                ] {
                                    if old_max != new_max {
                                        send_encrypted(
                                            &mut stream,
                                            send,
                                            use_expanded_send_key,
                                            build_max_energy(active_character.as_ref(), hitpoint_type, new_max)?,
                                            "0x050B max energy (gem remove)",
                                            logger,
                                        )?;
                                    }
                                }
                            }
                        }
                        if let Some(player) = connected_player.as_ref() {
                            player.update_items(active_items.clone());
                        }
                    }
                } else if decrypted_opcode == Opcode::ItemCompose as u16 {
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected item compose without active character");
                        continue;
                    };
                    let character_id = character.id;

                    // Max pools BEFORE the reroll, to diff afterwards -- the
                    // real server (per capture) sends one 0x050B max-energy
                    // update per pool that changed, NOT the full stat bundle
                    // (which crashed the client here).
                    let old_stats = CharacterStats::from_character(
                        active_character.as_ref(),
                        equipment_stats_with_buffs(
                            db::get_equipment_stats(&mut db_conn, &active_items)?,
                            &active_buffs,
                        ),
                    );

                    if let Some(outcome) = handle_item_compose(
                        &mut db_conn,
                        character_id,
                        &mut active_items,
                        &decrypted_payload,
                        logger,
                    )? {
                        if let Some(send) = aes_send.as_mut() {
                            // Confirmed sequence from a real capture:
                            // 0x050B (changed pools) -> 0x0806 -> 0x050A.
                            if outcome.item_equipped {
                                let new_stats = CharacterStats::from_character(
                                    active_character.as_ref(),
                                    equipment_stats_with_buffs(
                                        db::get_equipment_stats(&mut db_conn, &active_items)?,
                                        &active_buffs,
                                    ),
                                );
                                // Same hitpoint_type mapping as statpoints.rs:
                                // 0 = hp, 1 = sp, 2 = mp.
                                for (hitpoint_type, old_max, new_max) in [
                                    (0u8, old_stats.max_hp, new_stats.max_hp),
                                    (1u8, old_stats.max_sp, new_stats.max_sp),
                                    (2u8, old_stats.max_mp, new_stats.max_mp),
                                ] {
                                    if old_max != new_max {
                                        send_encrypted(
                                            &mut stream,
                                            send,
                                            use_expanded_send_key,
                                            build_max_energy(
                                                active_character.as_ref(),
                                                hitpoint_type,
                                                new_max,
                                            )?,
                                            "0x050B max energy (compose)",
                                            logger,
                                        )?;
                                    }
                                }
                            }
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                outcome.response,
                                "0x0806 item compose result",
                                logger,
                            )?;
                            if let Some(rune_consume) = outcome.rune_consume {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    rune_consume,
                                    "0x050A rune consume (compose)",
                                    logger,
                                )?;
                            }
                        }
                    }
                } else if decrypted_opcode == Opcode::RemoveItem as u16 {
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected drop item without active character");
                        continue;
                    };
                    let character = character.clone();

                    if let Some(packets) = handle_drop_item(
                        &mut db_conn,
                        &character,
                        &mut active_items,
                        &decrypted_payload,
                        logger,
                    )? {
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                packets.remove_item,
                                "0x0206 drop remove item",
                                logger,
                            )?;
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                packets.map_add_item.clone(),
                                "0x0401 drop map add item",
                                logger,
                            )?;

                            // Other players on the map couldn't see dropped
                            // items at all before this: map_items used to be
                            // private per-connection state.
                            for other in players_on_map_excluding_near(
                                character.map_id,
                                character.id,
                                character.pos_x,
                                character.pos_z,
                                VIEWPORT_RADIUS,
                            ) {
                                other.send_packet(
                                    &packets.map_add_item,
                                    "0x0401 drop map add item (broadcast)",
                                    logger,
                                )?;
                            }

                            if let Some(slot) = packets.equipment_slot {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    build_character_equipment(&character, &active_items, slot)?,
                                    &format!("0x0507 character equipment slot {}", slot),
                                    logger,
                                )?;
                            }

                            if packets.equipment_stats_changed {
                                let equipment_stats =
                                    db::get_equipment_stats(&mut db_conn, &active_items)?;
                                let character_stats = CharacterStats::from_character(
                                    Some(&character),
                                    equipment_stats,
                                );
                                send_character_stat_packets(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    Some(&character),
                                    &character_stats,
                                    logger,
                                )?;
                            }
                        }
                    }
                } else if decrypted_opcode == Opcode::AddItem as u16 {
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected pickup item without active character");
                        continue;
                    };
                    let character = character.clone();

                    if let Some(packets) = handle_pickup_item(
                        &mut db_conn,
                        &character,
                        &mut active_items,
                        &decrypted_payload,
                        logger,
                    )? {
                        if let Some(new_money) = packets.new_money {
                            if let Some(active) = active_character.as_mut() {
                                active.money = new_money;
                                if let Some(player) = connected_player.as_ref() {
                                    player.update_character(active.clone());
                                }
                            }
                        }
                        if let Some(send) = aes_send.as_mut() {
                            for (index, packet) in packets.add_item_packets.into_iter().enumerate()
                            {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    packet,
                                    &format!("0x0205 pickup add item {}", index + 1),
                                    logger,
                                )?;
                            }
                            if let Some(money_update) = packets.money_update {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    money_update,
                                    "0x0213 pickup money update",
                                    logger,
                                )?;
                            }
                            let others = players_on_map_excluding_near(
                                character.map_id,
                                character.id,
                                character.pos_x,
                                character.pos_z,
                                VIEWPORT_RADIUS,
                            );
                            if let Some(map_remove_item) = packets.map_remove_item {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    map_remove_item.clone(),
                                    "0x0402 pickup map remove item",
                                    logger,
                                )?;
                                // Other players must also stop seeing the
                                // item once it's been picked up.
                                for other in &others {
                                    other.send_packet(
                                        &map_remove_item,
                                        "0x0402 pickup map remove item (broadcast)",
                                        logger,
                                    )?;
                                }
                            }
                            if let Some(map_add_item) = packets.map_add_item {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    map_add_item.clone(),
                                    "0x0401 pickup map add remaining item",
                                    logger,
                                )?;
                                // Partial pickup: the reduced stack still on
                                // the ground must update for everyone too.
                                for other in &others {
                                    other.send_packet(
                                        &map_add_item,
                                        "0x0401 pickup map add remaining item (broadcast)",
                                        logger,
                                    )?;
                                }
                            }
                        }
                    }
                } else if decrypted_opcode == Opcode::ItemBagToBag as u16 {
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected item move without active character");
                        continue;
                    };
                    let character = character.clone();
                    let involves_warehouse = decrypted_payload.len() >= 6
                        && (decrypted_payload[2] == WAREHOUSE_BAG
                            || decrypted_payload[4] == WAREHOUSE_BAG);

                    if involves_warehouse {
                        let src_bag = decrypted_payload[2];
                        let src_slot = decrypted_payload[3];
                        let dest_bag = decrypted_payload[4];
                        let dest_slot = decrypted_payload[5];
                        let packet = handle_warehouse_move(
                            &mut db_conn,
                            character.id,
                            character.account_id,
                            character.money,
                            &mut active_items,
                            &mut active_stored_items,
                            src_bag,
                            src_slot,
                            dest_bag,
                            dest_slot,
                            logger,
                        )?;
                        if let Some(packet) = packet {
                            if let Some(send) = aes_send.as_mut() {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    packet,
                                    "0x0204 warehouse item move",
                                    logger,
                                )?;
                            }
                        }
                    } else {
                        handle_item_bag_to_bag_request(
                            &mut db_conn,
                            &character,
                            &connected_player,
                            is_admin,
                            &mut active_items,
                            &decrypted_payload,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::ShopSell as u16 {
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected shop sell without active character");
                        continue;
                    };
                    let character_id = character.id;
                    let current_money = character.money;
                    if let Some((packet, new_money)) = handle_shop_sell(
                        &mut db_conn,
                        character_id,
                        current_money,
                        &mut active_items,
                        &decrypted_payload,
                        logger,
                    )? {
                        if let Some(active) = active_character.as_mut() {
                            active.money = new_money;
                            if let Some(player) = connected_player.as_ref() {
                                player.update_character(active.clone());
                            }
                        }
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                packet,
                                "0x0703 shop sell",
                                logger,
                            )?;
                        }
                    }
                } else if decrypted_opcode == Opcode::ExchangeRequest as u16
                    && decrypted_payload.len() >= 6
                {
                    if let Some(player) = connected_player.as_ref() {
                        let target_id = read_le_u32(&decrypted_payload[2..6])?;
                        handle_request(player, target_id, logger)?;
                    }
                } else if decrypted_opcode == Opcode::ExchangeAnswer as u16
                    && decrypted_payload.len() >= 3
                {
                    if let Some(player) = connected_player.as_ref() {
                        // The incoming byte is "declined", not "accepted"
                        // (confirmed empirically: every real acceptance
                        // arrived as 0, matching Imgeneus's IsDeclined
                        // semantics, the opposite of the OUTGOING
                        // Accepted/Rejected result byte).
                        handle_answer(player, decrypted_payload[2] == 0, logger)?;
                    }
                } else if decrypted_opcode == Opcode::ExchangeReady as u16
                    && decrypted_payload.len() >= 3
                {
                    if let Some(player) = connected_player.as_ref() {
                        let raw_value = decrypted_payload[2];
                        if let Some(outcome) =
                            handle_ready(&mut db_conn, player, raw_value, logger)?
                        {
                            if let Some(character) = active_character.as_mut() {
                                active_items = outcome.items;
                                character.money = outcome.money;
                                player.update_items(active_items.clone());
                                player.update_character(character.clone());
                            }
                            if let Some(send) = aes_send.as_mut() {
                                for packet in outcome.packets {
                                    send_encrypted(
                                        &mut stream,
                                        send,
                                        use_expanded_send_key,
                                        packet,
                                        "0x0206/0x0205 trade result item",
                                        logger,
                                    )?;
                                }
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    build_set_money(outcome.money)?,
                                    "0x0213 trade result money",
                                    logger,
                                )?;
                            }
                        }
                    }
                } else if decrypted_opcode == Opcode::ExchangeAdd as u16
                    && decrypted_payload.len() >= 6
                {
                    if let Some(player) = connected_player.as_ref() {
                        handle_add_item(
                            player,
                            &active_items,
                            decrypted_payload[2],
                            decrypted_payload[3],
                            decrypted_payload[4],
                            decrypted_payload[5],
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::ExchangeRemove as u16
                    && decrypted_payload.len() >= 3
                {
                    if let Some(player) = connected_player.as_ref() {
                        handle_remove_item(player, decrypted_payload[2], logger)?;
                    }
                } else if decrypted_opcode == Opcode::ExchangeAddMoney as u16
                    && decrypted_payload.len() >= 6
                {
                    if let (Some(player), Some(character)) =
                        (connected_player.as_ref(), active_character.as_ref())
                    {
                        let requested = read_le_u32(&decrypted_payload[2..6])?;
                        handle_add_money(player, character.money, requested, logger)?;
                    }
                } else if decrypted_opcode == Opcode::ExchangeConfirm as u16
                    && decrypted_payload.len() >= 3
                {
                    if let Some(player) = connected_player.as_ref() {
                        let confirmed = decrypted_payload[2] != 0;
                        if let Some(outcome) =
                            handle_confirm(&mut db_conn, player, confirmed, logger)?
                        {
                            if let Some(character) = active_character.as_mut() {
                                active_items = outcome.items;
                                character.money = outcome.money;
                                player.update_items(active_items.clone());
                                player.update_character(character.clone());
                            }
                            if let Some(send) = aes_send.as_mut() {
                                for packet in outcome.packets {
                                    send_encrypted(
                                        &mut stream,
                                        send,
                                        use_expanded_send_key,
                                        packet,
                                        "0x0206/0x0205 trade result item",
                                        logger,
                                    )?;
                                }
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    build_set_money(outcome.money)?,
                                    "0x0213 trade result money",
                                    logger,
                                )?;
                            }
                        }
                    }
                } else if decrypted_opcode == Opcode::CharacterMove as u16 {
                    // Movement cancels an in-progress cast unconditionally
                    // (confirmed by Imgeneus's SkillCastingManager, which
                    // hooks the same move event) -- no distance threshold,
                    // any move packet interrupts.
                    if pending_cast.take().is_some() {
                        logger.line("Rapture World cancelled pending skill cast due to movement");
                    }
                    // Same for an in-progress auto-attack: without this, the
                    // 100ms read-timeout retry loop (see the `let Some(read)
                    // = read else` branch above) kept re-swinging forever
                    // regardless of the character having walked away, since
                    // nothing ever cleared `active_auto_attack` on move. A
                    // character that stays put is still meant to keep
                    // swinging (untouched), only actual movement cancels it.
                    if active_auto_attack.take().is_some() {
                        logger.line("Rapture World cancelled active auto attack due to movement");
                    }
                    handle_character_move(
                        &decrypted_payload,
                        &mut active_character,
                        &connected_player,
                        is_admin,
                        selected_faction,
                        &mut entity_visibility,
                        &mut db_conn,
                        &mut stream,
                        &mut aes_send,
                        use_expanded_send_key,
                        &mut position_dirty,
                        &mut last_position_save,
                        logger,
                    )?;
                } else if decrypted_opcode == Opcode::CharacterMotion as u16
                    && decrypted_payload.len() >= 3
                {
                    let motion = decrypted_payload[2];
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected motion without active character");
                        continue;
                    };
                    logger.line(format!(
                        "Rapture World character_id={} motion={motion}",
                        character.id
                    ));

                    // Sit is the only motion that persists as ongoing state
                    // (matches Imgeneus's MovementManager.Motion, which only
                    // ever keeps None or Sit); everything else (jump, etc.)
                    // is a one-shot animation relayed as-is with nothing
                    // cached for later viewers.
                    if let Some(player) = connected_player.as_ref() {
                        if motion == MOTION_SIT_DOWN {
                            player.set_sitting(true);
                            logger.line(format!(
                                "Rapture World character_id={} sitting=true",
                                character.id
                            ));
                        } else if motion == MOTION_STAND_UP {
                            player.set_sitting(false);
                            logger.line(format!(
                                "Rapture World character_id={} sitting=false",
                                character.id
                            ));
                        }
                    }

                    let motion_packet = build_character_motion(character.id, motion)?;
                    // Include self: the client may rely on the server's own
                    // echo to actually play the motion locally (excluding
                    // self broke the player's own ability to sit).
                    let my_gm_visible = connected_player.as_ref().is_some_and(|p| p.gm_visible());
                    for player in
                        players_on_map_near(character.map_id, character.pos_x, character.pos_z, VIEWPORT_RADIUS)
                    {
                        if player.character_id != character.id
                            && !can_see_character(player.is_admin, is_admin, my_gm_visible)
                        {
                            continue;
                        }
                        player.send_packet(&motion_packet, "0x0506 character motion", logger)?;
                    }
                } else if decrypted_opcode == Opcode::CharacterEnteredPortal as u16
                    && decrypted_payload.len() >= 3
                {
                    let portal_index = decrypted_payload[2];
                    handle_character_entered_portal(
                        portal_index,
                        &mut active_character,
                        &connected_player,
                        is_admin,
                        selected_faction,
                        &mut entity_visibility,
                        &mut db_conn,
                        &mut stream,
                        &mut aes_send,
                        use_expanded_send_key,
                        &mut position_dirty,
                        &mut last_position_save,
                        logger,
                    )?;
                } else if decrypted_opcode == Opcode::CharacterEnteredMap as u16 {
                    logger.line("Rapture World map loaded by client; sending character enter");

                    let current_map_items = active_character
                        .as_ref()
                        .map(|character| items_on_map(character.map_id))
                        .unwrap_or_default();
                    let current_map_entities = active_character
                        .as_ref()
                        .map(|character| {
                            entities_near(character.map_id, character.pos_x, character.pos_z)
                        })
                        .unwrap_or_default();
                    if let Some(send) = aes_send.as_mut() {
                        send_map_loaded_state(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            selected_faction,
                            active_character.as_ref(),
                            is_admin,
                            &active_items,
                            &current_map_items,
                            &current_map_entities,
                            logger,
                        )?;
                    }

                    if let Some(character) = active_character.as_ref() {
                        entity_visibility = visibility_from_entities(&current_map_entities);
                        logger.line(format!(
                            "Rapture World initialized entity visibility character_id={} map={} npcs={} mobs={}",
                            character.id,
                            character.map_id,
                            current_map_entities.npcs.len(),
                            current_map_entities.mobs.len()
                        ));
                        if let Some(existing) = connected_player.as_ref() {
                            existing.set_map(character.map_id);
                            existing.update_character(character.clone());
                            existing.update_items(active_items.clone());
                        } else {
                            let (player, guard) = register_player(
                                character.clone(),
                                active_items.clone(),
                                character.name.clone(),
                                is_admin,
                                selected_faction,
                                &stream,
                                Arc::clone(&logger_arc),
                            )?;
                            #[allow(unused_assignments)]
                            {
                                connected_player = Some(player);
                                chat_presence = Some(guard);
                            }
                        }
                        logger.line(format!(
                            "Rapture World registered character_id={} for chat/visibility delivery on map={}",
                            character.id, character.map_id
                        ));

                        // Mutual visibility: show every other player already
                        // on this map to us, and show us to them. The real
                        // client culls out-of-range characters natively, so
                        // no despawn packet is ever needed on our side.
                        let others = players_on_map_excluding_near(
                            character.map_id,
                            character.id,
                            character.pos_x,
                            character.pos_z,
                            VIEWPORT_RADIUS,
                        );
                        if let Some(send) = aes_send.as_mut() {
                            for other in &others {
                                if !can_see_character(is_admin, other.is_admin, other.gm_visible())
                                {
                                    continue;
                                }
                                logger.line(format!(
                                    "Rapture World introducing character_id={} to character_id={}, is_sitting={}",
                                    other.character_id, character.id, other.is_sitting()
                                ));
                                // 0x0201 must come first: it is what actually
                                // spawns the object (with position) in the
                                // client's world. 0x0303 alone only updates
                                // appearance for an object the client already
                                // knows about, so sending shape without this
                                // first left other players invisible.
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    other.entered_map_packet()?,
                                    "0x0201 other character entered map",
                                    logger,
                                )?;
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    other.shape_packet()?,
                                    "0x0303 other character shape",
                                    logger,
                                )?;
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    other.move_packet()?,
                                    "0x0501 other character move",
                                    logger,
                                )?;
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    other.overlay_packet()?,
                                    "0x0240 other character name color",
                                    logger,
                                )?;
                                if other.is_sitting() {
                                    send_encrypted(
                                        &mut stream,
                                        send,
                                        use_expanded_send_key,
                                        other.sit_motion_packet()?,
                                        "0x0506 other character sitting",
                                        logger,
                                    )?;
                                }
                            }
                        }
                        if let Some(player) = connected_player.as_ref() {
                            let my_entered_map = player.entered_map_packet()?;
                            let my_shape = player.shape_packet()?;
                            let my_move = player.move_packet()?;
                            let my_overlay = player.overlay_packet()?;
                            let my_sitting = player
                                .is_sitting()
                                .then(|| player.sit_motion_packet())
                                .transpose()?;
                            for other in &others {
                                if !can_see_character(other.is_admin, is_admin, player.gm_visible())
                                {
                                    continue;
                                }
                                other.send_packet(
                                    &my_entered_map,
                                    "0x0201 my character entered map",
                                    logger,
                                )?;
                                other.send_packet(
                                    &my_shape,
                                    "0x0303 my character shape",
                                    logger,
                                )?;
                                other.send_packet(&my_move, "0x0501 my character move", logger)?;
                                other.send_packet(
                                    &my_overlay,
                                    "0x0240 my character name color",
                                    logger,
                                )?;
                                if let Some(sit_packet) = &my_sitting {
                                    other.send_packet(
                                        sit_packet,
                                        "0x0506 my character sitting",
                                        logger,
                                    )?;
                                }
                            }
                        }
                        logger.line(format!(
                            "Rapture World exchanged visibility with {} other player(s) on map={}",
                            others.len(),
                            character.map_id
                        ));
                    }
                } else if decrypted_opcode == Opcode::LearnNewSkill as u16
                    && decrypted_payload.len() >= 5
                {
                    let skill_id = read_le_u16(&decrypted_payload[2..4])?;
                    let skill_level = decrypted_payload[4];
                    logger.line(format!(
                        "Rapture World learn skill request skill_id={skill_id} level={skill_level}"
                    ));
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected learn skill without active character");
                        continue;
                    };
                    let character = character.clone();
                    let (result_packet, new_skill_point) = handle_learn_new_skill(
                        &mut db_conn,
                        &character,
                        skill_id,
                        skill_level,
                        &mut active_skills,
                        logger,
                    )?;
                    // NOTE: do not resend 0x0105 CharacterDetails here to
                    // "fix" the stale skill-point display. Tried that once;
                    // it visibly corrupted HP/MP/SP/kills/etc. client-side
                    // until relog. 0x0105 is a once-only, character-select
                    // packet -- confirmed the hard way, not just a comment
                    // guess. Also confirmed via a real client screenshot
                    // that even the correctly-loaded StatPoint/SkillPoint
                    // values (10 / 0 in the DB) don't match what the client
                    // displays (198 / 150) at ANY point, including initial
                    // login -- so this isn't a "live update" gap, it's a
                    // wrong-field/wrong-formula bug in 0x0105 itself that
                    // needs a real packet capture to pin down before
                    // touching this again.
                    if let Some(new_skill_point) = new_skill_point {
                        if let Some(active) = active_character.as_mut() {
                            active.skill_point = new_skill_point;
                            if let Some(player) = connected_player.as_ref() {
                                player.update_character(active.clone());
                            }
                        }
                    }

                    if let Some(send) = aes_send.as_mut() {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            result_packet,
                            "0x0209 learn skill ok",
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::SetStatPoint as u16 {
                    let equipment_stats = db::get_equipment_stats(&mut db_conn, &active_items)?;
                    let Some(character) = active_character.as_mut() else {
                        logger.line("Rapture World rejected stat point apply without active character");
                        continue;
                    };
                    let outcome = handle_apply_stat_points(
                        &mut db_conn,
                        character,
                        equipment_stats,
                        &decrypted_payload,
                        logger,
                    )?;
                    if let Some(player) = connected_player.as_ref() {
                        player.update_character(character.clone());
                    }
                    if let Some(outcome) = outcome {
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                outcome.response,
                                "0x0208 stat point applied",
                                logger,
                            )?;
                            for (hitpoint_type, value) in outcome.max_energy_packets {
                                send_encrypted(
                                    &mut stream,
                                    send,
                                    use_expanded_send_key,
                                    build_max_energy(Some(&*character), hitpoint_type, value)?,
                                    "0x050B max energy update",
                                    logger,
                                )?;
                            }
                        }
                    }
                } else if decrypted_opcode == Opcode::CharacterSkillBar as u16 {
                    let Some(character) = active_character.as_ref() else {
                        logger
                            .line("Rapture World rejected skill bar save without active character");
                        continue;
                    };
                    active_quick_slots = parse_skill_bar(&decrypted_payload, logger)?;
                    db::save_character_quick_slots(
                        &mut db_conn,
                        character.id,
                        &active_quick_slots,
                    )?;
                    logger.line(format!(
                        "Rapture World saved {} quick slots",
                        active_quick_slots.len()
                    ));
                } else if decrypted_opcode == Opcode::AutoAttackStop as u16 {
                    active_auto_attack = None;
                    logger.line("Rapture World auto attack stop");
                } else if decrypted_opcode == Opcode::ResurrectRequest as u16 {
                    if player_dead {
                        if resurrect_character(
                            &mut active_character,
                            &connected_player,
                            &active_items,
                            is_admin,
                            selected_faction,
                            &mut entity_visibility,
                            &mut db_conn,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            &mut position_dirty,
                            &mut last_position_save,
                            logger,
                        )? {
                            player_dead = false;
                        }
                    } else {
                        logger.line("Rapture World ignored resurrect request while alive");
                    }
                } else if decrypted_opcode == Opcode::UseVehicle as u16 {
                    if let Some(character) = active_character.as_ref() {
                        handle_use_vehicle(
                            character.id,
                            character.map_id,
                            character.pos_x,
                            character.pos_z,
                            &active_items,
                            &mut mounted,
                            &mut stream,
                            &mut aes_send,
                            use_expanded_send_key,
                            logger,
                        )?;
                    } else {
                        logger.line("Rapture World rejected mount toggle without active character");
                    }
                } else if decrypted_opcode == Opcode::CharacterMobAutoAttack as u16
                    && decrypted_payload.len() >= 6
                {
                    let mob_global_id = read_le_u32(&decrypted_payload[2..6])?;
                    active_auto_attack = Some(AutoAttackState {
                        target_mob_id: mob_global_id,
                    });
                    logger.line(format!(
                        "Rapture World auto attack target set mob_global_id={mob_global_id}"
                    ));
                    if !process_auto_attack_swing(
                        mob_global_id,
                        mounted,
                        &mut active_character,
                        &connected_player,
                        &active_items,
                        &mut db_conn,
                        &mut stream,
                        &mut aes_send,
                        use_expanded_send_key,
                        &mut last_auto_attack_at,
                        logger,
                    )? {
                        active_auto_attack = None;
                    }
                } else if (decrypted_opcode == Opcode::SkillUse as u16
                    || decrypted_opcode == Opcode::SkillUseNoTarget as u16)
                    && decrypted_payload.len() >= 7
                {
                    // 0x0511 is the same request shape as 0x0517, sent
                    // instead by the client for skills that need no target
                    // ("cast around yourself", target_type 6, and self-
                    // heals) -- target_id is always 0 in that case, which
                    // `handle_use_skill` already tolerates. The outgoing
                    // ack must mirror whichever of the two opcodes the
                    // request came in on (confirmed by a real capture), so
                    // it's threaded straight through instead of hardcoding
                    // SkillUse.
                    let request_opcode = if decrypted_opcode == Opcode::SkillUseNoTarget as u16 {
                        Opcode::SkillUseNoTarget
                    } else {
                        Opcode::SkillUse
                    };
                    let number = decrypted_payload[2];
                    let target_mob_id = read_le_u32(&decrypted_payload[3..7])?;
                    handle_use_skill(
                        target_mob_id,
                        number,
                        request_opcode,
                        mounted,
                        &mut pending_cast,
                        &mut active_character,
                        &connected_player,
                        &active_items,
                        &active_skills,
                        &mut skill_cooldowns,
                        &mut active_buffs,
                        &mut db_conn,
                        &mut stream,
                        &mut aes_send,
                        use_expanded_send_key,
                        logger,
                    )?;
                } else if decrypted_opcode == Opcode::TargetMobHpUpdate as u16
                    && decrypted_payload.len() >= 6
                {
                    let mob_global_id = read_le_u32(&decrypted_payload[2..6])?;
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected mob target without active character");
                        continue;
                    };
                    if let Some(mob) = mob_by_id(character.map_id, mob_global_id) {
                        logger.line(format!(
                            "Rapture World mob target hp request mob_global_id={} mob_id={} hp={}/{}",
                            mob.id, mob.mob_id, mob.current_hp, mob.max_hp
                        ));
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_mob_target_hp(&mob)?,
                                "0x0305 mob target hp",
                                logger,
                            )?;
                        }
                    }
                } else if decrypted_opcode == Opcode::TargetMobGetState as u16
                    && decrypted_payload.len() >= 6
                {
                    let mob_global_id = read_le_u32(&decrypted_payload[2..6])?;
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected mob state without active character");
                        continue;
                    };
                    if let Some(mob) = mob_by_id(character.map_id, mob_global_id) {
                        logger.line(format!(
                            "Rapture World mob state request mob_global_id={} mob_id={} hp={}/{}",
                            mob.id, mob.mob_id, mob.current_hp, mob.max_hp
                        ));
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_mob_move(&mob, 0)?,
                                "0x0603 mob target position",
                                logger,
                            )?;
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                build_mob_state(&mob)?,
                                "0x0304 mob state",
                                logger,
                            )?;
                        }
                    }
                } else if decrypted_opcode == Opcode::TargetClear as u16 {
                    active_auto_attack = None;
                    logger.line("Rapture World target cleared");
                } else if decrypted_opcode == Opcode::PointBuy as u16 {
                    if decrypted_payload.len() < 2 {
                        logger.line("Rapture World rejected undersized point buy packet");
                        continue;
                    }
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected point buy without active character");
                        continue;
                    };
                    let character = character.clone();

                    let outcome = handle_point_buy(
                        &mut db_conn,
                        &character,
                        character.account_id,
                        active_account_points,
                        &mut active_items,
                        &decrypted_payload,
                        logger,
                    )?;
                    active_account_points = outcome.new_points;

                    if let Some(send) = aes_send.as_mut() {
                        for (index, packet) in outcome.add_item_packets.into_iter().enumerate() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                packet,
                                &format!("0x0205 point buy add item {}", index + 1),
                                logger,
                            )?;
                        }
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            outcome.response,
                            "0x2602 point buy result",
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::PointGiftSend as u16 {
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected gift send without active character");
                        continue;
                    };
                    let sender_account_id = character.account_id;

                    if let Some(outcome) = handle_gift_send(
                        &mut db_conn,
                        sender_account_id,
                        active_account_points,
                        &decrypted_payload,
                        logger,
                    )? {
                        active_account_points = outcome.new_points;
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                outcome.response,
                                "0x2603 gift send result",
                                logger,
                            )?;
                        }
                        // If the recipient happens to be online, push their
                        // refreshed gift-box list live so the gift shows up
                        // without a relog (offline recipients get it with the
                        // initial 0x2606 at their next character select).
                        if let Some((recipient_name, recipient_account_id)) = outcome.recipient {
                            if let Some(player) = find_player_by_name(&recipient_name) {
                                let list = build_gift_box_list(&mut db_conn, recipient_account_id)?;
                                let _ = player.send_packet(&list, "0x2606 gift box push", logger);
                            }
                        }
                    }
                } else if decrypted_opcode == Opcode::PointStoredItemGet as u16 {
                    let Some(character) = active_character.as_ref() else {
                        logger.line("Rapture World rejected gift claim without active character");
                        continue;
                    };
                    let (character_id, account_id) = (character.id, character.account_id);

                    if let Some(response) = handle_stored_item_get(
                        &mut db_conn,
                        character_id,
                        account_id,
                        &mut active_items,
                        &decrypted_payload,
                        logger,
                    )? {
                        if let Some(send) = aes_send.as_mut() {
                            send_encrypted(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                response,
                                "0x2607 gift claim result",
                                logger,
                            )?;
                        }
                    }
                } else if decrypted_opcode == Opcode::CashPoint as u16 {
                    // Empty-body request: the client is asking to refresh
                    // its displayed point balance (e.g. opening the cash
                    // shop UI). Just echo the session's current value.
                    logger.line("Rapture World cash point request");

                    if let Some(send) = aes_send.as_mut() {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            build_cash_points(active_account_points)?,
                            "0x2605 cash points",
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::Ping as u16 {
                    if let Some(send) = aes_send.as_mut() {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            build_ping()?,
                            "0xA303 ping",
                            logger,
                        )?;
                    }
                } else if decrypted_opcode == Opcode::Logout as u16 {
                    if position_dirty {
                        if let Some(character) = active_character.as_ref() {
                            db::save_character_position(
                                &mut db_conn,
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
                        }
                    }
                    if let Some(character) = active_character.as_ref() {
                        persist_active_buffs(&mut db_conn, character.id, &active_buffs)?;
                    }
                    active_buffs.clear();
                    // Unlike active_buffs (persisted just above) and
                    // skill_cooldowns (persisted continuously via
                    // CharSkills, rebuilt fresh at the next
                    // SelectCharacter), pending_cast has no per-character
                    // persistence at all -- leaving it set here would let a
                    // delayed cast started by THIS character resolve
                    // against whichever character is selected next on this
                    // same connection.
                    pending_cast = None;
                    mounted = false;
                    logger.line("Rapture World logout requested (relog to character select)");
                    let character_id = active_character.as_ref().map_or(0, |c| c.id);
                    if let Some(send) = aes_send.as_mut() {
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            build_leave_world_ack(character_id)?,
                            "0x0107 leave world ack",
                            logger,
                        )?;
                    }
                    // Relog stays on this same TCP connection and returns
                    // the client to the character-select screen -- it does
                    // not go back through the login server. The account
                    // guard stays held since the account is still logged in
                    // on this connection; only the in-world/chat presence is
                    // torn down.
                    chat_presence.take();
                    connected_player = None;
                    active_character = None;
                    active_items.clear();
                    active_skills.clear();
                    active_quick_slots.clear();
                    active_stored_items.clear();
                    active_account_points = 0;
                    item_cooldowns.clear();
                    skill_cooldowns.clear();
                    entity_visibility = EntityVisibility::default();
                    position_dirty = false;
                    use_expanded_send_key = false;

                    active_characters = db::get_characters_for_account(&mut db_conn, account_id)?
                        .into_iter()
                        .map(|character| {
                            let items = db::get_character_items(&mut db_conn, character.id)?;
                            Ok((character, items))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    if let Some(send) = aes_send.as_mut() {
                        if selected_faction != FACTION_NONE {
                            send_character_list(
                                &mut stream,
                                send,
                                use_expanded_send_key,
                                &active_characters,
                                logger,
                            )?;
                        }
                        send_encrypted(
                            &mut stream,
                            send,
                            use_expanded_send_key,
                            build_account_faction(selected_faction, max_mode)?,
                            "0x0109 account faction",
                            logger,
                        )?;
                    }
                }
            }
        }
    }
}

fn main() -> Result<()> {
    let logger = Arc::new(Logger::new("logs/rapture_world.log")?);
    logger.line("Starting Rapture World");

    {
        let logger = Arc::clone(&logger);
        thread::spawn(move || run_map_item_despawn_sweep(logger));
    }
    {
        let logger = Arc::clone(&logger);
        thread::spawn(move || run_mob_roam_sweep(logger));
    }
    {
        let logger = Arc::clone(&logger);
        thread::spawn(move || run_mob_respawn_sweep(logger));
    }
    {
        let logger = Arc::clone(&logger);
        thread::spawn(move || run_npc_patrol_sweep(logger));
    }
    {
        let logger = Arc::clone(&logger);
        thread::spawn(move || run_mob_dot_sweep(logger));
    }
    {
        let logger = Arc::clone(&logger);
        thread::spawn(move || run_mob_combat_sweep(logger));
    }

    ensure_map_data_dir()?;
    logger.line(format!(
        "Rapture World map data directory ready at {MAP_DATA_DIR}"
    ));
    load_svmap_entities(&logger)?;
    load_drop_tables(&logger)?;
    load_exp_table(&logger)?;

    let listener = TcpListener::bind("127.0.0.1:30810").context("failed to bind world port")?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let Some(guard) = ConnectionGuard::acquire(&logger) else {
                    continue;
                };
                apply_keepalive(&stream, &logger);
                apply_nodelay(&stream, &logger);
                let logger = Arc::clone(&logger);
                thread::spawn(move || {
                    let _guard = guard;
                    if let Err(error) = handle_client(stream, Arc::clone(&logger)) {
                        logger.line(format!("Rapture World client error: {error:?}"));
                    }
                });
            }
            Err(error) => logger.line(format!("Rapture World accept error: {error:?}")),
        }
    }

    Ok(())
}
