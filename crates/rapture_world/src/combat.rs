//! Player-vs-mob auto-attack combat: weapon/damage lookup and the actual
//! attack-swing resolution (damage, kill, drops, exp/level-up).

use anyhow::Result;
use rapture_common::db::{self, DbCharacter, DbCharacterItem};
use rapture_common::crypto::AesCtr;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::character::*;
use crate::chat::*;
use crate::drops::*;
use crate::experience::*;
use crate::session::*;
use crate::world_entities::*;

const WEAPON_SLOT: u8 = 5;
const AUTO_ATTACK_RANGE_GRACE: f32 = 2.0;
const AUTO_ATTACK_CANCEL_RANGE: f32 = 80.0;
const ATTACK_SUCCESS_NORMAL: u8 = 0;

pub(crate) fn basic_auto_attack_damage(stats: CharacterStats) -> u32 {
    let min = stats.min_attack.min(stats.max_attack);
    let max = stats.min_attack.max(stats.max_attack);
    min.saturating_add(max).saturating_div(2).max(1)
}

/// Same shape as `basic_auto_attack_damage`, but off `min_magic`/`max_magic`
/// (INT/WIS-derived) instead of `min_attack`/`max_attack` (STR/DEX-derived)
/// -- used for skills where `Skills.TypeAttack == 3` (magic), confirmed by
/// cross-referencing that column against class eligibility on every
/// damaging skill in the live DB.
pub(crate) fn basic_magic_attack_damage(stats: CharacterStats) -> u32 {
    let min = stats.min_magic.min(stats.max_magic);
    let max = stats.min_magic.max(stats.max_magic);
    min.saturating_add(max).saturating_div(2).max(1)
}

pub(crate) fn equipped_weapon_combat(
    db_conn: &mut db::Db,
    items: &[DbCharacterItem],
) -> Result<Option<db::DbWeaponCombat>> {
    let Some(weapon) = equipped_item(items, WEAPON_SLOT) else {
        return Ok(None);
    };
    db::get_weapon_combat(db_conn, weapon.item_type, weapon.type_id)
}

