use anyhow::Result;
use rapture_common::db::{self, DbCharacterSkill, DbQuickSlot};
use rapture_common::packet::{Opcode, PacketWriter, read_le_u16};

use crate::character::*;
use crate::session::*;

pub(crate) fn build_character_skills(skills: &[DbCharacterSkill]) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterSkills);
    packet.write_u16(0); // skill points
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

pub(crate) fn is_valid_quick_slot(quick_slot: &DbQuickSlot) -> bool {
    quick_slot.quick_bar < 4
        && quick_slot.quick_slot < 12
        && ((quick_slot.bag <= INVENTORY_LAST_BAG && quick_slot.number < 24)
            || (quick_slot.bag == 0 && quick_slot.number > 0))
}

pub(crate) fn build_learn_skill_result(ok: bool, skill: &DbCharacterSkill) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::LearnNewSkill);
    packet.write_u8(if ok { 0 } else { 1 });
    packet.write_u8(skill.number);
    packet.write_u16(skill.skill_id);
    packet.write_u8(skill.skill_level);
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

/// Handles 0x0209 LearnNewSkill: persists the learned skill, keeps
/// `active_skills` in sync (update in place if already known, else insert
/// and re-sort by bar number), and returns the 0x0209 ack packet to send.
pub(crate) fn handle_learn_new_skill(
    db_conn: &mut db::Db,
    character_id: u32,
    skill_id: u16,
    skill_level: u8,
    active_skills: &mut Vec<DbCharacterSkill>,
    logger: &Logger,
) -> Result<Vec<u8>> {
    let learned_skill = db::learn_character_skill(db_conn, character_id, skill_id, skill_level)?;
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
        "Rapture World learned skill number={} skill_id={} level={}",
        learned_skill.number, learned_skill.skill_id, learned_skill.skill_level
    ));
    build_learn_skill_result(true, &learned_skill)
}
