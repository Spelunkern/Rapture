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
            if let Some(active) = active_character.as_mut() {
                let scaled_exp = scaled_kill_exp(active.level, result.mob.level, result.mob.exp);
                let gain = apply_exp_gain(active, scaled_exp);
                db::set_character_level_exp(db_conn, active.id, active.level, active.exp)?;
                if gain.levels_gained > 0 {
                    db::set_character_status(
                        db_conn,
                        active.id,
                        active.max_hp,
                        active.max_mp,
                        active.max_sp,
                    )?;
                }
                if let Some(player) = connected_player.as_ref() {
                    player.update_character(active.clone());
                }
                logger.line(format!(
                    "Rapture World exp character_id={} mob_id={} mob_level={} base_exp={} scaled_gained={} level={} exp={} leveled_up={} levels_gained={}",
                    active.id,
                    result.mob.mob_id,
                    result.mob.level,
                    result.mob.exp,
                    scaled_exp,
                    gain.new_level,
                    gain.new_exp,
                    gain.leveled_up,
                    gain.levels_gained
                ));
                // Real packet capture (2026-07-09) confirmed a level-up
                // pushes 0x051F (max hp/mp/sp) live mid-session -- the exact
                // packet this bundle already sends for the GM `set level`
                // command, and further capture analysis traced the real
                // source of that jump to PS_GameDefs.dbo.StatusDefs (see
                // experience::apply_exp_gain), not the stat-derived
                // component. Reusing the bundle here pushes the correct,
                // now-StatusDefs-backed numbers live.
                if let Some(send) = aes_send.as_mut() {
                    let equipment_stats = db::get_equipment_stats(db_conn, active_items)?;
                    let character_stats =
                        CharacterStats::from_character(Some(active), equipment_stats);
                    send_character_stat_packets(
                        stream,
                        send,
                        use_expanded_send_key,
                        Some(active),
                        &character_stats,
                        logger,
                    )?;
                }
                // Still unconfirmed: the raw exp/level numbers themselves
                // (client exp bar, "Lv.X" nameplate, any level-up toast) --
                // no opcode for those was found in the Aries reference
                // (checked game/outgoing 0x0100-0x0900; 0x0522
                // GameUserKillInfoOutgoing is kills, not exp). DB and
                // in-memory state are fully correct and will show right on
                // next relog either way; only the LIVE mid-session exp/
                // level display is unverified.
            }
        }
        Ok(false)
    } else {
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
