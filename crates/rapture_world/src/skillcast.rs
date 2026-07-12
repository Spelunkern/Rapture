//! Using a learned skill against a mob (no cast-time/channeling in this
//! pass -- resolves instantly, matching the project's current scope
//! decision; player-vs-player skill targets are also out of scope for now,
//! mirroring the existing mob-only auto-attack system).
//!
//! Wire format: opcode **0x0517** for a mob target (confirmed against a
//! real capture). A second incoming opcode, **0x0511**, is sent instead by
//! the client for no-target skills (`TargetType` 6, "cast around
//! yourself", and self-heals) -- confirmed via a real capture showing the
//! client sending `0x0511` with the same 5-byte request shape (`number:u8,
//! targetId:u32`) but `targetId` always 0. Both incoming opcodes are
//! decoded identically and dispatched to the same handler. Outgoing result
//! payload shape matches Shaiya-Aries's `GameCharSkillUseOutgoing_EP5`
//! struct (`network/game/outgoing/0500.h`) byte-for-byte, confirmed via
//! capture: `targetType:u8, senderId:u32, targetId:u32, skillId:u16,
//! skillLv:u8, health:u16, stamina:u16, mana:u16` (12 bytes after the
//! opcode). `targetType = 1` for a mob target is confirmed by that capture
//! too. **Crucially, the outgoing ack's opcode mirrors whichever opcode the
//! request came in on** (0x0511 request -> 0x0511 ack, 0x0517 request ->
//! 0x0517 ack) -- confirmed by a real self-heal capture showing the
//! server's own ack on 0x0511, matching Aries's `GameCharSkillUseOutgoing_
//! EP5` struct, which is *itself* declared with `opcode{ 0x511 }` (Aries's
//! reference always acks on 0x511; this project's separate 0x0517-for-mob
//! finding still holds for the enemy-target path, so the response opcode
//! must track the request rather than being hardcoded to either value).
//!
//! A skill with a nonzero `AddDamageHP` deals a SECOND, separate hit via
//! opcode `0x051A` (confirmed against a real capture, and confirmed by the
//! user that this "fixed extra damage" is a property some skills have):
//! same fields as `0x0517` minus the leading `targetType` byte --
//! `senderId:u32, targetId:u32, skillId:u16, skillLv:u8, health:u16,
//! stamina:u16, mana:u16` (17 bytes). Unlike the main hit (stat-based
//! attack damage + `DamageHP`), this second hit is a flat, non-scaling
//! `AddDamageHP` amount, applied as its own independent damage instance
//! (its own `damage_mob` call and its own kill-consequence resolution) --
//! not summed into the first hit's number.
//!
//! `Skills.TypeAttack` picks which stat pair the main hit scales off of --
//! confirmed by cross-referencing it against class eligibility on every
//! damaging skill in the live DB: 1/2 (melee/ranged physical, fighter/
//! defender/ranger/archer) use `min_attack`/`max_attack` (STR/DEX), 3
//! (magic, mage/priest) uses `min_magic`/`max_magic` (INT/WIS).
//!
//! AoE via `Skills.TargetType`/`ApplyRange` (confirmed against Imgeneus's
//! `TargetType` enum, which casts retail's raw byte directly with no
//! remapping, so values line up 1:1): `3` (SelectedEnemy) and `1`
//! (AnyEnemy) are the existing single-target case; `6`
//! (EnemiesNearCaster) hits every mob within `ApplyRange` of the CASTER;
//! `7` (EnemiesNearTarget) hits every mob within `ApplyRange` of the
//! originally-clicked TARGET (itself included). `2`/`4`/`5`/`8` (self/
//! allies/party) are rejected outright rather than silently mishandled --
//! this project's combat is mob-only so far, no ally/self/party skill
//! targets are supported yet.

use anyhow::Result;
use rapture_common::crypto::AesCtr;
use rapture_common::db::{self, DbCharacter, DbCharacterItem, DbCharacterSkill, DbEquipmentStats, DbSkillDefinition};
use rapture_common::packet::{Opcode, PacketWriter};
use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::character::*;
use crate::chat::*;
use crate::combat::*;
use crate::session::*;
use crate::world_entities::*;

const SKILL_TARGET_TYPE_MOB: u8 = 1;
/// The outgoing ack's `targetType` byte for a self-heal -- confirmed
/// against a real capture to be `0`, NOT `2` (`TargetType::Caster`) as
/// originally guessed by analogy with `Skills.TargetType`; the ack's
/// target-type byte apparently isn't the same enum/value space as the DB
/// column.
const SKILL_TARGET_TYPE_SELF: u8 = 0;

/// Same tolerance idea as `combat::AUTO_ATTACK_RANGE_GRACE`: a strict
/// distance <= AttackRange check rejected valid casts constantly (client-
/// side position/facing is never in perfect sync with the server's last
/// known position), forcing the player to circle the mob dozens of times
/// before a cast landed. Matches the auto-attack fix exactly.
const SKILL_RANGE_GRACE: f32 = 2.0;

/// `Skills.TargetType` values (see module doc for the full mapping).
const TARGET_TYPE_ANY_ENEMY: u8 = 1;
const TARGET_TYPE_SELECTED_ENEMY: u8 = 3;
const TARGET_TYPE_ENEMIES_NEAR_CASTER: u8 = 6;
const TARGET_TYPE_ENEMIES_NEAR_TARGET: u8 = 7;

/// `Skills.TypeDetail` values relevant to healing (confirmed by the user
/// against native-client testing).
const TYPE_DETAIL_INSTANT_HEAL: u16 = 41;
const TYPE_DETAIL_PERIODIC_HEAL: u16 = 81;
/// A periodic heal (`TypeDetail == 81`) always ticks every 3 seconds,
/// regardless of the skill -- confirmed by the user; `Skills.KeepTime` is
/// only the total duration, not the tick interval.
const PERIODIC_HEAL_TICK: Duration = Duration::from_secs(3);

/// `Skills.TypeDetail == 82` ("Buff" in Imgeneus's `TypeDetail` enum) --
/// applies `AbilityType1..10`/`AbilityValue1..10` for `KeepTime` seconds.
/// First-pass scope (explicitly agreed): only the flat-stat ability types
/// below are applied; every other `AbilityType` (rates, regen, attack
/// power, etc.) is logged and ignored rather than silently mishandled.
const TYPE_DETAIL_BUFF: u16 = 82;
/// `AbilityType` values this first pass understands (see Imgeneus's
/// `AbilityType` enum for the full ~50-value list).
const ABILITY_HP: u8 = 1;
const ABILITY_MP: u8 = 2;
const ABILITY_SP: u8 = 3;
const ABILITY_STR: u8 = 4;
const ABILITY_REC: u8 = 5;
const ABILITY_INT: u8 = 6;
const ABILITY_WIS: u8 = 7;
const ABILITY_DEX: u8 = 8;
const ABILITY_LUC: u8 = 9;