pub(crate) fn process_auto_attack_swing(
    target_mob_id: u32,
    mounted: bool,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[DbCharacterItem],
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    last_auto_attack_at: &mut Option<Instant>,
    logger: &Logger,
) -> Result<bool> {
    // Cloned rather than kept as a borrow of active_character: the killed-mob
    // branch below needs to mutate active_character (exp/level gain) while
    // this function still reads plain `character.*` fields throughout,
    // exactly the same "clone up front" pattern already used by the GM `set`
    // command and others for the same reason.
    let Some(character) = active_character.clone() else {
        logger.line("Rapture World cancelled auto attack without active character");
        return Ok(false);
    };
    let character = &character;

    if mounted {
        logger.line(format!(
            "Rapture World cancelled auto attack while mounted character_id={} mob_global_id={target_mob_id}",
            character.id
        ));
        return Ok(false);
    }

    let weapon_combat = match equipped_weapon_combat(db_conn, active_items) {
        Ok(Some(weapon_combat)) => weapon_combat,
        Ok(None) => {
            logger.line(format!(
                "Rapture World cancelled auto attack without valid weapon character_id={} mob_global_id={}",
                character.id, target_mob_id
            ));
            return Ok(false);
        }
        Err(error) => {
            logger.line(format!(
                "Rapture World cancelled auto attack after weapon lookup error character_id={} mob_global_id={} error={error:?}",
                character.id, target_mob_id
            ));
            return Ok(false);
        }
    };

    if let Some(last_attack_at) = *last_auto_attack_at {
        let cooldown = Duration::from_millis(weapon_combat.test_delay_ms);
        let elapsed = last_attack_at.elapsed();
        if elapsed < cooldown {
            return Ok(true);
        }
    }

    let Some(mob) = mob_by_id(character.map_id, target_mob_id) else {
        logger.line(format!(
            "Rapture World cancelled auto attack unknown mob character_id={} mob_global_id={}",
            character.id, target_mob_id
        ));
        return Ok(false);
    };
    if mob.current_hp == 0 {
        logger.line(format!(
            "Rapture World cancelled auto attack dead mob character_id={} mob_global_id={} mob_id={}",
            character.id, mob.id, mob.mob_id
        ));
        return Ok(false);
    }

    let distance = distance_xz(character.pos_x, character.pos_z, mob.pos_x, mob.pos_z);
    let allowed_range = weapon_combat.range + AUTO_ATTACK_RANGE_GRACE;
    if distance > allowed_range {
        if distance > AUTO_ATTACK_CANCEL_RANGE {
            logger.line(format!(
                "Rapture World cancelled auto attack target too far character_id={} mob_global_id={} distance={distance:.2} weapon_range={:.2}",
                character.id, mob.id, weapon_combat.range
            ));
            return Ok(false);
        }
        return Ok(true);
    }

    let equipment_stats = match db::get_equipment_stats(db_conn, active_items) {
        Ok(equipment_stats) => equipment_stats,
        Err(error) => {
            logger.line(format!(
                "Rapture World cancelled auto attack after equipment stats error character_id={} mob_global_id={} error={error:?}",
                character.id, target_mob_id
            ));
            return Ok(false);
        }
    };
    let character_stats = CharacterStats::from_character(Some(character), equipment_stats);
    let requested_damage = basic_auto_attack_damage(character_stats);
    let Some(result) = damage_mob(character.map_id, target_mob_id, requested_damage) else {
        logger.line(format!(
            "Rapture World cancelled auto attack missing damage target character_id={} mob_global_id={}",
            character.id, target_mob_id
        ));
        return Ok(false);
    };

    // Provoke the mob into fighting back (retaliation aggro) -- applies even
    // to passive mobs (`ai == 0`), which only ever engage this way.
    set_mob_aggro(character.map_id, target_mob_id, character.id);

    *last_auto_attack_at = Some(Instant::now());
    logger.line(format!(
        "Rapture World auto attack character_id={} mob_global_id={} mob_id={} str={} dex={} atk={}-{} weapon_type={} weapon_type_id={} attack_time={} delay_ms={} original_delay_ms={} range={:.2} allowed_range={allowed_range:.2} distance={distance:.2} final_damage={} applied_hp={} hp={}/{} killed={} respawn_seconds={}",
        character.id,
        result.mob.id,
        result.mob.mob_id,
        character_stats.strength,
        character_stats.dexterity,
        character_stats.min_attack,
        character_stats.max_attack,
        weapon_combat.item_type,
        weapon_combat.type_id,
        weapon_combat.attack_time,
        weapon_combat.test_delay_ms,
        weapon_combat.original_delay_ms,
        weapon_combat.range,
        result.damage_hp,
        result.applied_hp,
        result.mob.current_hp,
        result.mob.max_hp,
        result.killed,
        result
            .respawn_seconds
            .map(|seconds| seconds.to_string())
            .unwrap_or_else(|| "none".to_string())
    ));

    let attack_packet = build_character_mob_attack(
        character.id,
        result.mob.id,
        ATTACK_SUCCESS_NORMAL,
        result.damage_hp,
    )?;
    for player in players_on_map_near(character.map_id, result.mob.pos_x, result.mob.pos_z, COMBAT_RADIUS) {
        player.send_packet(&attack_packet, "0x0503 character mob auto attack", logger)?;
    }

    resolve_mob_damage_result(
        &result,
        character.map_id,
        character.id,
        character.pos_y,
        active_character,
        connected_player,
        active_items,
        db_conn,
        stream,
        aes_send,
        use_expanded_send_key,
        logger,
    )
}

/// Death-type value carried by `0x0504 CharacterDeath` for a mob kill --
/// the only value observed in a real capture so far (see
/// `Opcode::CharacterDeath`).
const DEATH_TYPE_MOB: u8 = 0x02;

