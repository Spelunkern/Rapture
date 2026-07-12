use anyhow::Result;
use rapture_common::db::{self, DbCharacter, DbCharacterSkill, DbQuickSlot};
use rapture_common::packet::{Opcode, PacketWriter, read_le_u16};

use crate::character::*;
use crate::session::*;

/// Confirmed byte-for-byte against a real capture (10 skills, cross-checked
/// against the matching `CharSkills` DB rows): `count:u8` followed directly
/// by `count` entries of `skill_id:u16, skill_level:u8, number:u8,
/// delay:u32` (8 bytes each) -- there is NO leading "skill points" field.
/// A previous version wrote one anyway, shifting every subsequent byte by 2
/// and making the client silently fail to parse the whole list -- this is
/// why a just-learned skill showed correctly via the live 0x0209 ack (a
/// separate, unaffected packet) but the full list always looked empty again
/// after a relog, when only this packet is sent.
pub(crate) fn build_character_skills(skills: &[DbCharacterSkill]) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterSkills);
    let skills = &skills[..skills.len().min(255)];
    packet.write_u8(skills.len() as u8);
    for skill in skills {
        packet.write_u16(skill.skill_id);
        packet.write_u8(skill.skill_level);
        packet.write_u8(skill.number);
        packet.write_i32(skill.delay);
    }
    packet.into_bytes()
}

pub(crate) fn build_character_skill_bar(quick_slots: &[DbQuickSlot]) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterSkillBar);
    let quick_slots = quick_slots
        .iter()
        .filter(|quick_slot| is_valid_quick_slot(quick_slot))
        .take(128)
        .collect::<Vec<_>>();
    packet.write_u8(quick_slots.len() as u8);
    for quick_slot in quick_slots {
        packet.write_u8(quick_slot.quick_bar);
        packet.write_u8(quick_slot.quick_slot);
        packet.write_u8(quick_slot.bag);
        packet.write_u16(quick_slot.number);
    }
    packet.into_bytes()
}

/// Shaiya has 5 skill bars of 10 slots each (the "revolver" quick-cast bar
/// and the small 2-slot consumable bar are separate and not covered here).
pub(crate) const SKILL_BAR_COUNT: u8 = 5;
pub(crate) const SKILL_BAR_SLOT_COUNT: u8 = 10;

/// `Bag` sentinel the real client uses on a skill-bar entry to mean "this
/// slot holds a skill (Number = SkillID), not an item" -- confirmed via a
/// real capture. Previously assumed to be 0, which made every real client
/// skill-bar save get silently rejected by `is_valid_quick_slot` (0 doesn't
/// match either the item-bag range or this sentinel), so nothing ever
/// persisted despite the save/load DB plumbing already being correct.
/// Coincidentally the same numeric value as `warehouse::WAREHOUSE_BAG`, but
/// that's an unrelated bag-id namespace (inventory items vs. quick slots)
/// -- not the same concept, don't collapse them into one constant.
pub(crate) const QUICK_SLOT_SKILL_BAG: u8 = 100;

pub(crate) fn is_valid_quick_slot(quick_slot: &DbQuickSlot) -> bool {
    quick_slot.quick_bar < SKILL_BAR_COUNT
        && quick_slot.quick_slot < SKILL_BAR_SLOT_COUNT
        && ((quick_slot.bag <= INVENTORY_LAST_BAG && quick_slot.number < 24)
            || (quick_slot.bag == QUICK_SLOT_SKILL_BAG && quick_slot.number > 0))
}

pub(crate) fn build_learn_skill_result(
    ok: bool,
    number: u8,
    skill_id: u16,
    skill_level: u8,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::LearnNewSkill);
    packet.write_u8(if ok { 0 } else { 1 });
    packet.write_u8(number);
    packet.write_u16(skill_id);
    packet.write_u8(skill_level);
    packet.into_bytes()
}

