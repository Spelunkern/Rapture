//! Player experience (EXP) and leveling.
//!
//! Level requirements come from PS_GameDefs.dbo.ExpDefs (columns Level
//! smallint, Grow tinyint, Exp int), loaded once at startup the same way
//! drops.rs loads its drop tables. ExpDefs.Grow is 1-4 -- NOT the same
//! range as DbCharacter::mode/CharAttribute::Grow (0-3, confirmed by its
//! set_character_attribute bounds in rapture_common::db) -- so every
//! lookup here converts with `mode + 1`.
//!
//! `Chars.Exp` (DbCharacter::exp) is progress within the character's
//! CURRENT level, not a cumulative lifetime total: ExpDefs.Exp at
//! (character's level, grow) is the amount needed to advance to the next
//! level, and leveling up subtracts it (carrying over any excess) rather
//! than resetting to 0 -- matching the standard Shaiya emulator model
//! (e.g. Imgeneus) of "current-level progress bar", not "total XP ever
//! earned".
//!
//! Base max HP/MP/SP per level come from PS_GameDefs.dbo.StatusDefs
//! (Level, Job, HP, MP, SP -- `Job` matches DbCharacter::character_class
//! directly, same column name/range in both StatusDefs and Chars).
//! Confirmed against a real capture (2026-07-09) that leveling up changes
//! max hp/mp/sp by an amount far larger than the flat per-level stat
//! growth alone could produce via CharacterStats's stat-derived component
//! (e.g. +229 hp for a single level, vs. the +5 that formula gives per +1
//! Reaction) -- StatusDefs is the real, authoritative source for that
//! base component, previously hardcoded to a flat 100/100/100 placeholder
//! everywhere a character was loaded (see db::row_to_character).
//! StatusDefs values are absolute per level, not deltas: a level-up
//! REPLACES the character's base hp/mp/sp with the new level's row.

use anyhow::Result;
use rapture_common::db::{self, DbCharacter};
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::session::Logger;

/// Level cap requested for the current content/balance pass. ExpDefs
/// itself has rows up to level 80 (checked directly against the live DB),
/// but nothing above 60 is exposed yet -- raise this once higher-level
/// content is ready; no other code here needs to change.
pub(crate) const LEVEL_CAP: u16 = 60;

static EXP_TABLE: OnceLock<HashMap<(u16, u8), u32>> = OnceLock::new();
static STATUS_TABLE: OnceLock<HashMap<(u16, u8), (u32, u32, u32)>> = OnceLock::new();

/// Loads PS_GameDefs.dbo.ExpDefs once at server startup, mirroring
/// drops::load_drop_tables. Must be called before any `apply_exp_gain`.
pub(crate) fn load_exp_table(logger: &Logger) -> Result<()> {
    let mut db_conn = db::connect()?;
    let table = db::get_exp_definitions(&mut db_conn)?;
    logger.line(format!(
        "Rapture World loaded {} exp requirement(s) from ExpDefs",
        table.len()
    ));
    // set() only fails if already initialized -- harmless (and shouldn't
    // happen, since this is only ever called once from main()).
    let _ = EXP_TABLE.set(table);
    Ok(())
}

/// Loads PS_GameDefs.dbo.StatusDefs once at server startup. Must be called
/// before any `apply_exp_gain`.
pub(crate) fn load_status_table(logger: &Logger) -> Result<()> {
    let mut db_conn = db::connect()?;
    let table = db::get_status_definitions(&mut db_conn)?;
    logger.line(format!(
        "Rapture World loaded {} status requirement(s) from StatusDefs",
        table.len()
    ));
    let _ = STATUS_TABLE.set(table);
    Ok(())
}

/// Exp needed to advance from `level` to `level + 1` at growth stage
/// `mode` (DbCharacter::mode, 0-3). `None` if the table isn't loaded yet or
/// has no row for this (level, mode) pair.
fn exp_required(level: u16, mode: u8) -> Option<u32> {
    EXP_TABLE.get()?.get(&(level, mode.saturating_add(1))).copied()
}

/// Base (hp, mp, sp) at `level` for job/class `character_class`. `None` if
/// the table isn't loaded yet or has no row for this (level, class) pair.
fn base_status(level: u16, character_class: u8) -> Option<(u32, u32, u32)> {
    STATUS_TABLE.get()?.get(&(level, character_class)).copied()
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ExpGainResult {
    pub(crate) leveled_up: bool,
    pub(crate) levels_gained: u16,
    pub(crate) new_level: u16,
    pub(crate) new_exp: u32,
}

/// Adds `gained` exp to `character` (mutating its `level`/`exp` fields in
/// place) and advances levels while there's enough accumulated exp and
/// ExpDefs has a requirement for the character's current (level, mode) --
/// a single large reward can carry a character through several levels at
/// once. Permanently stops accruing once `LEVEL_CAP` is reached (there is
/// no further requirement to progress toward, so extra exp would just be
/// wasted precision, not real progress).
///
/// Base stats (str/rec/dex/luc/int/wis) deliberately do NOT grow with
/// level -- by project design, they stay fixed at their starting
/// value; only base hp/mp/sp scale with level. Each level gained resets
/// max_hp/max_mp/max_sp to the new level's StatusDefs row (see module
/// doc), fully healing current_hp/mp/sp to match (not confirmed against a
/// real capture -- the alternative, real capture showed the current hp/
/// mp/sp packet holding values far below the new max at the moment of a
/// level-up, but that packet's own meaning is still unconfirmed; full-
/// heal-on-level-up is the more common convention and the safer
/// assumption pending clearer evidence).
pub(crate) fn apply_exp_gain(character: &mut DbCharacter, gained: u32) -> ExpGainResult {
    if character.level >= LEVEL_CAP || gained == 0 {
        return ExpGainResult {
            leveled_up: false,
            levels_gained: 0,
            new_level: character.level,
            new_exp: character.exp,
        };
    }

    character.exp = character.exp.saturating_add(gained);
    let mut levels_gained = 0u16;
    while character.level < LEVEL_CAP {
        let Some(required) = exp_required(character.level, character.mode).filter(|&r| r > 0)
        else {
            break;
        };
        if character.exp < required {
            break;
        }
        character.exp -= required;
        character.level += 1;
        levels_gained += 1;
    }

    if levels_gained > 0 {
        if let Some((hp, mp, sp)) = base_status(character.level, character.character_class) {
            character.max_hp = hp;
            character.max_mp = mp;
            character.max_sp = sp;
            character.current_hp = hp;
            character.current_mp = mp;
            character.current_sp = sp;
        }
    }

    ExpGainResult {
        leveled_up: levels_gained > 0,
        levels_gained,
        new_level: character.level,
        new_exp: character.exp,
    }
}