/// One active "stat buff" effect on the caster (self-only, matching the
/// heal system's existing no-ally-targeting scope). Its bonuses are added
/// directly into a copy of the caster's equipment stats (see
/// `equipment_stats_with_buffs`) before computing `CharacterStats`, the
/// same way equipment bonuses already work -- so `str`/`hp`/etc. below
/// are deltas, not absolute values. Re-casting the same `skill_id`
/// replaces (refreshes) any existing entry rather than stacking.
pub(crate) struct ActiveBuff {
    id: u32,
    skill_id: u16,
    skill_level: u8,
    ends_at: Instant,
    strength: u32,
    reaction: u32,
    intelligence: u32,
    wisdom: u32,
    dexterity: u32,
    luck: u32,
    max_hp: u32,
    max_mp: u32,
    max_sp: u32,
}

/// Merges every active buff's stat deltas into a copy of `equipment` --
/// callers that need buff-aware `CharacterStats` pass the result here
/// through `CharacterStats::from_character` instead of the raw equipment
/// stats. Buffs not folded in here are simply invisible to that call site
/// (an acceptable, explicit limitation of this first pass: only
/// `resolve_skill_use`'s own MP/SP check and damage/heal math are
/// buff-aware for now, not the ~30 other `CharacterStats::from_character`
/// call sites across the codebase).
pub(crate) fn equipment_stats_with_buffs(mut equipment: DbEquipmentStats, active_buffs: &[ActiveBuff]) -> DbEquipmentStats {
    for buff in active_buffs {
        equipment.strength = equipment.strength.saturating_add(buff.strength);
        equipment.reaction = equipment.reaction.saturating_add(buff.reaction);
        equipment.intelligence = equipment.intelligence.saturating_add(buff.intelligence);
        equipment.wisdom = equipment.wisdom.saturating_add(buff.wisdom);
        equipment.dexterity = equipment.dexterity.saturating_add(buff.dexterity);
        equipment.luck = equipment.luck.saturating_add(buff.luck);
        equipment.max_hp = equipment.max_hp.saturating_add(buff.max_hp);
        equipment.max_mp = equipment.max_mp.saturating_add(buff.max_mp);
        equipment.max_sp = equipment.max_sp.saturating_add(buff.max_sp);
    }
    equipment
}

/// One active "heal over time" effect on the caster (self-only, matching
/// this project's current no-ally-targeting scope for healing). Re-casting
/// the same `skill_id` replaces (refreshes) any existing entry rather than
/// stacking a second one.
pub(crate) struct ActiveHeal {
    skill_id: u16,
    skill_level: u8,
    /// Mirrors whichever opcode the original cast request came in on (see
    /// module doc) -- every tick's ack reuses it, matching the confirmed
    /// request/response opcode symmetry.
    response_opcode: Opcode,
    ends_at: Instant,
    next_tick_at: Instant,
    heal_hp: u32,
    heal_sp: u32,
    heal_mp: u32,
}

/// A skill whose `ReadyTime` (cast time) hasn't elapsed yet. Stores
/// exactly what's needed to re-invoke `resolve_skill_use` once `ready_at`
/// passes, fully re-validating everything at that point rather than
/// trusting the state captured at cast start.
pub(crate) struct PendingCast {
    target_mob_id: u32,
    number: u8,
    request_opcode: Opcode,
    ready_at: Instant,
}

fn build_cast_started(sender_id: u32, target_id: u32, skill_id: u16, skill_level: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::SkillCastStarted);
    packet.write_u32(sender_id);
    packet.write_u32(target_id);
    packet.write_u16(skill_id);
    packet.write_u8(skill_level);
    packet.into_bytes()
}

/// Confirmed via a real capture: casting any skill (even an instant one)
/// sends a bare, payload-less `AutoAttackStop` first, interrupting
/// whatever auto-attack was in progress.
fn build_auto_attack_stop() -> Result<Vec<u8>> {
    PacketWriter::new(Opcode::AutoAttackStop).into_bytes()
}

fn build_skill_used_sp_mp(sp: u32, mp: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::SkillUsedSpMp);
    packet.write_u32(sp);
    packet.write_u32(mp);
    packet.into_bytes()
}

fn build_skill_use_result(
    response_opcode: Opcode,
    target_type: u8,
    sender_id: u32,
    target_id: u32,
    skill_id: u16,
    skill_level: u8,
    health: u16,
    stamina: u16,
    mana: u16,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(response_opcode);
    packet.write_u8(target_type);
    packet.write_u32(sender_id);
    packet.write_u32(target_id);
    packet.write_u16(skill_id);
    packet.write_u8(skill_level);
    packet.write_u16(health);
    packet.write_u16(stamina);
    packet.write_u16(mana);
    packet.into_bytes()
}

fn build_skill_extra_damage_result(
    sender_id: u32,
    target_id: u32,
    skill_id: u16,
    skill_level: u8,
    health: u16,
    stamina: u16,
    mana: u16,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::SkillExtraDamage);
    packet.write_u32(sender_id);
    packet.write_u32(target_id);
    packet.write_u16(skill_id);
    packet.write_u8(skill_level);
    packet.write_u16(health);
    packet.write_u16(stamina);
    packet.write_u16(mana);
    packet.into_bytes()
}

/// Confirmed against a real capture of `CharacterActiveBuffs` (0x010A,
/// which reuses this exact per-entry shape at login -- see
/// `build_character_active_buffs`): `id:u32, skillId:u16, skillLevel:u8,
/// countdownSeconds:u32` (11 bytes), matching Imgeneus's
/// `SerializedActiveBuff`/`SendAddBuff` byte-for-byte. No character/object
/// id -- this is a per-connection, self-only packet (the client already
/// knows it's about its own character).
fn build_buff_add(id: u32, skill_id: u16, skill_level: u8, countdown_seconds: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::BuffAdd);
    packet.write_u32(id);
    packet.write_u16(skill_id);
    packet.write_u8(skill_level);
    packet.write_u32(countdown_seconds);
    packet.into_bytes()
}

/// Matches Imgeneus's `SendRemoveBuff(client, id)` -- just the buff's own
/// unique `id` (not a character/object id).
fn build_buff_remove(id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::BuffRemove);
    packet.write_u32(id);
    packet.into_bytes()
}

/// The `CharacterActiveBuffs` (0x010A) list sent at character-select --
/// confirmed against a real capture showing a live buff ("Protective Aura
/// Lv6", `SkillID` 132, cross-referenced against the DB) with this exact
/// shape: `count:u8` + entries of `id:u32, skillId:u16, skillLevel:u8,
/// countdownSeconds:u32` (11 bytes each), matching Imgeneus's
/// `CharacterActiveBuffs`/`SerializedActiveBuff` byte-for-byte. Previously
/// this project always sent a hardcoded empty list here, which is why a
/// restored (or freshly cast) buff never showed its icon -- the stat
/// bonus was real, but the client was never told the buff existed.
pub(crate) fn build_character_active_buffs(active_buffs: &[ActiveBuff]) -> Result<Vec<u8>> {
    let now = Instant::now();
    let mut packet = PacketWriter::new(Opcode::CharacterActiveBuffs);
    packet.write_u8(active_buffs.len().min(u8::MAX as usize) as u8);
    for buff in active_buffs.iter().take(u8::MAX as usize) {
        let countdown = buff.ends_at.saturating_duration_since(now).as_secs().min(u64::from(u32::MAX)) as u32;
        packet.write_u32(buff.id);
        packet.write_u16(buff.skill_id);
        packet.write_u8(buff.skill_level);
        packet.write_u32(countdown);
    }
    packet.into_bytes()
}