pub(crate) fn parse_skill_bar(payload: &[u8], logger: &Logger) -> Result<Vec<DbQuickSlot>> {
    if payload.len() < 3 {
        logger.line(format!(
            "Rapture World skill bar packet too short len={}",
            payload.len()
        ));
        return Ok(Vec::new());
    }

    let count = payload[2].min(128);
    logger.line(format!("Rapture World skill bar save count={count}"));

    let mut quick_slots = Vec::new();
    let mut offset = 3usize;
    for index in 0..count {
        if payload.len() < offset + 5 {
            logger.line(format!(
                "Rapture World skill bar item {index} truncated at offset={offset}"
            ));
            break;
        }

        let quick_bar = payload[offset];
        let quick_slot = payload[offset + 1];
        let bag = payload[offset + 2];
        let number = read_le_u16(&payload[offset + 3..offset + 5])?;
        let quick_slot = DbQuickSlot {
            quick_bar,
            quick_slot,
            bag,
            number,
        };
        logger.line(format!(
            "Rapture World skill bar item {index}: bar={quick_bar} slot={} bag={bag} number={number}",
            quick_slot.quick_slot
        ));
        if is_valid_quick_slot(&quick_slot) {
            quick_slots.push(quick_slot);
        } else {
            logger.line(format!(
                "Rapture World ignored invalid skill bar item {index}: bar={quick_bar} slot={} bag={bag} number={number}",
                quick_slot.quick_slot
            ));
        }
        offset += 5;
    }

    Ok(quick_slots)
}

/// Handles 0x0209 LearnNewSkill: validates the request against
/// `PS_GameDefs.dbo.Skills` (class eligibility, character level, level
/// progression -- must already know the same skill one level down before
/// learning the next, prerequisite skill via `PrevSkillID`, and skill point
/// cost) before persisting anything. Previously this trusted the client's
/// request completely (any skill_id/skill_level, no cost deducted), which
/// amounted to an unintentional cheat.
///
/// Returns the 0x0209 ack packet to send, plus the character's new
/// `skill_point` balance if this call spent any (`None` on rejection or if
/// the definition somehow has zero cost).
pub(crate) fn handle_learn_new_skill(
    db_conn: &mut db::Db,
    character: &DbCharacter,
    skill_id: u16,
    skill_level: u8,
    active_skills: &mut Vec<DbCharacterSkill>,
    logger: &Logger,
) -> Result<(Vec<u8>, Option<u16>)> {
    let reject = |logger: &Logger, reason: &str| -> Result<(Vec<u8>, Option<u16>)> {
        logger.line(format!(
            "Rapture World rejected learn skill skill_id={skill_id} level={skill_level} character_id={}: {reason}",
            character.id
        ));
        Ok((build_learn_skill_result(false, 0, skill_id, skill_level)?, None))
    };

    let Some(definition) = db::get_skill_definition(db_conn, skill_id, skill_level)? else {
        return reject(logger, "no such skill/level in PS_GameDefs.Skills");
    };

    let Some(&eligible) = definition
        .class_eligible
        .get(character.character_class as usize)
    else {
        return reject(logger, "character_class out of range");
    };
    if !eligible {
        return reject(logger, "class not eligible for this skill");
    }

    if character.level < definition.req_level {
        return reject(logger, "character level too low");
    }

    if skill_level > 1 {
        let has_prev_level = active_skills
            .iter()
            .any(|skill| skill.skill_id == skill_id && skill.skill_level == skill_level - 1);
        if !has_prev_level {
            return reject(logger, "must already know the previous level of this skill");
        }
    } else if active_skills.iter().any(|skill| skill.skill_id == skill_id) {
        return reject(logger, "already known");
    }

    if definition.prev_skill_id != 0
        && !active_skills
            .iter()
            .any(|skill| skill.skill_id == definition.prev_skill_id)
    {
        return reject(logger, "missing prerequisite skill");
    }

    let cost = u16::from(definition.skill_point_cost);
    if character.skill_point < cost {
        return reject(logger, "not enough skill points");
    }

    let new_skill_point = character.skill_point - cost;
    if cost > 0 {
        db::set_character_attribute(
            db_conn,
            character.id,
            db::CharAttribute::SkillPoint,
            i64::from(new_skill_point),
        )?;
    }

    let learned_skill = db::learn_character_skill(db_conn, character.id, skill_id, skill_level)?;
    if let Some(existing) = active_skills
        .iter_mut()
        .find(|skill| skill.skill_id == learned_skill.skill_id)
    {
        *existing = learned_skill.clone();
    } else {
        active_skills.push(learned_skill.clone());
        active_skills.sort_by_key(|skill| skill.number);
    }
    logger.line(format!(
        "Rapture World learned skill number={} skill_id={} level={} cost={cost} remaining_skill_point={new_skill_point}",
        learned_skill.number, learned_skill.skill_id, learned_skill.skill_level
    ));
    let response = build_learn_skill_result(
        true,
        learned_skill.number,
        learned_skill.skill_id,
        learned_skill.skill_level,
    )?;
    Ok((response, Some(new_skill_point)))
}