/// Applies one incoming mob melee hit to the owning connection's own
/// character: computes the post-defense damage, deducts it (respecting the
/// raw-vs-effective HP invariant), persists it, broadcasts the 0x0605 swing
/// to nearby players, and on a lethal hit runs the death sequence
/// (0x051C + 0x0504 broadcast, deaths++, dropped mob aggro). Returns whether
/// this hit killed the player, so the caller can enter its dead-input-gated
/// state. Called only from the player's OWN connection thread, draining the
/// `pending_damage` mailbox the combat sweep queues into (see
/// chat::ConnectedPlayer::queue_damage / sweeps::run_mob_combat_sweep) --
/// that keeps HP authority on a single thread.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_incoming_mob_damage(
    mob_global_id: u32,
    mob_id: u16,
    raw_attack: u32,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[DbCharacterItem],
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<bool> {
    let Some(character) = active_character.as_mut() else {
        return Ok(false);
    };

    let equipment_stats = db::get_equipment_stats(db_conn, active_items)?;
    let stats = CharacterStats::from_character(Some(character), equipment_stats);
    let effective_hp = stats.current_hp;
    if effective_hp == 0 {
        // Already dead -- a queued hit that raced the death itself. Ignore.
        return Ok(false);
    }

    // A mob with no `Attack1` (missing/zero column) still visibly swings but
    // deals nothing; otherwise damage is attack minus the target's defense
    // AND enchant absorption (a flat, universal reduction from enchanted
    // armor -- see db::ARMOR_ENCHANT_ABSORPTION), floored at 1 so a real
    // attack always chips something off.
    let damage = if raw_attack == 0 {
        0
    } else {
        raw_attack
            .saturating_sub(stats.defense.saturating_add(stats.absorption))
            .max(1)
    };

    // Mirror of apply_heal_skill's HP math, in reverse: work in a DELTA on
    // the raw `current_hp` field, never assign an effective value to it (see
    // CharacterStats::from_character's invariant note).
    let new_effective = effective_hp.saturating_sub(damage);
    let delta = effective_hp - new_effective;
    let new_raw = character.current_hp.saturating_sub(delta);
    let killed = damage > 0 && new_effective == 0;

    character.current_hp = new_raw;
    db::set_character_status(db_conn, character.id, new_raw, character.current_mp, character.current_sp)?;

    let map_id = character.map_id;
    let victim_id = character.id;
    let pos_x = character.pos_x;
    let pos_z = character.pos_z;
    let result = if killed { 1 } else { 0 };
    let damage_hp = damage.min(u32::from(u16::MAX)) as u16;

    if let Some(player) = connected_player.as_ref() {
        player.update_character(character.clone());
    }

    // The mob's swing, broadcast to everyone near the victim (including the
    // victim's own client) via the stateless expanded-XOR path -- same
    // mechanism auto-attack (0x0503) uses for its broadcast.
    let attack_packet = build_mob_attack_character(result, mob_global_id, victim_id, damage_hp)?;
    for player in players_on_map_near(map_id, pos_x, pos_z, COMBAT_RADIUS) {
        player.send_packet(&attack_packet, "0x0605 mob attack character", logger)?;
    }

    logger.line(format!(
        "Rapture World mob attack character_id={victim_id} mob_global_id={mob_global_id} mob_id={mob_id} raw_attack={raw_attack} defense={} absorption={} damage={damage} hp={new_effective}/{} killed={killed}",
        stats.defense, stats.absorption, stats.max_hp
    ));

    // Re-sync the victim's authoritative HP to their own client.
    if let Some(send) = aes_send.as_mut() {
        let refreshed = CharacterStats::from_character(active_character.as_ref(), equipment_stats);
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_current_hitpoints(&refreshed)?,
            "0x0521 current hp after mob attack",
            logger,
        )?;
    }

    if killed {
        // Death sequence, in the order the real server sent it (capture):
        // 0x051C, then 0x0504, broadcast to everyone near the victim.
        let death_state = build_character_death_state(victim_id)?;
        let death = build_character_death(victim_id, DEATH_TYPE_MOB, mob_global_id)?;
        for player in players_on_map_near(map_id, pos_x, pos_z, COMBAT_RADIUS) {
            player.send_packet(&death_state, "0x051C character death state", logger)?;
            player.send_packet(&death, "0x0504 character death", logger)?;
        }

        // Record the death and drop the killer's aggro so it stops swinging
        // and returns to roaming.
        db::set_character_attribute(
            db_conn,
            victim_id,
            db::CharAttribute::Deaths,
            i64::from(active_character.as_ref().map_or(0, |c| c.deaths)) + 1,
        )?;
        if let Some(active) = active_character.as_mut() {
            active.deaths = active.deaths.saturating_add(1);
        }
        clear_mob_aggro(map_id, mob_global_id);
        if let Some(player) = connected_player.as_ref() {
            player.set_alive(false);
        }
        logger.line(format!(
            "Rapture World character died character_id={victim_id} killer_mob_global_id={mob_global_id} mob_id={mob_id}"
        ));
    }

    Ok(killed)
}