/// Handles 0x0511/0x0517 (skill use request): if the skill's `ReadyTime`
/// (cast time) is 0, resolves it immediately via `resolve_skill_use`,
/// unchanged from before cast-time support existed. Otherwise, this does a
/// cheap up-front validation (learned skill, not on cooldown, enough
/// MP/SP) just to fail fast on an obviously-invalid cast, sends the
/// `AutoAttackStop` + `SkillCastStarted` (0x0510) packets confirmed by a
/// real capture, and stores a `PendingCast` for `process_pending_cast` to
/// resolve (with full re-validation) once `ReadyTime * 250ms` elapses.
/// Only one cast can be pending at a time -- a new request while one is
/// already in progress is rejected outright.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_use_skill(
    target_mob_id: u32,
    number: u8,
    request_opcode: Opcode,
    pending_cast: &mut Option<PendingCast>,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[DbCharacterItem],
    active_skills: &[DbCharacterSkill],
    skill_cooldowns: &mut HashMap<u16, Instant>,
    active_heals: &mut Vec<ActiveHeal>,
    active_buffs: &mut Vec<ActiveBuff>,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    let Some(character) = active_character.clone() else {
        logger.line("Rapture World rejected skill use without active character");
        return Ok(());
    };

    if pending_cast.is_some() {
        logger.line(format!(
            "Rapture World rejected skill use: already casting character_id={}",
            character.id
        ));
        return Ok(());
    }

    let Some(known_skill) = active_skills.iter().find(|skill| skill.number == number) else {
        logger.line(format!(
            "Rapture World rejected skill use: no learned skill in bar slot number={number} character_id={}",
            character.id
        ));
        return Ok(());
    };

    let Some(definition) =
        db::get_skill_definition(db_conn, known_skill.skill_id, known_skill.skill_level)?
    else {
        logger.line(format!(
            "Rapture World rejected skill use: missing definition skill_id={} level={}",
            known_skill.skill_id, known_skill.skill_level
        ));
        return Ok(());
    };

    let cast_time_ms = u64::from(definition.ready_time) * 250;
    if cast_time_ms == 0 {
        return resolve_skill_use(
            target_mob_id,
            number,
            request_opcode,
            active_character,
            connected_player,
            active_items,
            active_skills,
            skill_cooldowns,
            active_heals,
            active_buffs,
            db_conn,
            stream,
            aes_send,
            use_expanded_send_key,
            logger,
        );
    }

    if let Some(ready_at) = skill_cooldowns.get(&known_skill.skill_id) {
        if *ready_at > Instant::now() {
            logger.line(format!(
                "Rapture World rejected skill use on cooldown skill_id={} character_id={}",
                known_skill.skill_id, character.id
            ));
            return Ok(());
        }
    }

    let equipment_stats = equipment_stats_with_buffs(db::get_equipment_stats(db_conn, active_items)?, active_buffs);
    let character_stats = CharacterStats::from_character(Some(&character), equipment_stats);
    if character_stats.current_mp < u32::from(definition.mp_cost)
        || character_stats.current_sp < u32::from(definition.sp_cost)
    {
        logger.line(format!(
            "Rapture World rejected skill use: not enough MP/SP skill_id={} character_id={}",
            known_skill.skill_id, character.id
        ));
        return Ok(());
    }

    // The cast-start packet's target is the caster itself for heal-type
    // skills (self-only, see resolve_skill_use's heal branches) and
    // whatever mob the client targeted otherwise (0 for an around-caster
    // AoE with no real target, matching Aries/Imgeneus sending 0 when
    // there's no target).
    let cast_target_id = if definition.type_detail == TYPE_DETAIL_INSTANT_HEAL
        || definition.type_detail == TYPE_DETAIL_PERIODIC_HEAL
        || definition.type_detail == TYPE_DETAIL_BUFF
    {
        character.id
    } else {
        target_mob_id
    };

    if let Some(send) = aes_send.as_mut() {
        send_encrypted(stream, send, use_expanded_send_key, build_auto_attack_stop()?, "0x0212 auto attack stop (casting)", logger)?;
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_cast_started(character.id, cast_target_id, known_skill.skill_id, known_skill.skill_level)?,
            "0x0510 skill cast started",
            logger,
        )?;
    }

    logger.line(format!(
        "Rapture World skill cast started character_id={} skill_id={} level={} cast_time_ms={cast_time_ms}",
        character.id, known_skill.skill_id, known_skill.skill_level
    ));

    *pending_cast = Some(PendingCast {
        target_mob_id,
        number,
        request_opcode,
        ready_at: Instant::now() + Duration::from_millis(cast_time_ms),
    });

    Ok(())
}

/// Checks the connection's single in-progress `PendingCast` (if any) and
/// resolves it via `resolve_skill_use` once `ready_at` has passed. Called
/// once per connection-loop iteration (see main.rs), same as
/// `process_active_heals`. Movement cancels a pending cast directly in
/// main.rs's `CharacterMove` handling (confirmed by Imgeneus's
/// `SkillCastingManager` canceling on move) rather than here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_pending_cast(
    pending_cast: &mut Option<PendingCast>,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[DbCharacterItem],
    active_skills: &[DbCharacterSkill],
    skill_cooldowns: &mut HashMap<u16, Instant>,
    active_heals: &mut Vec<ActiveHeal>,
    active_buffs: &mut Vec<ActiveBuff>,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    let Some(cast) = pending_cast.as_ref() else {
        return Ok(());
    };
    if cast.ready_at > Instant::now() {
        return Ok(());
    }

    let target_mob_id = cast.target_mob_id;
    let number = cast.number;
    let request_opcode = cast.request_opcode;
    *pending_cast = None;

    resolve_skill_use(
        target_mob_id,
        number,
        request_opcode,
        active_character,
        connected_player,
        active_items,
        active_skills,
        skill_cooldowns,
        active_heals,
        active_buffs,
        db_conn,
        stream,
        aes_send,
        use_expanded_send_key,
        logger,
    )
}

