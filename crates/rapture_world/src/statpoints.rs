//! Assigning stat points (0x0208) to STR/DEX/REC/INT/WIS/LUC.
//!
//! Wire format confirmed against a real capture: a 12-byte payload of six
//! u16 deltas, in order `str, dex, rec, int, wis, luc`. The ack simply
//! echoes back the deltas that were applied. REC/DEX/WIS also trigger a
//! live 0x050B CharacterMaxEnergy update (hitpoint_type 0=hp, 1=sp, 2=mp)
//! since they feed derived max hp/sp/mp -- confirmed against the same
//! capture (each point moved the corresponding max stat by exactly the
//! project's existing HP_PER_REACTION/SP_PER_DEXTERITY/MP_PER_WISDOM=5).
//!
//! No respec/reset support -- not needed for this pass.

use anyhow::Result;
use rapture_common::db::{self, CharAttribute, DbCharacter, DbEquipmentStats};
use rapture_common::packet::{Opcode, PacketWriter, read_le_u16};

use crate::character::CharacterStats;
use crate::session::Logger;

fn build_stat_point_result(deltas: [u16; 6]) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::SetStatPoint);
    for delta in deltas {
        packet.write_u16(delta);
    }
    packet.into_bytes()
}

pub(crate) struct StatPointOutcome {
    pub(crate) response: Vec<u8>,
    /// (hitpoint_type, value) pairs for any 0x050B updates to send.
    pub(crate) max_energy_packets: Vec<(u8, u32)>,
}

/// Handles 0x0208: applies the requested per-stat deltas (rejecting the
/// whole request, with nothing persisted, if the total exceeds the
/// character's available `stat_point`), mutates `character` in place, and
/// returns the ack plus any derived max hp/sp/mp updates to send.
pub(crate) fn handle_apply_stat_points(
    db_conn: &mut db::Db,
    character: &mut DbCharacter,
    equipment: DbEquipmentStats,
    payload: &[u8],
    logger: &Logger,
) -> Result<Option<StatPointOutcome>> {
    if payload.len() < 14 {
        logger.line(format!(
            "Rapture World set stat point packet too short len={}",
            payload.len()
        ));
        return Ok(None);
    }

    let str_delta = read_le_u16(&payload[2..4])?;
    let dex_delta = read_le_u16(&payload[4..6])?;
    let rec_delta = read_le_u16(&payload[6..8])?;
    let int_delta = read_le_u16(&payload[8..10])?;
    let wis_delta = read_le_u16(&payload[10..12])?;
    let luc_delta = read_le_u16(&payload[12..14])?;

    let total = u32::from(str_delta)
        + u32::from(dex_delta)
        + u32::from(rec_delta)
        + u32::from(int_delta)
        + u32::from(wis_delta)
        + u32::from(luc_delta);
    if total == 0 {
        logger.line("Rapture World rejected stat point request with no deltas");
        return Ok(None);
    }
    if total > u32::from(character.stat_point) {
        logger.line(format!(
            "Rapture World rejected stat point request character_id={}: total={total} exceeds available={}",
            character.id, character.stat_point
        ));
        return Ok(None);
    }

    if str_delta > 0 {
        character.strength = character.strength.saturating_add(str_delta);
        db::set_character_attribute(
            db_conn,
            character.id,
            CharAttribute::Str,
            i64::from(character.strength),
        )?;
    }
    if dex_delta > 0 {
        character.dexterity = character.dexterity.saturating_add(dex_delta);
        db::set_character_attribute(
            db_conn,
            character.id,
            CharAttribute::Dex,
            i64::from(character.dexterity),
        )?;
    }
    if rec_delta > 0 {
        character.reaction = character.reaction.saturating_add(rec_delta);
        db::set_character_attribute(
            db_conn,
            character.id,
            CharAttribute::Rec,
            i64::from(character.reaction),
        )?;
    }
    if int_delta > 0 {
        character.intelligence = character.intelligence.saturating_add(int_delta);
        db::set_character_attribute(
            db_conn,
            character.id,
            CharAttribute::Int,
            i64::from(character.intelligence),
        )?;
    }
    if wis_delta > 0 {
        character.wisdom = character.wisdom.saturating_add(wis_delta);
        db::set_character_attribute(
            db_conn,
            character.id,
            CharAttribute::Wis,
            i64::from(character.wisdom),
        )?;
    }
    if luc_delta > 0 {
        character.luck = character.luck.saturating_add(luc_delta);
        db::set_character_attribute(
            db_conn,
            character.id,
            CharAttribute::Luc,
            i64::from(character.luck),
        )?;
    }

    character.stat_point = character.stat_point.saturating_sub(total.min(u32::from(u16::MAX)) as u16);
    db::set_character_attribute(
        db_conn,
        character.id,
        CharAttribute::StatPoint,
        i64::from(character.stat_point),
    )?;

    let stats = CharacterStats::from_character(Some(character), equipment);
    let mut max_energy_packets = Vec::new();
    if rec_delta > 0 {
        max_energy_packets.push((0u8, stats.max_hp));
    }
    if dex_delta > 0 {
        max_energy_packets.push((1u8, stats.max_sp));
    }
    if wis_delta > 0 {
        max_energy_packets.push((2u8, stats.max_mp));
    }

    logger.line(format!(
        "Rapture World applied stat points character_id={} str+={str_delta} dex+={dex_delta} rec+={rec_delta} int+={int_delta} wis+={wis_delta} luc+={luc_delta} remaining_stat_point={}",
        character.id, character.stat_point
    ));

    let response =
        build_stat_point_result([str_delta, dex_delta, rec_delta, int_delta, wis_delta, luc_delta])?;
    Ok(Some(StatPointOutcome {
        response,
        max_energy_packets,
    }))
}