/// Shared "what happens after a mob takes damage" handling -- death
/// broadcast + drops + exp/level gain if it died, or just a target-HP
/// update if it didn't. Used by both auto-attack and skill-cast damage,
/// since the consequences of a kill don't depend on how the killing blow
/// was dealt. Returns whether the mob is still alive (mirroring
/// `process_auto_attack_swing`'s original "keep attacking?" return value).
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_mob_damage_result(
    result: &MobDamageResult,
    map_id: u16,
    killer_id: u32,
    killer_pos_y: f32,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[DbCharacterItem],
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<bool> {
    if result.killed {
        let death_packet = build_mob_death(result.mob.id, killer_id)?;
        for player in players_on_map_near(map_id, result.mob.pos_x, result.mob.pos_z, COMBAT_RADIUS) {
            player.send_packet(&death_packet, "0x0606 mob death", logger)?;
        }
        // Y uses the killer's own live position, not the mob's stored pos_y:
        // .svmap only gives a coarse per-area Y range (no real per-tile
        // heightmap), so that value is frequently wrong on sloped terrain.
        // The killer's client-reported Y is a real, live ground-height
        // sample, and they're necessarily standing right next to (or in
        // clear line-of-sight range of) the mob when it dies, so it tracks
        // local terrain far more reliably than an area-wide average.
        let drop_packets = roll_mob_drops(
            map_id,
            result.mob.mob_id,
            result.mob.money_min,
            result.mob.money_max,
            killer_id,
            result.mob.pos_x,
            killer_pos_y,
            result.mob.pos_z,
            logger,
        )?;
        if !drop_packets.is_empty() {
            let recipients = players_on_map_near(map_id, result.mob.pos_x, result.mob.pos_z, COMBAT_RADIUS);
            for packet in drop_packets {
                for player in &recipients {
                    player.send_packet(&packet, "0x0401 mob drop", logger)?;
                }
            }
        }

        if result.mob.exp > 0 {
            grant_mob_kill_exp(
                result.mob.mob_id,
                result.mob.level,
                result.mob.exp,
                active_character,
                connected_player,
                active_items,
                db_conn,
                stream,
                aes_send,
                use_expanded_send_key,
                logger,
            )?;
        }
        Ok(false)
    } else {
        // Confirmed via a real capture of a hit-through-death sequence:
        // the real server broadcasts a mob current-HP sync (0x0610) to
        // everyone nearby after every non-lethal hit -- NOT the 0x0305
        // ack below, which the capture never showed happening
        // automatically (0x0305 only ever appeared as a direct response
        // to the client explicitly requesting/re-selecting a target,
        // matching this project's existing on-request handler in
        // main.rs). Previously this broadcast only fired for DoT ticks
        // (see sweeps.rs's run_mob_dot_sweep) -- missing it here left
        // the client with no ongoing HP sync after an ordinary hit, which
        // is the confirmed root cause of a reported bug where a mob's HP
        // bar visibly reset to 0 about a second after any hit until the
        // player re-selected the target (re-triggering 0x0305).
        if let Ok(hp_packet) = build_mob_current_hp(result.mob.id, result.mob.current_hp) {
            for player in players_on_map_near(map_id, result.mob.pos_x, result.mob.pos_z, COMBAT_RADIUS) {
                let _ = player.send_packet(&hp_packet, "0x0610 mob current hp after attack", logger);
            }
        }
        if let Some(send) = aes_send.as_mut() {
            send_encrypted(
                stream,
                send,
                use_expanded_send_key,
                build_mob_target_hp(&result.mob)?,
                "0x0305 mob target hp after attack",
                logger,
            )?;
        }
        Ok(true)
    }
}

/// Grants scaled EXP for a mob kill to `active_character` (if any is set),
/// persists it, and pushes live stat packets to `stream`/`aes_send` (if
/// present). Extracted out of `resolve_mob_damage_result` so a kill that
/// happens OUTSIDE any specific connection's own call stack -- a DoT tick
/// firing from the background sweep thread (see skillcast.rs/sweeps.rs),
/// attributed to whichever character cast it -- can grant the exact same
/// EXP via the killer's own connection thread (queued through
/// `ConnectedPlayer::queue_mob_kill_exp`, drained in main.rs same as
/// `pending_teleport`/`pending_trade_refresh`) instead of duplicating this
/// logic.
#[allow(clippy::too_many_arguments)]
pub(crate) fn grant_mob_kill_exp(
    mob_id: u16,
    mob_level: u16,
    mob_exp: u32,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[DbCharacterItem],
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    let Some(active) = active_character.as_mut() else {
        return Ok(());
    };

    let scaled_exp = scaled_kill_exp(active.level, mob_level, mob_exp);
    let gain = apply_exp_gain(active, scaled_exp);
    db::set_character_level_exp(db_conn, active.id, active.level, active.exp)?;
    if gain.levels_gained > 0 {
        db::set_character_status(db_conn, active.id, active.max_hp, active.max_mp, active.max_sp)?;
    }
    if let Some(player) = connected_player.as_ref() {
        player.update_character(active.clone());
    }
    logger.line(format!(
        "Rapture World exp character_id={} mob_id={mob_id} mob_level={mob_level} base_exp={mob_exp} scaled_gained={} level={} exp={} leveled_up={} levels_gained={}",
        active.id,
        scaled_exp,
        gain.new_level,
        gain.new_exp,
        gain.leveled_up,
        gain.levels_gained
    ));
    // Real packet capture (2026-07-09) confirmed a level-up pushes 0x051F
    // (max hp/mp/sp) live mid-session -- the exact packet this bundle
    // already sends for the GM `set level` command. The base component of
    // that jump is now a hardcoded flat 100/level (see
    // experience::apply_exp_gain / character::base_pool_for_level), not the
    // stat-derived component. Reusing the bundle here pushes the correct
    // numbers live.
    if let Some(send) = aes_send.as_mut() {
        let equipment_stats = db::get_equipment_stats(db_conn, active_items)?;
        let character_stats = CharacterStats::from_character(Some(active), equipment_stats);
        send_character_stat_packets(stream, send, use_expanded_send_key, Some(active), &character_stats, logger)?;
    }
    // Still unconfirmed: the raw exp/level numbers themselves (client exp
    // bar, "Lv.X" nameplate, any level-up toast) -- no opcode for those
    // was found in the Aries reference (checked game/outgoing 0x0100-
    // 0x0900; 0x0522 GameUserKillInfoOutgoing is kills, not exp). DB and
    // in-memory state are fully correct and will show right on next relog
    // either way; only the LIVE mid-session exp/level display is
    // unverified.
    Ok(())
}

/// Same EXP grant as `grant_mob_kill_exp`, but for a DoT kill attributed
/// to a caster who's no longer online (see sweeps::run_mob_dot_sweep) --
/// works purely against the DB, no live packets or in-memory state, since
/// there's no connection to keep in sync.
pub(crate) fn grant_offline_mob_kill_exp(
    db_conn: &mut db::Db,
    character_id: u32,
    mob_id: u16,
    mob_level: u16,
    mob_exp: u32,
    logger: &Logger,
) -> Result<()> {
    let Some(mut character) = db::get_character_by_char_id(db_conn, character_id)? else {
        return Ok(());
    };

    let scaled_exp = scaled_kill_exp(character.level, mob_level, mob_exp);
    let gain = apply_exp_gain(&mut character, scaled_exp);
    db::set_character_level_exp(db_conn, character.id, character.level, character.exp)?;
    if gain.levels_gained > 0 {
        db::set_character_status(
            db_conn,
            character.id,
            character.max_hp,
            character.max_mp,
            character.max_sp,
        )?;
    }
    logger.line(format!(
        "Rapture World offline exp character_id={} mob_id={mob_id} mob_level={mob_level} base_exp={mob_exp} scaled_gained={} level={} exp={} leveled_up={} levels_gained={}",
        character.id,
        scaled_exp,
        gain.new_level,
        gain.new_exp,
        gain.leveled_up,
        gain.levels_gained
    ));
    Ok(())
}