/// The actual skill-effect resolution: resolves the skill by its bar slot
/// (`number`), validates MP/SP cost, cooldown, and range, deducts the
/// cost, computes damage (caster's stat-based attack damage + the skill's
/// flat DamageHP/AddDamageHP bonus, matching `basic_auto_attack_damage`'s
/// shape), applies it, and shares kill-consequence handling with
/// auto-attack via `resolve_mob_damage_result`.
///
/// Called either immediately from `handle_use_skill` (when the skill has
/// no cast time), or from `process_pending_cast` once a delayed cast's
/// `ReadyTime` has elapsed -- in both cases every validation here (learned
/// skill, cooldown, MP/SP, mob still alive/in range) is re-checked fresh
/// at the moment of actual resolution, matching Imgeneus's
/// `SkillCastingManager` (`CanUseSkill` is checked both at cast start and
/// at cast completion).
#[allow(clippy::too_many_arguments)]
fn resolve_skill_use(
    target_mob_id: u32,
    number: u8,
    request_opcode: Opcode,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[DbCharacterItem],
    active_skills: &[DbCharacterSkill],
    skill_cooldowns: &mut HashMap<u16, Instant>,
    active_heals: &mut Vec<ActiveHeal>,
    active_buffs: &mut Vec<ActiveBuff>,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    let Some(character) = active_character.clone() else {
        logger.line("Rapture World rejected skill use without active character");
        return Ok(());
    };

    let Some(known_skill) = active_skills.iter().find(|skill| skill.number == number) else {
        logger.line(format!(
            "Rapture World rejected skill use: no learned skill in bar slot number={number} character_id={}",
            character.id
        ));
        return Ok(());
    };

    let Some(definition) =
        db::get_skill_definition(db_conn, known_skill.skill_id, known_skill.skill_level)?
    else {
        logger.line(format!(
            "Rapture World rejected skill use: missing definition skill_id={} level={}",
            known_skill.skill_id, known_skill.skill_level
        ));
        return Ok(());
    };

    if let Some(ready_at) = skill_cooldowns.get(&known_skill.skill_id) {
        if *ready_at > Instant::now() {
            logger.line(format!(
                "Rapture World rejected skill use on cooldown skill_id={} character_id={}",
                known_skill.skill_id, character.id
            ));
            return Ok(());
        }
    }

    // Compared against the EFFECTIVE current MP/SP (raw DB value plus
    // equipment/stat-derived bonuses, i.e. what the client actually
    // displays), not the raw `character.current_mp`/`current_sp` fields --
    // those alone are lower than the displayed total, which made casting
    // lock up well before the visible MP/SP bar looked empty. Also folds
    // in active buff bonuses (e.g. a Str/Int buff raising max MP via
    // WIS) so the check reflects the true current effective pool.
    let equipment_stats = equipment_stats_with_buffs(db::get_equipment_stats(db_conn, active_items)?, active_buffs);
    let character_stats = CharacterStats::from_character(Some(&character), equipment_stats);
    if character_stats.current_mp < u32::from(definition.mp_cost)
        || character_stats.current_sp < u32::from(definition.sp_cost)
    {
        logger.line(format!(
            "Rapture World rejected skill use: not enough MP/SP skill_id={} character_id={}",
            known_skill.skill_id, character.id
        ));
        return Ok(());
    }

    // Stat buff (`TypeDetail == 82`): applies flat AbilityType bonuses for
    // KeepTime seconds instead of anything instant. Self-only, same scope
    // limitation as heals.
    if definition.type_detail == TYPE_DETAIL_BUFF {
        start_buff(
            &character,
            &definition,
            known_skill,
            request_opcode,
            skill_cooldowns,
            active_buffs,
            active_character,
            connected_player,
            active_items,
            db_conn,
            stream,
            aes_send,
            use_expanded_send_key,
            logger,
        )?;
        return Ok(());
    }

    // Periodic heal (`TypeDetail == 81`): starts/refreshes a ticking
    // "heal over time" effect instead of applying anything instantly --
    // still pays the cast's own MP/SP cost and cooldown right away, same
    // as any other skill use. Self-only, same scope limitation as the
    // instant heal below.
    if definition.type_detail == TYPE_DETAIL_PERIODIC_HEAL {
        start_periodic_heal(
            &character,
            &definition,
            known_skill,
            request_opcode,
            skill_cooldowns,
            active_heals,
            active_character,
            connected_player,
            db_conn,
            stream,
            aes_send,
            use_expanded_send_key,
            logger,
        )?;
        return Ok(());
    }

    // Instant heal skills (`TypeDetail == 41`, `HealHP`/`HealSP`/`HealMP`
    // nonzero) are handled entirely separately from the enemy-targeting/
    // damage path below -- there's no mob to look up, range-check, or
    // damage. Scoped to self-heal only for now: ally/party targeting isn't
    // implemented anywhere in this project yet (see the target_type
    // rejection further down), so every heal lands on the caster
    // regardless of its `TargetType` (2=self, 3/5=selected/party in the
    // retail data, but without ally-targeting infra there's no other valid
    // target to use).
    if definition.type_detail == TYPE_DETAIL_INSTANT_HEAL {
        apply_heal_skill(
            &character,
            &definition,
            known_skill,
            request_opcode,
            character_stats,
            skill_cooldowns,
            active_character,
            connected_player,
            active_items,
            db_conn,
            stream,
            aes_send,
            use_expanded_send_key,
            logger,
        )?;
        return Ok(());
    }

    // `EnemiesNearCaster` skills (target_type 6, "cast around yourself")
    // are sent by the client with no real target (target_mob_id == 0) --
    // there's nothing to look up or range-check against a specific mob for
    // those, the caster's own position is the center of the AoE. Every
    // other supported target_type requires resolving (and range-checking
    // against) a real mob target first.
    let targets = if definition.target_type == TARGET_TYPE_ENEMIES_NEAR_CASTER {
        mobs_within_range(character.map_id, character.pos_x, character.pos_z, definition.apply_range)
    } else {
        let Some(mob) = mob_by_id(character.map_id, target_mob_id) else {
            logger.line(format!(
                "Rapture World rejected skill use: unknown mob target_mob_id={target_mob_id} character_id={}",
                character.id
            ));
            return Ok(());
        };
        if mob.current_hp == 0 {
            return Ok(());
        }

        let distance = distance_xz(character.pos_x, character.pos_z, mob.pos_x, mob.pos_z);
        let allowed_range = f32::from(definition.attack_range).max(1.0) + SKILL_RANGE_GRACE;
        if distance > allowed_range {
            logger.line(format!(
                "Rapture World rejected skill use: out of range skill_id={} distance={distance:.2} range={} allowed_range={allowed_range:.2} character_id={}",
                known_skill.skill_id, definition.attack_range, character.id
            ));
            return Ok(());
        }

        match definition.target_type {
            TARGET_TYPE_ANY_ENEMY | TARGET_TYPE_SELECTED_ENEMY => vec![mob.id],
            TARGET_TYPE_ENEMIES_NEAR_TARGET => {
                mobs_within_range(character.map_id, mob.pos_x, mob.pos_z, definition.apply_range)
            }
            other => {
                logger.line(format!(
                    "Rapture World rejected skill use: unsupported target_type={other} skill_id={} character_id={} (self/ally/party skills aren't implemented yet)",
                    known_skill.skill_id, character.id
                ));
                return Ok(());
            }
        }
    };
    // Unlike the enemy-required branches above (which always resolve to at
    // least one real mob or return early), an around-caster AoE with
    // nothing nearby legitimately resolves to zero targets -- the cast
    // still happens (cost/cooldown still apply below, matching native
    // behavior), it just doesn't hit anything.

    let stat_damage = if definition.type_attack == 3 {
        basic_magic_attack_damage(character_stats)
    } else {
        basic_auto_attack_damage(character_stats)
    };
    let main_damage = stat_damage.saturating_add_signed(definition.damage_hp).max(1);

    skill_cooldowns.insert(
        known_skill.skill_id,
        Instant::now() + Duration::from_secs(u64::from(definition.reset_time_seconds)),
    );
    // Persisted too (not just in-memory) so the cooldown survives a relog
    // instead of resetting -- see set_character_skill_cooldown's doc.
    db::set_character_skill_cooldown(
        db_conn,
        character.id,
        known_skill.skill_id,
        definition.reset_time_seconds,
    )?;

    let new_mp = character.current_mp.saturating_sub(u32::from(definition.mp_cost));
    let new_sp = character.current_sp.saturating_sub(u32::from(definition.sp_cost));
    if let Some(active) = active_character.as_mut() {
        active.current_mp = new_mp;
        active.current_sp = new_sp;
        if let Some(player) = connected_player.as_ref() {
            player.update_character(active.clone());
        }
    }
    if let Some(send) = aes_send.as_mut() {
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_skill_used_sp_mp(u32::from(definition.sp_cost), u32::from(definition.mp_cost))?,
            "0x050C skill used sp/mp",
            logger,
        )?;
    }

    logger.line(format!(
        "Rapture World skill use character_id={} skill_id={} level={} target_type={} targets={} main_damage={} mp_cost={} sp_cost={}",
        character.id,
        known_skill.skill_id,
        known_skill.skill_level,
        definition.target_type,
        targets.len(),
        main_damage,
        definition.mp_cost,
        definition.sp_cost,
    ));

    for target_id in targets {
        apply_skill_hit_to_mob(
            target_id,
            main_damage,
            &definition,
            &character,
            &known_skill,
            request_opcode,
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

    Ok(())
}

/// Finds every live mob on `map_id` within `range` of `(x, z)`. No cap on
/// how many can be hit -- the native game doesn't appear to have one
/// either (or if it does, it's high enough not to matter), so `ApplyRange`
/// itself is the only real limiter.
fn mobs_within_range(map_id: u16, x: f32, z: f32, range: u8) -> Vec<u32> {
    let range = f32::from(range).max(1.0) + SKILL_RANGE_GRACE;
    entities_on_map(map_id)
        .mobs
        .into_iter()
        .filter(|mob| mob.current_hp > 0)
        .filter(|mob| distance_xz(x, z, mob.pos_x, mob.pos_z) <= range)
        .map(|mob| mob.id)
        .collect()
}

/// Applies one skill's main hit (and, if it survives, its secondary
/// AddDamageHP hit) to a single mob target, sending the appropriate
/// packets and resolving kill consequences. Shared by both the single-
/// target and AoE cases.
#[allow(clippy::too_many_arguments)]
fn apply_skill_hit_to_mob(
    target_mob_id: u32,
    main_damage: u32,
    definition: &DbSkillDefinition,
    character: &DbCharacter,
    known_skill: &DbCharacterSkill,
    request_opcode: Opcode,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[DbCharacterItem],
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    let Some(main_result) = damage_mob(character.map_id, target_mob_id, main_damage) else {
        return Ok(());
    };

    let response = build_skill_use_result(
        request_opcode,
        SKILL_TARGET_TYPE_MOB,
        character.id,
        main_result.mob.id,
        known_skill.skill_id,
        known_skill.skill_level,
        main_result.damage_hp,
        0,
        0,
    )?;
    if let Some(send) = aes_send.as_mut() {
        send_encrypted(stream, send, use_expanded_send_key, response, "0x0517/0x0511 skill use", logger)?;
    }

    let main_killed = main_result.killed;
    resolve_mob_damage_result(
        &main_result,
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
    )?;

    // Second, independent hit for skills with a flat "extra damage" bonus
    // -- skipped if the main hit already finished the mob off.
    if !main_killed && definition.add_damage_hp > 0 {
        let extra_damage = definition.add_damage_hp.max(0) as u32;
        if let Some(extra_result) = damage_mob(character.map_id, target_mob_id, extra_damage) {
            logger.line(format!(
                "Rapture World skill extra damage character_id={} skill_id={} mob_global_id={} extra_damage={} killed={}",
                character.id,
                known_skill.skill_id,
                extra_result.mob.id,
                extra_result.damage_hp,
                extra_result.killed
            ));

            let extra_response = build_skill_extra_damage_result(
                character.id,
                extra_result.mob.id,
                known_skill.skill_id,
                known_skill.skill_level,
                extra_result.damage_hp,
                0,
                0,
            )?;
            if let Some(send) = aes_send.as_mut() {
                send_encrypted(
                    stream,
                    send,
                    use_expanded_send_key,
                    extra_response,
                    "0x051A skill extra damage",
                    logger,
                )?;
            }

            resolve_mob_damage_result(
                &extra_result,
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
            )?;
        }
    }

    Ok(())
}

/// Applies an instant self-heal skill: restores HP/SP/MP, deducts the
/// skill's own MP/SP cast cost, and starts its cooldown -- all the same
/// bookkeeping `handle_use_skill` does for a damaging skill, just without
/// any mob/range resolution.
///
/// WIS scaling (confirmed by the user against native-client testing):
/// only the HP component of a `TypeAttack == 3` (magic) heal gets a
/// `+ wisdom_total * 2` bonus on top of the skill's base `HealHP` --
/// `TypeAttack != 3` heals (e.g. "Divine Intervention", a buff-tier
/// ability rather than a cast mage/priest spell) restore their `HealHP`
/// flat, and `HealSP`/`HealMP` are always flat regardless of `TypeAttack`.
/// This mirrors the DB evidence: every heal skill with `TypeAttack == 3`
/// is a mage/priest "Healing"/"Instant Heal"/"Recovery"-style spell, while
/// the flat, huge-value heals are all `TypeAttack == 0`.
///
/// Each restore is computed as a delta against the caster's EFFECTIVE
/// (equipment/stat-bonus-inclusive) current/max values from `stats`, then
/// applied as that same delta to the raw DB fields -- this way the clamp
/// to "full" always lines up with the true effective max the client
/// displays, without needing to reverse-engineer the equipment bonus back
/// out of `stats`.
///
/// Confirmed against a real self-heal capture: the ack's `health`/
/// `stamina`/`mana` fields are all sent as **zero** -- this packet does
/// NOT carry the restored amount for a heal (unlike a damage skill, where
/// `health` carries the damage dealt). The client apparently gets the
/// actual restored amount from elsewhere (likely a `0x0521` current-
/// hitpoints refresh, which this function already sends). The ack's own
/// opcode also mirrors whichever opcode the request came in on (see
/// module doc) rather than always using `Opcode::SkillUse`.
#[allow(clippy::too_many_arguments)]
fn apply_heal_skill(
    character: &DbCharacter,
    definition: &DbSkillDefinition,
    known_skill: &DbCharacterSkill,
    request_opcode: Opcode,
    stats: CharacterStats,
    skill_cooldowns: &mut HashMap<u16, Instant>,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[DbCharacterItem],
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    skill_cooldowns.insert(
        known_skill.skill_id,
        Instant::now() + Duration::from_secs(u64::from(definition.reset_time_seconds)),
    );
    db::set_character_skill_cooldown(
        db_conn,
        character.id,
        known_skill.skill_id,
        definition.reset_time_seconds,
    )?;

    let heal_hp = if definition.type_attack == 3 {
        (definition.heal_hp.max(0) as u32).saturating_add(stats.wisdom.saturating_mul(2))
    } else {
        definition.heal_hp.max(0) as u32
    };
    let heal_sp = definition.heal_sp.max(0) as u32;
    let heal_mp = definition.heal_mp.max(0) as u32;

    let new_effective_hp = stats.current_hp.saturating_add(heal_hp).min(stats.max_hp);
    let new_effective_sp = stats.current_sp.saturating_add(heal_sp).min(stats.max_sp);
    let new_effective_mp = stats.current_mp.saturating_add(heal_mp).min(stats.max_mp);
    let actual_heal_hp = new_effective_hp - stats.current_hp;
    let actual_heal_sp = new_effective_sp - stats.current_sp;
    let actual_heal_mp = new_effective_mp - stats.current_mp;

    let new_hp = character.current_hp.saturating_add(actual_heal_hp);
    let new_mp = character
        .current_mp
        .saturating_add(actual_heal_mp)
        .saturating_sub(u32::from(definition.mp_cost));
    let new_sp = character
        .current_sp
        .saturating_add(actual_heal_sp)
        .saturating_sub(u32::from(definition.sp_cost));

    db::set_character_status(db_conn, character.id, new_hp, new_mp, new_sp)?;
    if let Some(active) = active_character.as_mut() {
        active.current_hp = new_hp;
        active.current_mp = new_mp;
        active.current_sp = new_sp;
        if let Some(player) = connected_player.as_ref() {
            player.update_character(active.clone());
        }
    }
    if let Some(send) = aes_send.as_mut() {
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_skill_used_sp_mp(u32::from(definition.sp_cost), u32::from(definition.mp_cost))?,
            "0x050C skill used sp/mp",
            logger,
        )?;
    }

    logger.line(format!(
        "Rapture World skill heal character_id={} skill_id={} level={} heal_hp={actual_heal_hp} heal_sp={actual_heal_sp} heal_mp={actual_heal_mp} mp_cost={} sp_cost={}",
        character.id, known_skill.skill_id, known_skill.skill_level, definition.mp_cost, definition.sp_cost,
    ));

    let response = build_skill_use_result(
        request_opcode,
        SKILL_TARGET_TYPE_SELF,
        character.id,
        character.id,
        known_skill.skill_id,
        known_skill.skill_level,
        0,
        0,
        0,
    )?;
    if let Some(send) = aes_send.as_mut() {
        send_encrypted(stream, send, use_expanded_send_key, response, "0x0517/0x0511 skill heal", logger)?;

        let equipment_stats = db::get_equipment_stats(db_conn, active_items)?;
        let refreshed_stats = CharacterStats::from_character(active_character.as_ref(), equipment_stats);
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_current_hitpoints(&refreshed_stats)?,
            "0x0521 current hp/mp/sp after heal",
            logger,
        )?;
    }

    Ok(())
}

/// Starts (or refreshes, if the same skill is already ticking) a periodic
/// heal-over-time effect on the caster: pays the cast's own MP/SP cost and
/// starts its cooldown immediately, same as any other skill use, but
/// doesn't restore anything until the first 3-second tick (see
/// `process_active_heals`). `TimeHealHP/SP/MP` never scale with WIS
/// (confirmed by the user), unlike instant `HealHP`.
#[allow(clippy::too_many_arguments)]
fn start_periodic_heal(
    character: &DbCharacter,
    definition: &DbSkillDefinition,
    known_skill: &DbCharacterSkill,
    request_opcode: Opcode,
    skill_cooldowns: &mut HashMap<u16, Instant>,
    active_heals: &mut Vec<ActiveHeal>,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    skill_cooldowns.insert(
        known_skill.skill_id,
        Instant::now() + Duration::from_secs(u64::from(definition.reset_time_seconds)),
    );
    db::set_character_skill_cooldown(
        db_conn,
        character.id,
        known_skill.skill_id,
        definition.reset_time_seconds,
    )?;

    let new_mp = character.current_mp.saturating_sub(u32::from(definition.mp_cost));
    let new_sp = character.current_sp.saturating_sub(u32::from(definition.sp_cost));
    db::set_character_status(db_conn, character.id, character.current_hp, new_mp, new_sp)?;
    if let Some(active) = active_character.as_mut() {
        active.current_mp = new_mp;
        active.current_sp = new_sp;
        if let Some(player) = connected_player.as_ref() {
            player.update_character(active.clone());
        }
    }
    if let Some(send) = aes_send.as_mut() {
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_skill_used_sp_mp(u32::from(definition.sp_cost), u32::from(definition.mp_cost))?,
            "0x050C skill used sp/mp",
            logger,
        )?;
    }

    let now = Instant::now();
    active_heals.retain(|heal| heal.skill_id != known_skill.skill_id);
    active_heals.push(ActiveHeal {
        skill_id: known_skill.skill_id,
        skill_level: known_skill.skill_level,
        response_opcode: request_opcode,
        ends_at: now + Duration::from_secs(u64::from(definition.keep_time)),
        next_tick_at: now + PERIODIC_HEAL_TICK,
        heal_hp: definition.time_heal_hp.max(0) as u32,
        heal_sp: definition.time_heal_sp.max(0) as u32,
        heal_mp: definition.time_heal_mp.max(0) as u32,
    });

    logger.line(format!(
        "Rapture World skill periodic heal started character_id={} skill_id={} level={} keep_time={} tick_hp={} tick_sp={} tick_mp={} mp_cost={} sp_cost={}",
        character.id,
        known_skill.skill_id,
        known_skill.skill_level,
        definition.keep_time,
        definition.time_heal_hp,
        definition.time_heal_sp,
        definition.time_heal_mp,
        definition.mp_cost,
        definition.sp_cost,
    ));

    // Ack the cast itself with a zero-amount result -- nothing is restored
    // until the first tick fires.
    let response = build_skill_use_result(
        request_opcode,
        SKILL_TARGET_TYPE_SELF,
        character.id,
        character.id,
        known_skill.skill_id,
        known_skill.skill_level,
        0,
        0,
        0,
    )?;
    if let Some(send) = aes_send.as_mut() {
        send_encrypted(stream, send, use_expanded_send_key, response, "0x0517/0x0511 skill periodic heal cast", logger)?;
    }

    Ok(())
}

/// Applies every due tick of every active periodic heal on the caller's
/// own character, and drops any effect whose duration has expired. Called
/// once per connection-loop iteration (see main.rs) so a 3-second cadence
/// is kept regardless of whether the client is otherwise sending packets --
/// the same approach already used for auto-attack swings.
///
/// Not yet confirmed against a real capture: the per-tick ack reuses
/// 0x0517 (like the instant heal) since periodic ticks have never been
/// captured, and a 0x0521 current-hitpoints refresh is sent alongside each
/// tick to make sure the client's bars visibly move.
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_active_heals(
    active_heals: &mut Vec<ActiveHeal>,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[DbCharacterItem],
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    if active_heals.is_empty() {
        return Ok(());
    }

    let now = Instant::now();
    active_heals.retain(|heal| heal.ends_at > now);
    if active_heals.is_empty() {
        return Ok(());
    }

    if active_character.is_none() {
        return Ok(());
    }
    let character_id = active_character.as_ref().unwrap().id;

    // Re-reads `active_character` fresh on every iteration (rather than
    // working off one clone taken before the loop) so that a second tick
    // due in the same pass -- either a different active heal, or this same
    // one on an unusually long-delayed loop wakeup -- sees the first
    // tick's already-applied HP/MP/SP instead of stale pre-loop values.
    for index in 0..active_heals.len() {
        if active_heals[index].next_tick_at > now {
            continue;
        }
        active_heals[index].next_tick_at += PERIODIC_HEAL_TICK;

        let equipment_stats = db::get_equipment_stats(db_conn, active_items)?;
        let stats = CharacterStats::from_character(active_character.as_ref(), equipment_stats);

        let heal = &active_heals[index];
        let new_effective_hp = stats.current_hp.saturating_add(heal.heal_hp).min(stats.max_hp);
        let new_effective_sp = stats.current_sp.saturating_add(heal.heal_sp).min(stats.max_sp);
        let new_effective_mp = stats.current_mp.saturating_add(heal.heal_mp).min(stats.max_mp);
        let actual_heal_hp = new_effective_hp - stats.current_hp;
        let actual_heal_sp = new_effective_sp - stats.current_sp;
        let actual_heal_mp = new_effective_mp - stats.current_mp;
        let skill_id = heal.skill_id;
        let skill_level = heal.skill_level;
        let response_opcode = heal.response_opcode;

        let current = active_character.as_ref().unwrap();
        let new_hp = current.current_hp.saturating_add(actual_heal_hp);
        let new_mp = current.current_mp.saturating_add(actual_heal_mp);
        let new_sp = current.current_sp.saturating_add(actual_heal_sp);

        db::set_character_status(db_conn, character_id, new_hp, new_mp, new_sp)?;
        if let Some(active) = active_character.as_mut() {
            active.current_hp = new_hp;
            active.current_mp = new_mp;
            active.current_sp = new_sp;
            if let Some(player) = connected_player.as_ref() {
                player.update_character(active.clone());
            }
        }

        logger.line(format!(
            "Rapture World skill periodic heal tick character_id={character_id} skill_id={skill_id} level={skill_level} heal_hp={actual_heal_hp} heal_sp={actual_heal_sp} heal_mp={actual_heal_mp}"
        ));

        // Zero health/stamina/mana here too, matching the confirmed
        // instant-heal capture (this ack doesn't carry the restored
        // amount) -- the 0x0521 refresh right below is what actually
        // moves the client's bars.
        let response = build_skill_use_result(
            response_opcode,
            SKILL_TARGET_TYPE_SELF,
            character_id,
            character_id,
            skill_id,
            skill_level,
            0,
            0,
            0,
        )?;
        if let Some(send) = aes_send.as_mut() {
            send_encrypted(stream, send, use_expanded_send_key, response, "0x0517/0x0511 skill periodic heal tick", logger)?;

            let equipment_stats = db::get_equipment_stats(db_conn, active_items)?;
            let refreshed_stats = CharacterStats::from_character(active_character.as_ref(), equipment_stats);
            send_encrypted(
                stream,
                send,
                use_expanded_send_key,
                build_current_hitpoints(&refreshed_stats)?,
                "0x0521 current hp/mp/sp after periodic heal tick",
                logger,
            )?;
        }
    }

    Ok(())
}

/// Turns one `AbilityType`/`AbilityValue` pair into the matching
/// `ActiveBuff` delta field, if it's one of the flat-stat types this first
/// pass understands. Anything else is logged and ignored (matching
/// Imgeneus's own `ApplyAbility` default case, which does the same for
/// ability types it hasn't implemented).
fn apply_ability_to_buff(buff: &mut ActiveBuff, ability_type: u8, ability_value: i16, logger: &Logger) {
    // Accumulates (matching Imgeneus's own `ApplyAbility`, which does
    // `+=`) rather than overwriting -- a skill could in principle use the
    // same AbilityType across more than one of its 10 slots, and each
    // should stack rather than the last one silently winning. No known
    // skill in the live DB currently does this, but this is the correct
    // behavior regardless.
    let value = ability_value.max(0) as u32;
    match ability_type {
        0 => {}
        ABILITY_HP => buff.max_hp = buff.max_hp.saturating_add(value),
        ABILITY_MP => buff.max_mp = buff.max_mp.saturating_add(value),
        ABILITY_SP => buff.max_sp = buff.max_sp.saturating_add(value),
        ABILITY_STR => buff.strength = buff.strength.saturating_add(value),
        ABILITY_REC => buff.reaction = buff.reaction.saturating_add(value),
        ABILITY_INT => buff.intelligence = buff.intelligence.saturating_add(value),
        ABILITY_WIS => buff.wisdom = buff.wisdom.saturating_add(value),
        ABILITY_DEX => buff.dexterity = buff.dexterity.saturating_add(value),
        ABILITY_LUC => buff.luck = buff.luck.saturating_add(value),
        other => logger.line(format!(
            "Rapture World buff ability_type={other} not implemented yet (value={ability_value}), ignored"
        )),
    }
}

/// Starts (or refreshes) a stat buff on the caster: pays the cast's own
/// MP/SP cost and cooldown immediately, applies the skill's flat
/// `AbilityType`/`AbilityValue` bonuses for `KeepTime` seconds, and
/// refreshes the client's stat display packets. Self-only (see module
/// doc); UNCONFIRMED against a real capture (see `Opcode::BuffAdd`).
#[allow(clippy::too_many_arguments)]
fn start_buff(
    character: &DbCharacter,
    definition: &DbSkillDefinition,
    known_skill: &DbCharacterSkill,
    request_opcode: Opcode,
    skill_cooldowns: &mut HashMap<u16, Instant>,
    active_buffs: &mut Vec<ActiveBuff>,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[DbCharacterItem],
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    skill_cooldowns.insert(
        known_skill.skill_id,
        Instant::now() + Duration::from_secs(u64::from(definition.reset_time_seconds)),
    );
    db::set_character_skill_cooldown(
        db_conn,
        character.id,
        known_skill.skill_id,
        definition.reset_time_seconds,
    )?;

    let new_mp = character.current_mp.saturating_sub(u32::from(definition.mp_cost));
    let new_sp = character.current_sp.saturating_sub(u32::from(definition.sp_cost));
    db::set_character_status(db_conn, character.id, character.current_hp, new_mp, new_sp)?;
    if let Some(active) = active_character.as_mut() {
        active.current_mp = new_mp;
        active.current_sp = new_sp;
        if let Some(player) = connected_player.as_ref() {
            player.update_character(active.clone());
        }
    }
    if let Some(send) = aes_send.as_mut() {
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_skill_used_sp_mp(u32::from(definition.sp_cost), u32::from(definition.mp_cost))?,
            "0x050C skill used sp/mp",
            logger,
        )?;
    }

    // Re-casting the same skill reuses its existing `id` rather than
    // minting a new one -- the client tracks buffs by this id (see
    // `Opcode::BuffAdd`'s doc), so treating a refresh as remove-then-add
    // with a fresh id would orphan the old id's icon client-side (we'd
    // never send a matching BUFF_REMOVE for it) while a second one
    // appeared, instead of the same icon just resetting its countdown.
    let buff_id = active_buffs
        .iter()
        .find(|existing| existing.skill_id == known_skill.skill_id)
        .map(|existing| existing.id)
        .unwrap_or_else(|| active_buffs.iter().map(|buff| buff.id).max().unwrap_or(0) + 1);
    let mut buff = ActiveBuff {
        id: buff_id,
        skill_id: known_skill.skill_id,
        skill_level: known_skill.skill_level,
        ends_at: Instant::now() + Duration::from_secs(u64::from(definition.keep_time)),
        strength: 0,
        reaction: 0,
        intelligence: 0,
        wisdom: 0,
        dexterity: 0,
        luck: 0,
        max_hp: 0,
        max_mp: 0,
        max_sp: 0,
    };
    for index in 0..10 {
        apply_ability_to_buff(&mut buff, definition.ability_type[index], definition.ability_value[index], logger);
    }

    active_buffs.retain(|existing| existing.skill_id != known_skill.skill_id);
    active_buffs.push(buff);

    logger.line(format!(
        "Rapture World skill buff started character_id={} skill_id={} level={} keep_time={}",
        character.id, known_skill.skill_id, known_skill.skill_level, definition.keep_time
    ));

    let response = build_skill_use_result(
        request_opcode,
        SKILL_TARGET_TYPE_SELF,
        character.id,
        character.id,
        known_skill.skill_id,
        known_skill.skill_level,
        0,
        0,
        0,
    )?;
    if let Some(send) = aes_send.as_mut() {
        send_encrypted(stream, send, use_expanded_send_key, response, "0x0517/0x0511 skill buff cast", logger)?;
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_buff_add(buff_id, known_skill.skill_id, known_skill.skill_level, definition.keep_time.into())?,
            "0x050D buff add",
            logger,
        )?;

        let equipment_stats = equipment_stats_with_buffs(db::get_equipment_stats(db_conn, active_items)?, active_buffs);
        let refreshed_stats = CharacterStats::from_character(active_character.as_ref(), equipment_stats);
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_character_additional_stats(&refreshed_stats)?,
            "0x0526 additional stats after buff",
            logger,
        )?;
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_max_hp_mp_sp(active_character.as_ref(), &refreshed_stats)?,
            "0x051F max hp/mp/sp after buff",
            logger,
        )?;
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_current_hitpoints(&refreshed_stats)?,
            "0x0521 current hp/mp/sp after buff",
            logger,
        )?;
    }

    Ok(())
}

/// Expires any active buff whose duration has ended, removing its stat
/// bonuses and refreshing the client's stat packets. Called once per
/// connection-loop iteration (see main.rs), same as `process_active_heals`
/// and `process_pending_cast`.
pub(crate) fn process_active_buffs(
    active_buffs: &mut Vec<ActiveBuff>,
    active_character: &mut Option<DbCharacter>,
    active_items: &[DbCharacterItem],
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    logger: &Logger,
) -> Result<()> {
    if active_buffs.is_empty() {
        return Ok(());
    }

    let now = Instant::now();
    let expired: Vec<ActiveBuff> = {
        let mut expired = Vec::new();
        let mut index = 0;
        while index < active_buffs.len() {
            if active_buffs[index].ends_at <= now {
                expired.push(active_buffs.remove(index));
            } else {
                index += 1;
            }
        }
        expired
    };
    if expired.is_empty() {
        return Ok(());
    }

    let Some(character) = active_character.as_ref() else {
        return Ok(());
    };
    let character_id = character.id;

    for buff in &expired {
        logger.line(format!(
            "Rapture World skill buff expired character_id={character_id} skill_id={} level={}",
            buff.skill_id, buff.skill_level
        ));
    }

    if let Some(send) = aes_send.as_mut() {
        for buff in &expired {
            send_encrypted(stream, send, use_expanded_send_key, build_buff_remove(buff.id)?, "0x050E buff remove", logger)?;
        }

        let equipment_stats = equipment_stats_with_buffs(db::get_equipment_stats(db_conn, active_items)?, active_buffs);
        let refreshed_stats = CharacterStats::from_character(active_character.as_ref(), equipment_stats);
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_character_additional_stats(&refreshed_stats)?,
            "0x0526 additional stats after buff expiry",
            logger,
        )?;
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_max_hp_mp_sp(active_character.as_ref(), &refreshed_stats)?,
            "0x051F max hp/mp/sp after buff expiry",
            logger,
        )?;
        send_encrypted(
            stream,
            send,
            use_expanded_send_key,
            build_current_hitpoints(&refreshed_stats)?,
            "0x0521 current hp/mp/sp after buff expiry",
            logger,
        )?;
    }

    Ok(())
}

/// Persists every active buff's remaining seconds to `CharApplySkills`
/// (confirmed by the user to be the retail table for this). Buffs freeze
/// while offline instead of continuing to count down (confirmed by the
/// user, the opposite of skill cooldowns), so a plain remaining-seconds
/// snapshot at the moment of logout/disconnect is all that's needed --
/// no timestamp/DATEDIFF trick required.
pub(crate) fn persist_active_buffs(
    db_conn: &mut db::Db,
    character_id: u32,
    active_buffs: &[ActiveBuff],
) -> Result<()> {
    let now = Instant::now();
    for buff in active_buffs {
        let remaining = buff.ends_at.saturating_duration_since(now).as_secs().min(u64::from(u32::MAX)) as u32;
        db::set_character_apply_skill(db_conn, character_id, buff.skill_id, buff.skill_level, remaining)?;
    }
    Ok(())
}

/// Restores active buffs from `CharApplySkills` at character-select.
/// Only `SkillID`/`SkillLevel`/`LeftResetTime` are persisted, so each
/// buff's stat deltas are recomputed fresh from its current skill
/// definition (same as a live cast would). Each restored row is deleted
/// immediately after being read -- the in-memory `ActiveBuff` it becomes
/// is the sole source of truth from this point on, and gets re-persisted
/// fresh (or not at all, if it expires online first) at the next
/// logout/disconnect.
pub(crate) fn restore_active_buffs(
    db_conn: &mut db::Db,
    character_id: u32,
    logger: &Logger,
) -> Result<Vec<ActiveBuff>> {
    let rows = db::get_character_apply_skills(db_conn, character_id)?;
    let now = Instant::now();
    let mut active_buffs = Vec::new();
    for row in rows {
        db::clear_character_apply_skill(db_conn, character_id, row.skill_id)?;

        let Some(definition) = db::get_skill_definition(db_conn, row.skill_id, row.skill_level)? else {
            continue;
        };
        if definition.type_detail != TYPE_DETAIL_BUFF {
            continue;
        }

        let mut buff = ActiveBuff {
            id: active_buffs.len() as u32 + 1,
            skill_id: row.skill_id,
            skill_level: row.skill_level,
            ends_at: now + Duration::from_secs(u64::from(row.left_reset_time_seconds)),
            strength: 0,
            reaction: 0,
            intelligence: 0,
            wisdom: 0,
            dexterity: 0,
            luck: 0,
            max_hp: 0,
            max_mp: 0,
            max_sp: 0,
        };
        for index in 0..10 {
            apply_ability_to_buff(&mut buff, definition.ability_type[index], definition.ability_value[index], logger);
        }
        logger.line(format!(
            "Rapture World restored buff character_id={character_id} skill_id={} level={} remaining={}s",
            row.skill_id, row.skill_level, row.left_reset_time_seconds
        ));
        active_buffs.push(buff);
    }
    Ok(active_buffs)
}
