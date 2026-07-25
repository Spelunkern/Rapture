use anyhow::{bail, Context, Result};
use rusqlite::{Connection, ToSql};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Recovers from a poisoned lock instead of panicking, so one thread's
/// panic while holding this lock can't cascade into every other thread
/// that needs the connection.
fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

const SERVER_ID: u8 = 1;
const FACTION_NONE: u8 = 2;
const MAX_GROW_ULTIMATE: u8 = 3;
const DEFAULT_STAT: u16 = 10;
const DEFAULT_HP: u32 = 100;
const DEFAULT_MP: u32 = 100;
const DEFAULT_SP: u32 = 100;

#[derive(Clone, Debug)]
pub struct DbAccount {
    pub id: u32,
    pub username: String,
    pub faction: u8,
    pub max_grow: u8,
    // Real retail Users_Master already has Admin (bit) and AdminLevel
    // (tinyint) columns -- no schema migration needed. Treated as admin if
    // either is set, since it's unclear which one a given retail tool
    // actually writes to.
    pub is_admin: bool,
    // Raw AdminLevel (0 if not admin). Aries's AuthStatus enum
    // (shaiya/include/common/UserTypes.h) has 5 admin tiers, AdminA=1..
    // AdminE=5; used to pick which tier to report in the login success
    // packet.
    pub admin_level: u8,
    /// `Users_Master.Point` -- Item Mall / cash shop point balance.
    pub point: u32,
}

#[derive(Clone, Debug)]
pub struct DbCharacter {
    pub id: u32,
    pub account_id: u32,
    pub slot: u8,
    pub name: String,
    pub level: u16,
    pub map_id: u16,
    pub pos_x: f32,
    pub pos_y: f32,
    pub pos_z: f32,
    pub race: u8,
    pub mode: u8,
    pub hair: u8,
    pub face: u8,
    pub height: u8,
    pub character_class: u8,
    pub gender: u8,
    pub strength: u16,
    pub reaction: u16,
    pub intelligence: u16,
    pub wisdom: u16,
    pub dexterity: u16,
    pub luck: u16,
    pub stat_point: u16,
    pub skill_point: u16,
    pub max_hp: u32,
    pub max_mp: u32,
    pub max_sp: u32,
    pub current_hp: u32,
    pub current_mp: u32,
    pub current_sp: u32,
    pub exp: u32,
    pub money: u32,
    pub kills: u32,
    pub deaths: u32,
    pub victories: u32,
    pub defeats: u32,
    /// True when this character is in the "pending deletion" state (Del = 1
    /// but still within the 2-hour restore window). Only ever set true by
    /// `get_characters_for_account` (the selection-screen list); every other
    /// character query filters `Del = 0`, so a loaded/playable character is
    /// always `false` here.
    pub pending_delete: bool,
    /// True when the character is flagged for a forced rename on next login
    /// (`Chars.RenameCnt >= 1`, set by a name-change stone). Drives the
    /// `pending_rename` byte in the selection-screen list, which makes the
    /// client prompt for a new name before letting the character log in.
    pub rename_pending: bool,
}

#[derive(Clone, Debug)]
pub struct DbCharacterItem {
    pub row_id: u32,
    pub bag: u8,
    pub slot: u8,
    pub item_type: u8,
    pub type_id: u8,
    pub quality: u16,
    pub gems: [u8; 6],
    pub count: u8,
    pub craft_name: String,
    pub dyed: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DbEquipmentStats {
    pub strength: u32,
    pub reaction: u32,
    pub intelligence: u32,
    pub wisdom: u32,
    pub dexterity: u32,
    pub luck: u32,
    pub max_hp: u32,
    pub max_mp: u32,
    pub max_sp: u32,
    pub min_attack: u32,
    pub max_attack: u32,
    pub min_magic: u32,
    pub max_magic: u32,
    pub defense: u32,
    pub resistance: u32,
    /// Flat, universal damage reduction from enchanted armor (see
    /// `ARMOR_ENCHANT_ABSORPTION`). Distinct from `defense` -- not
    /// Imgeneus-style separate absorption packet/shield, just a plain
    /// subtraction applied to the final damage formula (see
    /// combat::apply_incoming_mob_damage), per the user's explicit design.
    pub absorption: u32,
}

/// Whether `item_type` is a weapon, per the retail Type -> category table
/// (RealType is NOT a DB column -- see `DbComposeInfo`'s doc), confirmed by
/// the user against the live retail server: Type 1-15 and 45-65 are
/// weapons. Shared by recreation (compose), enchant, and equipment stat
/// aggregation (weapon enchant attack bonus, see `get_equipment_stats`).
pub fn is_weapon_item_type(item_type: u8) -> bool {
    (1..=15).contains(&item_type) || (45..=65).contains(&item_type)
}

/// Whether `item_type` is armor, per the same user-confirmed table: Type
/// 16-21, 31-36, and 66-92 are armor. Shared by recreation (compose),
/// enchant, and equipment stat aggregation (armor enchant absorption, see
/// `get_equipment_stats`).
pub fn is_armor_item_type(item_type: u8) -> bool {
    (16..=21).contains(&item_type) || (31..=36).contains(&item_type) || (66..=92).contains(&item_type)
}

/// Flat attack bonus (added to BOTH physical and magic min/max attack) per
/// enchant step, index = enchant level 0-20. Confirmed by the user against
/// the retail server's step tables.
pub const WEAPON_ENCHANT_ATTACK_BONUS: [u32; 21] = [
    0, 7, 14, 21, 31, 41, 51, 64, 77, 90, 106, 122, 138, 157, 176, 195, 217, 239, 261, 286, 311,
];

/// Flat universal damage-reduction ("absorption") bonus per enchant step,
/// index = enchant level 0-20. Confirmed by the user against the retail
/// server's step tables.
pub const ARMOR_ENCHANT_ABSORPTION: [u32; 21] = [
    0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55, 60, 65, 70, 75, 80, 85, 90, 95, 100,
];

#[derive(Clone, Copy, Debug)]
pub struct DbConsumableEffect {
    pub special: u8,
    pub hp: u32,
    pub mp: u32,
    pub sp: u32,
    /// Cooldown group id (PS_GameDefs.dbo.Items.ReqIg). 0 means no cooldown;
    /// see rapture_world::inventory::cooldown_ms_for_req_ig for the
    /// ReqIg -> milliseconds table (client-matched).
    pub req_ig: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct DbWeaponCombat {
    pub item_type: u8,
    pub type_id: u8,
    pub attack_time: u8,
    pub range: f32,
    pub original_delay_ms: u64,
    pub test_delay_ms: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct DbItemDefinition {
    pub item_type: u8,
    pub type_id: u8,
    pub quality: u16,
    pub max_stack: u8,
    /// `PS_GameDefs.dbo.Items.Sell` -- gold paid per unit when sold to an
    /// NPC vendor.
    pub sell_price: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct DbMobDrop {
    pub mob_id: u16,
    pub item_order: u8,
    pub grade: u16,
    pub drop_rate: u32,
}

#[derive(Clone, Debug)]
pub struct DbQuickSlot {
    pub quick_bar: u8,
    pub quick_slot: u8,
    pub bag: u8,
    pub number: u16,
}

#[derive(Clone, Debug)]
pub struct DbCharacterSkill {
    pub skill_id: u16,
    pub skill_level: u8,
    pub number: u8,
    /// Seconds remaining on this skill's cooldown, as of when it was
    /// loaded (`get_character_skills` computes this live from
    /// `Delay`/`CreateTime`, so it's always fresh at load time -- see
    /// `set_character_skill_cooldown` for how those two columns are
    /// actually used).
    pub delay: i32,
}

/// One (SkillID, SkillLevel) row from `PS_GameDefs.dbo.Skills`.
/// `class_eligible` is indexed by `DbCharacter::character_class` (0-5):
/// 0=Attackfighter, 1=Defensefighter, 2=Patrolrogue, 3=Shootrogue,
/// 4=Attackmage, 5=Defensemage -- confirmed mapping.
#[derive(Clone, Copy, Debug)]
pub struct DbSkillDefinition {
    pub skill_id: u16,
    pub skill_level: u8,
    pub req_level: u16,
    pub skill_point_cost: u8,
    pub prev_skill_id: u16,
    pub class_eligible: [bool; 6],
    /// Combat fields (used when actually casting the skill, not just
    /// learning it): MP/SP cost, cooldown (`ResetTime`, seconds), flat
    /// damage bonus (`DamageHP` + `AddDamageHP`, added to the caster's
    /// stat-based attack damage), and cast range.
    pub mp_cost: u16,
    pub sp_cost: u16,
    pub reset_time_seconds: u16,
    pub damage_hp: i32,
    pub add_damage_hp: i32,
    /// `Skills.HealHP/HealSP/HealMP` -- an instant restore instead of
    /// damage. Present independently of `damage_hp` (never both nonzero on
    /// the same row in the live DB) and independently of each other -- a
    /// skill can restore any subset of HP/MP/SP, including all three.
    pub heal_hp: i32,
    pub heal_sp: i32,
    pub heal_mp: i32,
    /// `Skills.TypeDetail` -- confirmed by the user against native-client
    /// testing: `41` = instant heal (`HealHP`/`HealSP`/`HealMP`), `81` =
    /// periodic heal over time (`TimeHealHP`/`TimeHealSP`/`TimeHealMP`,
    /// ticking every 3 seconds for `KeepTime` seconds total). Both are
    /// confirmed to correlate 1:1 with their respective heal columns being
    /// nonzero across every row in the live DB.
    pub type_detail: u16,
    /// `Skills.KeepTime` -- total duration in seconds a periodic
    /// (`type_detail == 81`) effect lasts.
    pub keep_time: u16,
    /// `Skills.TimeHealHP/TimeHealSP/TimeHealMP` -- the flat amount
    /// restored per 3-second tick of a periodic heal. Confirmed by the
    /// user to never scale with WIS (unlike instant `HealHP`, which does
    /// for `TypeAttack == 3` skills).
    pub time_heal_hp: i32,
    pub time_heal_sp: i32,
    pub time_heal_mp: i32,
    /// `Skills.ReadyTime` -- cast time, in units of 250ms (confirmed via
    /// Imgeneus's `Skill.CastTime => ReadyTime * 250` and matched against a
    /// real capture: `ReadyTime == 4` on a heal that took ~1 real second to
    /// resolve). `0` means the skill resolves instantly with no cast bar.
    pub ready_time: u8,
    /// `Skills.AbilityType1..10`/`AbilityValue1..10` -- used by `TypeDetail
    /// == 82` ("Buff") skills. Confirmed via Imgeneus's `AbilityType` enum:
    /// `1`=HP, `2`=MP, `3`=SP, `4`=Str, `5`=Rec, `6`=Int, `7`=Wis, `8`=Dex,
    /// `9`=Luc (only these are currently applied -- see skillcast.rs's
    /// buff handling; every other `AbilityType` is logged and ignored for
    /// now, matching this project's incremental scope).
    pub ability_type: [u8; 10],
    pub ability_value: [i16; 10],
    /// `Skills.TimeDamageHP/TimeDamageSP/TimeDamageMP` -- periodic damage
    /// (`TypeDetail == 61`, "PeriodicalDebuff" in Imgeneus's enum), ticking
    /// every 3 seconds (confirmed by the user) for `KeepTime` seconds,
    /// applied to a MOB target (unlike heal/buff, which are self-only).
    /// Independent of `damage_hp` -- a skill can deal instant damage AND
    /// a DoT together (confirmed by the user; e.g. "Meteor" in the live
    /// DB has both), or a DoT alone with `damage_hp == 0` (e.g. "Poison").
    /// `Skills.TimeDamageType` (not stored here) supposedly distinguishes
    /// flat vs. percentage-of-max-HP vs. a doubling-each-tick scaling per
    /// Imgeneus, but the real DB's observed values (0, 1, 4, 5, 12) don't
    /// obviously map onto that 3-way split -- current scope (confirmed by
    /// the user) always treats these as flat literal amounts regardless
    /// of TimeDamageType, pending real evidence of what the other values
    /// mean.
    pub time_damage_hp: i32,
    pub time_damage_sp: i32,
    pub time_damage_mp: i32,
    pub attack_range: u8,
    /// `Skills.TypeAttack` -- confirmed by cross-referencing its values
    /// against class eligibility on every damaging skill in the live DB:
    /// 0 = no damage component (passive/buff), 1 = melee physical
    /// (fighter/defender/ranger), 2 = ranged physical (archer), 3 = magic
    /// (mage/priest). Determines whether a skill's damage scales off
    /// STR/DEX (`CharacterStats::min_attack/max_attack`) or INT/WIS
    /// (`min_magic/max_magic`).
    pub type_attack: u8,
    /// `Skills.TargetType`/`ApplyRange`/`MultiAttack` -- present in the
    /// schema (AoE/multi-hit) but not yet interpreted; exposed here for
    /// callers that want to inspect them, not consumed by combat yet.
    pub target_type: u8,
    pub apply_range: u8,
    pub multi_attack: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct DbMobDefinition {
    pub mob_id: u16,
    pub level: u16,
    pub max_hp: u32,
    pub money_min: u32,
    pub money_max: u32,
    pub respawn_seconds: Option<u64>,
    pub exp: u32,
    /// `Mobs.Attack1` -- the mob's basic melee attack damage (before the
    /// target's defense is subtracted). 0 if the column is missing/zero.
    pub attack: u32,
    /// `Mobs.AI` -- 0 means passive (only fights back when attacked), >= 1
    /// means aggressive (attacks players who come within `chase_range`).
    pub ai: u8,
    /// `Mobs.ChaseRange` (meters) -- aggro detection radius for aggressive
    /// mobs. 0.0 if the column is missing/zero.
    pub chase_range: f32,
}

#[derive(Clone, Debug)]
pub struct NewCharacter {
    pub slot: u8,
    pub race: u8,
    pub mode: u8,
    pub hair: u8,
    pub face: u8,
    pub height: u8,
    pub character_class: u8,
    pub gender: u8,
    pub name: String,
    pub map_id: u16,
    pub pos_x: f32,
    pub pos_y: f32,
    pub pos_z: f32,
}

/// A lightweight handle to the process-wide SQLite connections.
///
/// Actual `rusqlite::Connection`s are owned by the two global, lazily
/// initialized connections (`gamedata_conn`/`world_conn`), not by this
/// struct, so `Db` is cheap to create per client session and never opens a
/// fresh connection per query.
pub struct Db;

pub fn connect() -> Result<Db> {
    // Touch both global connections eagerly so path/permission errors
    // surface immediately instead of on first query.
    let _ = gamedata_conn()?;
    let _ = world_conn()?;
    Ok(Db)
}

/// Tables `gamedata.db` is expected to already have. Checked against
/// `sqlite_master` on open -- see `open_existing_db`'s doc for why this
/// project deliberately never creates or migrates schema at runtime.
const GAMEDATA_TABLES: &[&str] = &["Items", "MobItems", "Mobs", "ExpDefs", "Skills", "ProductList"];

/// Tables `world.db` is expected to already have. See `GAMEDATA_TABLES`.
const WORLD_TABLES: &[&str] = &[
    "Users_Master",
    "UserMaxGrow",
    "Chars",
    "CharItems",
    "UserStoredItems",
    "CharQuickSlots",
    "CharSkills",
    "CharApplySkills",
    "Users_Product3",
];

/// Opens an already-existing SQLite file and checks it has every table in
/// `expected_tables`. Rapture ships `gamedata.db`/`world.db` as canonical,
/// pre-built data files (see `crates/rapture_common/schema/*.sql` for the
/// exact DDL they were built from) -- the running server never creates,
/// bootstraps, or migrates a database on its own. A missing file or a
/// missing table is a hard, clearly-reported startup error instead of
/// something this code silently papers over.
fn open_existing_db(path: &std::path::Path, expected_tables: &[&str], label: &str) -> Result<Connection> {
    if !path.exists() {
        bail!(
            "{label} not found at {} -- Rapture does not create this file at runtime; \
             copy the canonical {label} into place before starting the server \
             (see docs/INSTALLATION.md's Database section)",
            path.display()
        );
    }
    let conn = Connection::open(path).with_context(|| format!("failed to open {label} at {}", path.display()))?;

    let mut existing: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .with_context(|| format!("failed to inspect schema of {label} at {}", path.display()))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            existing.insert(row.get::<_, String>(0)?);
        }
    }

    let missing: Vec<&str> = expected_tables
        .iter()
        .filter(|table| !existing.contains(**table))
        .copied()
        .collect();
    if !missing.is_empty() {
        bail!(
            "{label} at {} is missing expected table(s): {} -- this file doesn't match the \
             schema this server expects (see crates/rapture_common/schema/*.sql); it may be an \
             old/partial copy",
            path.display(),
            missing.join(", ")
        );
    }

    Ok(conn)
}

/// Global connection to `gamedata.db` (static, read-only game content).
/// Initialized on first use; a failed open is retried on the next call
/// rather than cached, since `OnceLock` has no fallible `get_or_init` on
/// stable Rust.
fn gamedata_conn() -> Result<&'static Mutex<Connection>> {
    static CONN: OnceLock<Mutex<Connection>> = OnceLock::new();
    if let Some(conn) = CONN.get() {
        return Ok(conn);
    }
    let path = crate::config::gamedata_db_path();
    let conn = open_existing_db(&path, GAMEDATA_TABLES, "gamedata.db")?;
    let _ = CONN.set(Mutex::new(conn));
    Ok(CONN.get().expect("gamedata connection was just set"))
}

/// Global connection to `world.db` (live account/character/inventory
/// state). See `gamedata_conn` for the initialization strategy.
fn world_conn() -> Result<&'static Mutex<Connection>> {
    static CONN: OnceLock<Mutex<Connection>> = OnceLock::new();
    if let Some(conn) = CONN.get() {
        return Ok(conn);
    }
    let path = crate::config::world_db_path();
    let conn = open_existing_db(&path, WORLD_TABLES, "world.db")?;
    let _ = CONN.set(Mutex::new(conn));
    Ok(CONN.get().expect("world connection was just set"))
}

/// Routes the old SQL-Server "database" name to the right SQLite file:
/// `"PS_GameDefs"` (Items/MobItems/Mobs/ExpDefs/Skills/ProductList) lives in
/// `gamedata.db`; everything else (`PS_GameData`/`PS_UserData`/`PS_Billing`)
/// now lives together in `world.db`.
fn conn_for(database: &str) -> Result<&'static Mutex<Connection>> {
    if database == "PS_GameDefs" {
        gamedata_conn()
    } else {
        world_conn()
    }
}

pub fn init_schema(_conn: &mut Db) -> Result<()> {
    // Eagerly touches both global connections so a missing/malformed
    // gamedata.db or world.db (see `open_existing_db`) is a clear startup
    // error instead of surfacing on the first query a client happens to
    // trigger.
    let _ = gamedata_conn()?;
    let _ = world_conn()?;
    Ok(())
}

/// Runs `sql` against the routed connection and maps the first returned
/// row with `map`, or `None` if the query returned no rows.
fn query_one<T>(
    conn: &mut Db,
    database: &str,
    sql: &str,
    params: &[&dyn ToSql],
    map: impl FnOnce(&rusqlite::Row<'_>) -> T,
) -> Result<Option<T>> {
    let _ = conn;
    let guard = lock_or_recover(conn_for(database)?);
    let mut stmt = guard.prepare(sql).context("failed to prepare SQL query")?;
    let mut rows = stmt.query(params).context("failed to execute SQL query")?;
    match rows.next().context("failed to read SQL row")? {
        Some(row) => Ok(Some(map(row))),
        None => Ok(None),
    }
}

/// Like `query_one`, but collects every returned row.
fn query_many<T>(
    conn: &mut Db,
    database: &str,
    sql: &str,
    params: &[&dyn ToSql],
    mut map: impl FnMut(&rusqlite::Row<'_>) -> T,
) -> Result<Vec<T>> {
    let _ = conn;
    let guard = lock_or_recover(conn_for(database)?);
    let mut stmt = guard.prepare(sql).context("failed to prepare SQL query")?;
    let mut rows = stmt.query(params).context("failed to execute SQL query")?;
    let mut results = Vec::new();
    while let Some(row) = rows.next().context("failed to read SQL row")? {
        results.push(map(row));
    }
    Ok(results)
}

fn execute(conn: &mut Db, database: &str, sql: &str, params: &[&dyn ToSql]) -> Result<()> {
    let _ = conn;
    let guard = lock_or_recover(conn_for(database)?);
    guard
        .execute(sql, params)
        .context("failed to execute SQL command")?;
    Ok(())
}

/// Like `execute`, but returns the total number of rows the statement
/// affected -- used where "did this actually change a row?" is the result
/// (e.g. soft-delete/restore, which report ok/fail based on whether the
/// scoped WHERE matched a row).
fn execute_rows(conn: &mut Db, database: &str, sql: &str, params: &[&dyn ToSql]) -> Result<u64> {
    let _ = conn;
    let guard = lock_or_recover(conn_for(database)?);
    let rows = guard
        .execute(sql, params)
        .context("failed to execute SQL command")?;
    Ok(rows as u64)
}

/// Like `execute`, but returns `rowid` of the row just inserted (SQLite's
/// `last_insert_rowid()`, the equivalent of SQL Server's `OUTPUT
/// INSERTED.<identity column>`).
fn insert_and_last_row_id(
    conn: &mut Db,
    database: &str,
    sql: &str,
    params: &[&dyn ToSql],
) -> Result<i64> {
    let _ = conn;
    let guard = lock_or_recover(conn_for(database)?);
    guard
        .execute(sql, params)
        .context("failed to execute SQL insert")?;
    Ok(guard.last_insert_rowid())
}

/// A fresh, effectively-unique 63-bit item UID (the SQLite replacement for
/// the old `CHECKSUM(NEWID())`-based random bigint). Masked to stay
/// non-negative for a slightly tidier `ItemUID` column.
fn random_item_uid() -> i64 {
    (rand::random::<u64>() & 0x7FFF_FFFF_FFFF_FFFF) as i64
}

pub fn authenticate_account(
    conn: &mut Db,
    username: &str,
    password: &str,
) -> Result<Option<DbAccount>> {
    let sql = r#"
        SELECT
            u.UserUID,
            u.UserID,
            u.Admin,
            u.AdminLevel,
            u.Point,
            COALESCE(g.Country, 2) AS Country,
            COALESCE(g.MaxGrow, 3) AS MaxGrow
        FROM Users_Master AS u
        LEFT JOIN UserMaxGrow AS g
            ON g.ServerID = ?1
            AND g.UserUID = u.UserUID
            AND g.Del = 0
        WHERE u.UserID = ?2
            AND u.Pw = ?3
            AND u.Leave = 0
        LIMIT 1
    "#;

    let server_id = i32::from(SERVER_ID);
    let row = query_one(
        conn,
        "PS_UserData",
        sql,
        &[&server_id, &username, &password],
        account_from_row,
    )?;
    Ok(row)
}

pub fn get_account_by_id(conn: &mut Db, account_id: u32) -> Result<Option<DbAccount>> {
    let sql = r#"
        SELECT
            u.UserUID,
            u.UserID,
            u.Admin,
            u.AdminLevel,
            u.Point,
            COALESCE(g.Country, 2) AS Country,
            COALESCE(g.MaxGrow, 3) AS MaxGrow
        FROM Users_Master AS u
        LEFT JOIN UserMaxGrow AS g
            ON g.ServerID = ?1
            AND g.UserUID = u.UserUID
            AND g.Del = 0
        WHERE u.UserUID = ?2
            AND u.Leave = 0
        LIMIT 1
    "#;

    let server_id = i32::from(SERVER_ID);
    let account_id = account_id as i32;
    let row = query_one(
        conn,
        "PS_UserData",
        sql,
        &[&server_id, &account_id],
        account_from_row,
    )?;
    Ok(row)
}

pub fn update_account_faction(conn: &mut Db, account_id: u32, faction: u8) -> Result<()> {
    let sql = r#"
        INSERT INTO UserMaxGrow (ServerID, UserUID, Country, MaxGrow, Del)
        VALUES (?1, ?2, ?3, ?4, 0)
        ON CONFLICT(ServerID, UserUID) DO UPDATE SET
            Country = excluded.Country,
            MaxGrow = CASE
                WHEN UserMaxGrow.MaxGrow < excluded.MaxGrow THEN excluded.MaxGrow
                ELSE UserMaxGrow.MaxGrow
            END,
            Del = 0
    "#;

    let server_id = i32::from(SERVER_ID);
    let account_id = account_id as i32;
    let faction = i32::from(faction);
    let max_grow = i32::from(MAX_GROW_ULTIMATE);
    execute(
        conn,
        "PS_GameData",
        sql,
        &[&server_id, &account_id, &faction, &max_grow],
    )
    .context("failed to update account faction")
}

/// Returns every non-deleted character on the account, ordered by slot.
/// Used to render the full character-select list; `get_first_character`
/// only returns one row and must not be used where multiple characters
/// per account need to be visible (e.g. the character list packet).
/// How long a soft-deleted character stays restorable before it's treated
/// as permanently gone (matches the retail server's 2-hour window).
pub const CHARACTER_DELETE_WINDOW_HOURS: i64 = 2;

pub fn get_characters_for_account(conn: &mut Db, account_id: u32) -> Result<Vec<DbCharacter>> {
    // Include characters pending deletion (Del = 1) that are still inside the
    // 2h restore window, flagged via `pending_delete` so the selection screen
    // can render them greyed-out/restorable. Rows past the window are treated
    // as purged and simply excluded. Non-deleted characters (Del = 0) always
    // show. Every OTHER character query keeps filtering Del = 0, so a pending
    // character can be listed but never selected/played until restored.
    let sql = format!(
        r#"
        SELECT
            {CHARACTER_COLUMNS}, Del
        FROM Chars
        WHERE UserUID = ?1
          AND (
            Del = 0
            OR (Del = 1 AND DeleteDate IS NOT NULL AND DeleteDate >= datetime('now', '-{CHARACTER_DELETE_WINDOW_HOURS} hours'))
          )
        ORDER BY Slot
    "#
    );
    let account_id_param = account_id as i32;
    let rows = query_many(conn, "PS_GameData", &sql, &[&account_id_param], |row| {
        let mut character = row_to_character(row);
        character.pending_delete = flexible_u32(row, "Del", 0) == 1;
        character
    })?;
    Ok(rows)
}

/// Soft-deletes a character into the pending-deletion window: sets Del = 1
/// and stamps DeleteDate = now. Scoped to the owning account so a client
/// can't delete another account's character, and only touches a currently-
/// live (Del = 0) row. Returns whether a row was actually deleted.
pub fn set_character_pending_delete(
    conn: &mut Db,
    account_id: u32,
    character_id: u32,
) -> Result<bool> {
    let sql = r#"
        UPDATE Chars
        SET Del = 1, DeleteDate = datetime('now')
        WHERE CharID = ?1 AND UserUID = ?2 AND Del = 0
    "#;
    let character_id_param = character_id as i32;
    let account_id_param = account_id as i32;
    let rows = execute_rows(conn, "PS_GameData", sql, &[&character_id_param, &account_id_param])
        .context("failed to soft-delete character")?;
    Ok(rows > 0)
}

/// Restores a character from the pending-deletion window (Del = 0, clears
/// DeleteDate) -- but only while still within CHARACTER_DELETE_WINDOW_HOURS
/// of the DeleteDate stamp. Scoped to the owning account. Returns whether a
/// row was restored (false if not found, not this account's, not pending, or
/// already past the window).
pub fn restore_character(conn: &mut Db, account_id: u32, character_id: u32) -> Result<bool> {
    let sql = format!(
        r#"
        UPDATE Chars
        SET Del = 0, DeleteDate = NULL
        WHERE CharID = ?1 AND UserUID = ?2 AND Del = 1
          AND DeleteDate IS NOT NULL
          AND DeleteDate >= datetime('now', '-{CHARACTER_DELETE_WINDOW_HOURS} hours')
    "#
    );
    let character_id_param = character_id as i32;
    let account_id_param = account_id as i32;
    let rows = execute_rows(conn, "PS_GameData", &sql, &[&character_id_param, &account_id_param])
        .context("failed to restore character")?;
    Ok(rows > 0)
}

/// Flags a character for a forced rename on next login (`RenameCnt = 1`),
/// set when a name-change stone is used. Scoped to CharID only -- the caller
/// only ever passes its own loaded/active character. Returns whether a row
/// was updated.
pub fn set_character_pending_rename(conn: &mut Db, character_id: u32) -> Result<bool> {
    let character_id_param = character_id as i32;
    let rows = execute_rows(
        conn,
        "PS_GameData",
        "UPDATE Chars SET RenameCnt = 1 WHERE CharID = ?1 AND Del = 0",
        &[&character_id_param],
    )
    .context("failed to flag character for rename")?;
    Ok(rows > 0)
}

/// Commits a pending rename: sets the new name and clears `RenameCnt`, but
/// only for a character that (a) belongs to `account_id`, and (b) is actually
/// pending a rename (`RenameCnt >= 1`) -- so a modified client can't rename a
/// character it never flagged. The caller must have already verified the new
/// name isn't taken (see `character_name_taken`). Returns whether a row was
/// renamed.
pub fn rename_character(
    conn: &mut Db,
    account_id: u32,
    character_id: u32,
    new_name: &str,
) -> Result<bool> {
    let character_id_param = character_id as i32;
    let account_id_param = account_id as i32;
    let rows = execute_rows(
        conn,
        "PS_GameData",
        "UPDATE Chars SET CharName = ?1, RenameCnt = 0 \
         WHERE CharID = ?2 AND UserUID = ?3 AND Del = 0 AND RenameCnt >= 1",
        &[&new_name, &character_id_param, &account_id_param],
    )
    .context("failed to rename character")?;
    Ok(rows > 0)
}

pub fn get_character_by_slot(
    conn: &mut Db,
    account_id: u32,
    slot: u8,
) -> Result<Option<DbCharacter>> {
    let sql = format!(
        "{} WHERE UserUID = ?1 AND Slot = ?2 AND Del = 0 LIMIT 1",
        character_select_sql()
    );
    let account_id = account_id as i32;
    let slot = i32::from(slot);
    let row = query_one(conn, "PS_GameData", &sql, &[&account_id, &slot], row_to_character)?;
    Ok(row)
}

pub fn get_character_by_id(
    conn: &mut Db,
    account_id: u32,
    character_id: u32,
) -> Result<Option<DbCharacter>> {
    let sql = format!(
        "{} WHERE UserUID = ?1 AND CharID = ?2 AND Del = 0 LIMIT 1",
        character_select_sql()
    );
    let account_id = account_id as i32;
    let character_id = character_id as i32;
    let row = query_one(conn, "PS_GameData", &sql, &[&account_id, &character_id], row_to_character)?;
    Ok(row)
}

/// Looks up a character by name regardless of which account owns it --
/// used by GM commands (set/amove/asummon/iclear/eclear) that target
/// another player by name.
pub fn get_character_by_name(conn: &mut Db, name: &str) -> Result<Option<DbCharacter>> {
    let sql = format!(
        "{} WHERE CharName = ?1 AND Del = 0 LIMIT 1",
        character_select_sql()
    );
    let row = query_one(conn, "PS_GameData", &sql, &[&name], row_to_character)?;
    Ok(row)
}

/// Whether `name` is already taken by a live character on any account, or by
/// one still inside its pending-deletion window (whose name stays reserved
/// until it's actually purged, so restoring it can't collide). Used to
/// reject a duplicate name at character creation -- this client has no
/// explicit name-check packet (unlike EP8/Imgeneus); a duplicate simply
/// fails the create with an in-game "name in use" notice.
pub fn character_name_taken(conn: &mut Db, name: &str) -> Result<bool> {
    let sql = format!(
        r#"
        SELECT 1 AS Taken
        FROM Chars
        WHERE CharName = ?1
          AND (
            Del = 0
            OR (Del = 1 AND DeleteDate IS NOT NULL AND DeleteDate >= datetime('now', '-{CHARACTER_DELETE_WINDOW_HOURS} hours'))
          )
        LIMIT 1
    "#
    );
    let row = query_one(conn, "PS_GameData", &sql, &[&name], |_row| ())?;
    Ok(row.is_some())
}

/// Looks up a character purely by CharID, regardless of account -- used
/// for granting EXP to an OFFLINE DoT-kill caster (no account_id is
/// available in that context, only the character_id the DoT was cast
/// with), unlike `get_character_by_id`, which requires a matching
/// account_id (a live session's own lookup pattern).
pub fn get_character_by_char_id(conn: &mut Db, character_id: u32) -> Result<Option<DbCharacter>> {
    let sql = format!(
        "{} WHERE CharID = ?1 AND Del = 0 LIMIT 1",
        character_select_sql()
    );
    let character_id_param = character_id as i32;
    let row = query_one(conn, "PS_GameData", &sql, &[&character_id_param], row_to_character)?;
    Ok(row)
}

/// Character stat columns settable via the GM `set` command. The column
/// name for each is a fixed match in set_character_attribute -- never
/// built from user input -- so there's no SQL injection surface even
/// though callers ultimately originate from a chat command.
#[derive(Clone, Copy, Debug)]
pub enum CharAttribute {
    Level,
    Grow,
    Money,
    Exp,
    StatPoint,
    SkillPoint,
    Str,
    Dex,
    Rec,
    Int,
    Luc,
    Wis,
    Kills,
    Deaths,
}

/// Hard gold cap requested for this project (below u32::MAX so it always
/// fits the bit-cast round trip through the signed 32-bit Money column).
pub const MONEY_CAP: u32 = 4_000_000_000;

impl CharAttribute {
    pub fn name(self) -> &'static str {
        match self {
            CharAttribute::Level => "level",
            CharAttribute::Grow => "grow",
            CharAttribute::Money => "money",
            CharAttribute::Exp => "exp",
            CharAttribute::StatPoint => "statpoint",
            CharAttribute::SkillPoint => "skillpoint",
            CharAttribute::Str => "str",
            CharAttribute::Dex => "dex",
            CharAttribute::Rec => "rec",
            CharAttribute::Int => "int",
            CharAttribute::Luc => "luc",
            CharAttribute::Wis => "wis",
            CharAttribute::Kills => "kills",
            CharAttribute::Deaths => "deaths",
        }
    }
}

/// Sets one of `Chars`'s stat columns live, clamped to that column's real
/// range (or, for level/grow, a tighter game-design cap the user asked
/// for even though the DB column allows more). Returns the value actually
/// applied. `Grow` maps to Aries's Grow enum (Easy..Ultimate, 0-3); `Kills`/
/// `Deaths` map to the K1/K2 columns (same ones 0x020E already reports).
pub fn set_character_attribute(
    conn: &mut Db,
    character_id: u32,
    attribute: CharAttribute,
    value: i64,
) -> Result<i32> {
    let (column, min, max): (&str, i64, i64) = match attribute {
        CharAttribute::Level => ("Level", 0, 255),
        CharAttribute::Grow => ("Grow", 0, 3),
        CharAttribute::Money => ("Money", 0, MONEY_CAP as i64),
        CharAttribute::Exp => ("Exp", 0, i32::MAX as i64),
        CharAttribute::StatPoint => ("StatPoint", 0, i16::MAX as i64),
        CharAttribute::SkillPoint => ("SkillPoint", 0, i16::MAX as i64),
        CharAttribute::Str => ("Str", 0, i16::MAX as i64),
        CharAttribute::Dex => ("Dex", 0, i16::MAX as i64),
        CharAttribute::Rec => ("Rec", 0, i16::MAX as i64),
        CharAttribute::Int => ("Int", 0, i16::MAX as i64),
        CharAttribute::Luc => ("Luc", 0, i16::MAX as i64),
        CharAttribute::Wis => ("Wis", 0, i16::MAX as i64),
        CharAttribute::Kills => ("K1", 0, i32::MAX as i64),
        CharAttribute::Deaths => ("K2", 0, i32::MAX as i64),
    };
    let clamped = value.clamp(min, max) as i32;
    let sql = format!("UPDATE Chars SET {column} = ?1 WHERE CharID = ?2");
    let character_id_param = character_id as i32;
    execute(conn, "PS_GameData", &sql, &[&clamped, &character_id_param])
        .context("failed to update character attribute")?;
    Ok(clamped)
}

/// Adds `amount` gold, clamped to MONEY_CAP. Money is stored in a signed
/// 32-bit SQL column but treated as unsigned up to MONEY_CAP, so this reads
/// the current value with a bit-reinterpretation cast (not a clamp-to-0),
/// does the add/clamp in Rust, then writes back the same way.
pub fn add_character_money(conn: &mut Db, character_id: u32, amount: u32) -> Result<u32> {
    let character_id_param = character_id as i32;
    let current = query_one(
        conn,
        "PS_GameData",
        "SELECT Money FROM Chars WHERE CharID = ?1",
        &[&character_id_param],
        |row| row.get::<_, i64>(0).unwrap_or_default() as i32,
    )?
    .context("character not found while adding money")? as u32;
    let new_money = current.saturating_add(amount).min(MONEY_CAP);
    let stored = new_money as i32;
    execute(
        conn,
        "PS_GameData",
        "UPDATE Chars SET Money = ?1 WHERE CharID = ?2",
        &[&stored, &character_id_param],
    )?;
    Ok(new_money)
}

pub fn set_character_money(conn: &mut Db, character_id: u32, money: u32) -> Result<u32> {
    let applied =
        set_character_attribute(conn, character_id, CharAttribute::Money, i64::from(money))?;
    Ok(applied as u32)
}

/// Counts how many of a character's items would be affected by
/// clear_character_items, for reporting purposes before the clear happens.
pub fn count_character_items(conn: &mut Db, character_id: u32, equipped_only: bool) -> Result<u32> {
    let sql = if equipped_only {
        "SELECT COUNT(*) AS Cnt FROM CharItems WHERE CharID = ?1 AND Bag = 0 AND Del = 0 AND Count > 0"
    } else {
        "SELECT COUNT(*) AS Cnt FROM CharItems WHERE CharID = ?1 AND Bag <> 0 AND Del = 0 AND Count > 0"
    };
    let character_id_param = character_id as i32;
    let count = query_one(conn, "PS_GameData", sql, &[&character_id_param], |row| {
        row.get::<_, i64>("Cnt").unwrap_or(0)
    })?
    .unwrap_or(0)
    .max(0) as u32;
    Ok(count)
}

/// Soft-deletes every item in bag 0 (equipped, `equipped_only = true`) or
/// every item outside bag 0 (inventory, `equipped_only = false`) for a
/// character. Used by the GM `eclear`/`iclear` commands.
pub fn clear_character_items(conn: &mut Db, character_id: u32, equipped_only: bool) -> Result<()> {
    let sql = if equipped_only {
        r#"
            UPDATE CharItems
            SET Count = 0, Del = 1
            WHERE CharID = ?1 AND Bag = 0 AND Del = 0
        "#
    } else {
        r#"
            UPDATE CharItems
            SET Count = 0, Del = 1
            WHERE CharID = ?1 AND Bag <> 0 AND Del = 0
        "#
    };
    let character_id_param = character_id as i32;
    execute(conn, "PS_GameData", sql, &[&character_id_param])
        .context("failed to clear character items")
}

fn row_to_character_item(row: &rusqlite::Row<'_>, bag_override: Option<u8>) -> DbCharacterItem {
    DbCharacterItem {
        row_id: row.get::<_, i64>("RowID").unwrap_or_default().max(0) as u32,
        bag: bag_override.unwrap_or_else(|| tinyint_or_default(row, "Bag", 0)),
        slot: tinyint_or_default(row, "Slot", 0),
        item_type: tinyint_or_default(row, "Type", 0),
        type_id: tinyint_or_default(row, "TypeID", 0),
        quality: positive_smallint_as_u16(row, "Quality", 0),
        gems: [
            tinyint_or_default(row, "Gem1", 0),
            tinyint_or_default(row, "Gem2", 0),
            tinyint_or_default(row, "Gem3", 0),
            tinyint_or_default(row, "Gem4", 0),
            tinyint_or_default(row, "Gem5", 0),
            tinyint_or_default(row, "Gem6", 0),
        ],
        count: tinyint_or_default(row, "Count", 1),
        craft_name: row
            .get::<_, String>("Craftname")
            .unwrap_or_default()
            .trim_end()
            .to_string(),
        dyed: false,
    }
}

pub fn get_character_items(conn: &mut Db, character_id: u32) -> Result<Vec<DbCharacterItem>> {
    let sql = r#"
        SELECT
            RowID, Bag, Slot, Type, TypeID, Quality,
            Gem1, Gem2, Gem3, Gem4, Gem5, Gem6,
            Count, Craftname
        FROM CharItems
        WHERE CharID = ?1
            AND Del = 0
            AND Count > 0
        ORDER BY Bag, Slot
    "#;

    let character_id = character_id as i32;
    query_many(conn, "PS_GameData", sql, &[&character_id], |row| {
        row_to_character_item(row, None)
    })
}

/// The orange (recreated) bonus stats decoded from an item's 20-digit
/// `Craftname` string: 10 pairs of 2 digits, in order
/// `str dex rec int wis luc hp/100 mp/100 sp/100 enchantLevel`.
/// (e.g. "05000200000000000000" = +5 STR, +2 REC). Same encoding Imgeneus
/// parses/produces; the enchant pair is carried but not yet applied
/// anywhere (future enchant support).
#[derive(Clone, Copy, Debug, Default)]
pub struct DbCraftStats {
    pub strength: u32,
    pub dexterity: u32,
    pub reaction: u32,
    pub intelligence: u32,
    pub wisdom: u32,
    pub luck: u32,
    pub max_hp: u32,
    pub max_mp: u32,
    pub max_sp: u32,
    pub enchant_level: u8,
}

/// Decodes a `Craftname` string (see `DbCraftStats`). `None` for anything
/// that isn't exactly 20 ASCII digits (empty/legacy values simply add no
/// stats).
pub fn parse_craft_name(craft_name: &str) -> Option<DbCraftStats> {
    let bytes = craft_name.trim_end().as_bytes();
    if bytes.len() != 20 || !bytes.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    let pair = |index: usize| -> u32 {
        u32::from(bytes[index] - b'0') * 10 + u32::from(bytes[index + 1] - b'0')
    };
    Some(DbCraftStats {
        strength: pair(0),
        dexterity: pair(2),
        reaction: pair(4),
        intelligence: pair(6),
        wisdom: pair(8),
        luck: pair(10),
        max_hp: pair(12) * 100,
        max_mp: pair(14) * 100,
        max_sp: pair(16) * 100,
        enchant_level: pair(18) as u8,
    })
}

pub fn get_equipment_stats(conn: &mut Db, items: &[DbCharacterItem]) -> Result<DbEquipmentStats> {
    let equipped_items = items
        .iter()
        .filter(|item| item.bag == 0)
        .map(|item| {
            (
                i32::from(item.item_type),
                i32::from(item.type_id),
                item.craft_name.clone(),
                item.gems,
            )
        })
        .collect::<Vec<_>>();

    if equipped_items.is_empty() {
        return Ok(DbEquipmentStats::default());
    }

    let sql = r#"
        SELECT
            Effect1, Effect2, Effect3, Effect4,
            ConstHP, ConstSP, ConstMP,
            ConstStr, ConstDex, ConstRec, ConstInt, ConstWis, ConstLuc
        FROM Items
        WHERE Type = ?1
            AND TypeID = ?2
        LIMIT 1
    "#;

    let mut stats = DbEquipmentStats::default();

    for (item_type, type_id, craft_name, gems) in equipped_items {
        query_one(conn, "PS_GameDefs", sql, &[&item_type, &type_id], |row| {
            stats.add_item_row(item_type as u8, row);
        })?;

        // Socketed gems (see Opcode::GemAdd) contribute the exact same
        // Effect1-4/Const* columns as the item itself, read off each
        // gem's own GEM_ITEM_TYPE/TypeID row -- reuses add_item_row
        // rather than a separate aggregation path.
        for &gem_type_id in gems.iter() {
            if gem_type_id == 0 {
                continue;
            }
            let gem_type_param = i32::from(GEM_ITEM_TYPE);
            let gem_type_id_param = i32::from(gem_type_id);
            query_one(
                conn,
                "PS_GameDefs",
                sql,
                &[&gem_type_param, &gem_type_id_param],
                |row| {
                    stats.add_item_row(GEM_ITEM_TYPE, row);
                },
            )?;
        }

        // Orange (recreated) bonus stats ride on top of the item's own
        // constants -- an equipped recreated item contributes both.
        if let Some(craft) = parse_craft_name(&craft_name) {
            stats.strength = stats.strength.saturating_add(craft.strength);
            stats.dexterity = stats.dexterity.saturating_add(craft.dexterity);
            stats.reaction = stats.reaction.saturating_add(craft.reaction);
            stats.intelligence = stats.intelligence.saturating_add(craft.intelligence);
            stats.wisdom = stats.wisdom.saturating_add(craft.wisdom);
            stats.luck = stats.luck.saturating_add(craft.luck);
            stats.max_hp = stats.max_hp.saturating_add(craft.max_hp);
            stats.max_mp = stats.max_mp.saturating_add(craft.max_mp);
            stats.max_sp = stats.max_sp.saturating_add(craft.max_sp);

            // Enchant bonus (see Opcode::EnchantAdd): a weapon's step
            // adds flat attack to BOTH physical and magic min/max (per
            // the user's explicit design); an armor piece's step adds
            // flat universal damage reduction ("absorption", applied in
            // combat::apply_incoming_mob_damage's final formula, not a
            // separate stat/packet). Index clamped to 20 (the design
            // max) in case a craftname somehow holds a stray value
            // above that.
            if craft.enchant_level > 0 {
                let step = usize::from(craft.enchant_level.min(20));
                if is_weapon_item_type(item_type as u8) {
                    let bonus = WEAPON_ENCHANT_ATTACK_BONUS[step];
                    stats.min_attack = stats.min_attack.saturating_add(bonus);
                    stats.max_attack = stats.max_attack.saturating_add(bonus);
                    stats.min_magic = stats.min_magic.saturating_add(bonus);
                    stats.max_magic = stats.max_magic.saturating_add(bonus);
                } else if is_armor_item_type(item_type as u8) {
                    stats.absorption =
                        stats.absorption.saturating_add(ARMOR_ENCHANT_ABSORPTION[step]);
                }
            }
        }
    }

    Ok(stats)
}

/// Per-item recreation parameters from PS_GameDefs.dbo.Items (see
/// `Opcode::ItemCompose`): `req_wis` = max value of each rolled bonus (0 =
/// not recreatable), `recreation_count` = how many bonuses get rolled
/// (`Server` column). NOTE: RealType is NOT a column in the canonical DB --
/// confirmed by a live "Invalid column name 'RealType'" against the exact
/// schema the retail server itself reads. Weapon/armor classification is
/// instead a user-confirmed Type-range table -- see
/// `is_weapon_item_type`/`is_armor_item_type`.
#[derive(Clone, Copy, Debug)]
pub struct DbComposeInfo {
    pub req_wis: u8,
    pub recreation_count: u8,
}

pub fn get_item_compose_info(
    conn: &mut Db,
    item_type: u8,
    type_id: u8,
) -> Result<Option<DbComposeInfo>> {
    let sql = r#"
        SELECT ReqWis, Server
        FROM Items
        WHERE Type = ?1
            AND TypeID = ?2
        LIMIT 1
    "#;

    let item_type_param = i32::from(item_type);
    let type_id_param = i32::from(type_id);
    let row = query_one(conn, "PS_GameDefs", sql, &[&item_type_param, &type_id_param], |row| {
        DbComposeInfo {
            req_wis: flexible_u32(row, "ReqWis", 0).min(99) as u8,
            recreation_count: flexible_u32(row, "Server", 0).min(9) as u8,
        }
    })?;
    Ok(row)
}

/// `Items.Type` for a gem/lapis, confirmed by the user against the retail
/// server (Type 30). Distinct from the "lapisia" enchant stone (`Type 95`,
/// see enchant.rs) -- a different item category despite the similar name.
pub const GEM_ITEM_TYPE: u8 = 30;

/// Target-item info needed to validate/apply a gem link (see
/// `Opcode::GemAdd`): `slots` = max gem sockets for this item TYPE
/// (`Items.Slot`, 0-6), `level` = this item's own required level
/// (`Items.ReqLevel`), checked against the gem's own `req_level`
/// (`get_gem_req_level`) -- confirmed by the user: a gem's `Items.ReqInt`
/// is the minimum LEVEL OF THE TARGET ITEM it can be linked to (not a
/// character-level check).
#[derive(Clone, Copy, Debug)]
pub struct DbGemTargetInfo {
    pub slots: u8,
    pub level: u16,
}

pub fn get_item_gem_target_info(
    conn: &mut Db,
    item_type: u8,
    type_id: u8,
) -> Result<Option<DbGemTargetInfo>> {
    let sql = r#"
        SELECT Slot, ReqLevel
        FROM Items
        WHERE Type = ?1
            AND TypeID = ?2
        LIMIT 1
    "#;

    let item_type_param = i32::from(item_type);
    let type_id_param = i32::from(type_id);
    let row = query_one(conn, "PS_GameDefs", sql, &[&item_type_param, &type_id_param], |row| {
        DbGemTargetInfo {
            slots: flexible_u32(row, "Slot", 0).min(6) as u8,
            level: positive_smallint_as_u16(row, "ReqLevel", 0),
        }
    })?;
    Ok(row)
}

/// A gem's own `Items.ReqInt` (see `DbGemTargetInfo`'s doc): the minimum
/// level a target item must have for this gem to be linkable to it. `None`
/// if the gem's item definition doesn't exist.
pub fn get_gem_req_level(conn: &mut Db, gem_type_id: u8) -> Result<Option<u16>> {
    let sql = r#"
        SELECT ReqInt
        FROM Items
        WHERE Type = ?1
            AND TypeID = ?2
        LIMIT 1
    "#;

    let gem_type_param = i32::from(GEM_ITEM_TYPE);
    let type_id_param = i32::from(gem_type_id);
    let row = query_one(conn, "PS_GameDefs", sql, &[&gem_type_param, &type_id_param], |row| {
        positive_smallint_as_u16(row, "ReqInt", 0)
    })?;
    Ok(row)
}

/// Writes one gem socket (`position` 0-5, mapping to `Gem1`..`Gem6`) on a
/// character item. `gem_type_id` 0 clears the socket (used by extraction).
/// The column name is chosen via a fixed match, never built from `position`
/// as a string, so there's no SQL injection surface.
pub fn set_character_item_gem(
    conn: &mut Db,
    character_id: u32,
    row_id: u32,
    position: u8,
    gem_type_id: u8,
) -> Result<()> {
    let column = match position {
        0 => "Gem1",
        1 => "Gem2",
        2 => "Gem3",
        3 => "Gem4",
        4 => "Gem5",
        _ => "Gem6",
    };
    let sql = format!("UPDATE CharItems SET {column} = ?1 WHERE CharID = ?2 AND RowID = ?3 AND Del = 0");
    let gem_type_id_param = i32::from(gem_type_id);
    let character_id_param = character_id as i32;
    let row_id_param = row_id as i32;
    execute(
        conn,
        "PS_GameData",
        &sql,
        &[&gem_type_id_param, &character_id_param, &row_id_param],
    )
    .context("failed to update character item gem")?;
    Ok(())
}

/// `Items.ReqLuc` on a weapon or armor piece: the exact item ID
/// (`Type * 1000 + TypeID`, same convention as `ProductList`/gift box) of
/// the enchant material this item requires, or 0 if it's not enchantable at
/// all (jewelry/capes always read 0 here by design -- see
/// `Opcode::EnchantAdd`). `None` if the item definition itself doesn't
/// exist.
pub fn get_item_req_luc(conn: &mut Db, item_type: u8, type_id: u8) -> Result<Option<u32>> {
    let sql = r#"
        SELECT ReqLuc
        FROM Items
        WHERE Type = ?1
            AND TypeID = ?2
        LIMIT 1
    "#;

    let item_type_param = i32::from(item_type);
    let type_id_param = i32::from(type_id);
    let row = query_one(conn, "PS_GameDefs", sql, &[&item_type_param, &type_id_param], |row| {
        flexible_u32(row, "ReqLuc", 0)
    })?;
    Ok(row)
}

/// Deducts `amount` gold if the character has enough, atomically (read then
/// write within the same call, mirroring `add_character_money`'s bit-
/// reinterpretation handling of the signed `Money` column). Returns the new
/// balance, or `None` if the character doesn't have `amount` -- in which
/// case nothing is written.
pub fn deduct_character_money(conn: &mut Db, character_id: u32, amount: u32) -> Result<Option<u32>> {
    let character_id_param = character_id as i32;
    let current = query_one(
        conn,
        "PS_GameData",
        "SELECT Money FROM Chars WHERE CharID = ?1",
        &[&character_id_param],
        |row| row.get::<_, i64>(0).unwrap_or_default() as i32,
    )?
    .context("character not found while deducting money")? as u32;
    if current < amount {
        return Ok(None);
    }
    let new_money = current - amount;
    let stored = new_money as i32;
    execute(
        conn,
        "PS_GameData",
        "UPDATE Chars SET Money = ?1 WHERE CharID = ?2",
        &[&stored, &character_id_param],
    )?;
    Ok(Some(new_money))
}

/// Persists a recreated item's new `Craftname` (see `DbCraftStats` for the
/// encoding). Scoped by CharID + RowID like every other item update.
pub fn set_character_item_craftname(
    conn: &mut Db,
    character_id: u32,
    row_id: u32,
    craft_name: &str,
) -> Result<()> {
    let character_id_param = character_id as i32;
    let row_id_param = row_id as i32;
    execute(
        conn,
        "PS_GameData",
        "UPDATE CharItems SET Craftname = ?1 WHERE CharID = ?2 AND RowID = ?3 AND Del = 0",
        &[&craft_name, &character_id_param, &row_id_param],
    )
    .context("failed to update item craftname")?;
    Ok(())
}

pub fn get_consumable_effect(
    conn: &mut Db,
    item_type: u8,
    type_id: u8,
) -> Result<Option<DbConsumableEffect>> {
    let sql = r#"
        SELECT
            Special, ConstHP, ConstSP, ConstMP, ReqIg
        FROM Items
        WHERE Type = ?1
            AND TypeID = ?2
        LIMIT 1
    "#;

    let item_type = i32::from(item_type);
    let type_id = i32::from(type_id);
    let row = query_one(conn, "PS_GameDefs", sql, &[&item_type, &type_id], |row| {
        DbConsumableEffect {
            special: tinyint_or_default(row, "Special", 0),
            hp: positive_smallint_as_u32(row, "ConstHP"),
            sp: positive_smallint_as_u32(row, "ConstSP"),
            mp: positive_smallint_as_u32(row, "ConstMP"),
            req_ig: tinyint_or_default(row, "ReqIg", 0),
        }
    })?;
    Ok(row)
}

pub fn weapon_attack_delay_ms(attack_time: u8) -> Option<(u64, u64)> {
    // Native client/server attack delays, indexed by Items.AttackTime:
    // 1=4000, 2=3000, 3=2750, 4=2500, 5=2250, 6=2000,
    // 7=1750, 8=1500, 9=1250, 10=1000 ms. AttackTime 0 is invalid.
    // Rapture currently uses accelerated test delays capped to 1500..500 ms.
    match attack_time {
        1 => Some((4_000, 1_500)),
        2 => Some((3_000, 1_500)),
        3 => Some((2_750, 1_375)),
        4 => Some((2_500, 1_250)),
        5 => Some((2_250, 1_125)),
        6 => Some((2_000, 1_000)),
        7 => Some((1_750, 875)),
        8 => Some((1_500, 750)),
        9 => Some((1_250, 625)),
        10 => Some((1_000, 500)),
        _ => None,
    }
}

pub fn get_weapon_combat(
    conn: &mut Db,
    item_type: u8,
    type_id: u8,
) -> Result<Option<DbWeaponCombat>> {
    let sql = r#"
        SELECT
            Type, TypeID, AttackTime, Range
        FROM Items
        WHERE Type = ?1
            AND TypeID = ?2
        LIMIT 1
    "#;

    let item_type_i32 = i32::from(item_type);
    let type_id_i32 = i32::from(type_id);
    let row = query_one(conn, "PS_GameDefs", sql, &[&item_type_i32, &type_id_i32], |row| {
        let attack_time = tinyint_or_default(row, "AttackTime", 0).min(10);
        let (original_delay_ms, test_delay_ms) = weapon_attack_delay_ms(attack_time)?;
        let range = flexible_f32(row, "Range", 2.0);
        let range = if range <= 0.0 { 2.0 } else { range };

        Some(DbWeaponCombat {
            item_type: tinyint_or_default(row, "Type", item_type),
            type_id: tinyint_or_default(row, "TypeID", type_id),
            attack_time,
            range,
            original_delay_ms,
            test_delay_ms,
        })
    })?;
    Ok(row.flatten())
}

pub fn get_item_definition(
    conn: &mut Db,
    item_type: u8,
    type_id: u8,
) -> Result<Option<DbItemDefinition>> {
    let sql = r#"
        SELECT
            Type, TypeID, Quality, Count, Sell
        FROM Items
        WHERE Type = ?1
            AND TypeID = ?2
        LIMIT 1
    "#;

    let item_type_i32 = i32::from(item_type);
    let type_id_i32 = i32::from(type_id);
    let row = query_one(conn, "PS_GameDefs", sql, &[&item_type_i32, &type_id_i32], |row| {
        DbItemDefinition {
            item_type: tinyint_or_default(row, "Type", item_type),
            type_id: tinyint_or_default(row, "TypeID", type_id),
            quality: positive_smallint_as_u16(row, "Quality", 0),
            max_stack: tinyint_or_default(row, "Count", 1).max(1),
            sell_price: row.get::<_, i64>("Sell").unwrap_or_default().max(0) as u32,
        }
    })?;
    Ok(row)
}

pub fn get_mob_drops(conn: &mut Db) -> Result<HashMap<u16, Vec<DbMobDrop>>> {
    let sql = r#"
        SELECT MobID, ItemOrder, Grade, DropRate
        FROM MobItems
        WHERE Grade > 0
            AND DropRate > 0
        ORDER BY MobID, ItemOrder
    "#;

    let rows = query_many(conn, "PS_GameDefs", sql, &[], |row| {
        (
            numeric_u16(row, "MobID", 0),
            tinyint_or_default(row, "ItemOrder", 0),
            numeric_u16(row, "Grade", 0),
            numeric_u32(row, "DropRate", 0),
        )
    })?;

    let mut drops = HashMap::<u16, Vec<DbMobDrop>>::new();
    for (mob_id, item_order, grade, drop_rate) in rows {
        if mob_id == 0 || grade == 0 || drop_rate == 0 {
            continue;
        }
        drops.entry(mob_id).or_default().push(DbMobDrop {
            mob_id,
            item_order,
            grade,
            drop_rate,
        });
    }

    Ok(drops)
}

pub fn get_item_definitions_by_grade(conn: &mut Db) -> Result<HashMap<u16, Vec<DbItemDefinition>>> {
    let sql = r#"
        SELECT Type, TypeID, Quality, Count, Grade
        FROM Items
        WHERE Grade > 0
            AND Type > 0
            AND TypeID > 0
        ORDER BY Grade, Type, TypeID
    "#;

    let rows = query_many(conn, "PS_GameDefs", sql, &[], |row| {
        (
            numeric_u16(row, "Grade", 0),
            DbItemDefinition {
                item_type: tinyint_or_default(row, "Type", 0),
                type_id: tinyint_or_default(row, "TypeID", 0),
                quality: positive_smallint_as_u16(row, "Quality", 0),
                max_stack: tinyint_or_default(row, "Count", 1).max(1),
                sell_price: 0,
            },
        )
    })?;

    let mut definitions = HashMap::<u16, Vec<DbItemDefinition>>::new();
    for (grade, definition) in rows {
        if grade == 0 {
            continue;
        }
        definitions.entry(grade).or_default().push(definition);
    }

    Ok(definitions)
}

pub fn get_mob_definitions(conn: &mut Db) -> Result<HashMap<u16, DbMobDefinition>> {
    let sql = r#"
        SELECT MobID, Level, HP, Money1, Money2, AttackSpecial3, Exp, Attack1, AI, ChaseRange
        FROM Mobs
    "#;

    let rows = query_many(conn, "PS_GameDefs", sql, &[], |row| {
        let mob_id = numeric_u16(row, "MobID", 0);
        (mob_id, mob_definition_from_row(row, mob_id))
    })?;

    Ok(rows
        .into_iter()
        .filter(|(mob_id, _)| *mob_id != 0)
        .collect())
}

/// Single-mob-id lookup, same columns/fallbacks as `get_mob_definitions`
/// but without loading all ~3700 rows -- used by GM `mmake` so a manually
/// summoned mob gets its real level/HP/money/exp instead of a hardcoded
/// placeholder.
pub fn get_mob_definition(conn: &mut Db, mob_id: u16) -> Result<Option<DbMobDefinition>> {
    let sql = r#"
        SELECT MobID, Level, HP, Money1, Money2, AttackSpecial3, Exp, Attack1, AI, ChaseRange
        FROM Mobs
        WHERE MobID = ?1
        LIMIT 1
    "#;

    let mob_id_param = i32::from(mob_id);
    let row = query_one(conn, "PS_GameDefs", sql, &[&mob_id_param], |row| {
        mob_definition_from_row(row, mob_id)
    })?;
    Ok(row)
}

/// Builds a `DbMobDefinition` from a `Mobs` row, shared by the load-all and
/// single-id lookups so both apply the exact same columns and fallbacks.
fn mob_definition_from_row(row: &rusqlite::Row<'_>, mob_id: u16) -> DbMobDefinition {
    DbMobDefinition {
        mob_id,
        level: numeric_u16(row, "Level", 1).max(1),
        max_hp: numeric_u32(row, "HP", 100).max(1),
        money_min: numeric_u32(row, "Money1", 0),
        money_max: numeric_u32(row, "Money2", 0),
        respawn_seconds: mob_respawn_seconds(tinyint_or_default(row, "AttackSpecial3", 0)),
        exp: numeric_u32(row, "Exp", 0),
        attack: flexible_u32(row, "Attack1", 0),
        ai: flexible_u32(row, "AI", 0).min(u32::from(u8::MAX)) as u8,
        chase_range: flexible_f32(row, "ChaseRange", 0.0),
    }
}

/// Exp required to advance FROM `(Level, Grow)` to the next level, from
/// PS_GameDefs.dbo.ExpDefs. Keyed by the table's own 1-4 `Grow` value --
/// NOT the same range as `DbCharacter::mode`/`CharAttribute::Grow` (0-3):
/// callers must look up with `mode + 1`.
pub fn get_exp_definitions(conn: &mut Db) -> Result<HashMap<(u16, u8), u32>> {
    let sql = r#"
        SELECT Level, Grow, Exp
        FROM ExpDefs
    "#;

    let rows = query_many(conn, "PS_GameDefs", sql, &[], |row| {
        let level = positive_smallint_as_u16(row, "Level", 0);
        let grow = tinyint_or_default(row, "Grow", 1);
        let exp = numeric_u32(row, "Exp", 0);
        ((level, grow), exp)
    })?;

    Ok(rows.into_iter().collect())
}

/// Persists a character's appearance (and, for the change-sex item, sex).
/// `size` maps to the `Size` column (this project's `height`), `sex` to `Sex`
/// (0 = male, 1 = female). Used by the change-appearance/change-sex
/// consumables (see `Opcode::ChangeAppearance`).
pub fn set_character_appearance(
    conn: &mut Db,
    character_id: u32,
    hair: u8,
    face: u8,
    size: u8,
    sex: u8,
) -> Result<()> {
    let hair_param = i32::from(hair);
    let face_param = i32::from(face);
    let size_param = i32::from(size);
    let sex_param = i32::from(sex);
    let character_id_param = character_id as i32;
    execute(
        conn,
        "PS_GameData",
        "UPDATE Chars SET Hair = ?1, Face = ?2, Size = ?3, Sex = ?4 WHERE CharID = ?5",
        &[&hair_param, &face_param, &size_param, &sex_param, &character_id_param],
    )
    .context("failed to update character appearance")?;
    Ok(())
}

/// Persists a stat respec: all six base stats back to `base_stat` (10) and
/// the refunded available point pool. Used by the reset-stats stone (see
/// `Opcode::StatsReset`). HP/MP/SP columns are left untouched -- they store
/// the level/job base max (StatusDefs), and the stat-derived portion of the
/// max is recomputed live from the reset stats, not persisted.
pub fn set_character_reset_stats(
    conn: &mut Db,
    character_id: u32,
    base_stat: u16,
    stat_point: u16,
) -> Result<()> {
    let base_param = i32::from(base_stat);
    let stat_point_param = i32::from(stat_point);
    let character_id_param = character_id as i32;
    execute(
        conn,
        "PS_GameData",
        "UPDATE Chars \
         SET Str = ?1, Dex = ?1, Rec = ?1, Int = ?1, Wis = ?1, Luc = ?1, StatPoint = ?2 \
         WHERE CharID = ?3",
        &[&base_param, &stat_point_param, &character_id_param],
    )
    .context("failed to reset character stats")?;
    Ok(())
}

/// Writes `HP`/`MP`/`SP` directly (absolute values, not a delta) -- used
/// after a level-up sets the character's new base max from StatusDefs.
pub fn set_character_status(
    conn: &mut Db,
    character_id: u32,
    hp: u32,
    mp: u32,
    sp: u32,
) -> Result<()> {
    let hp_param = hp as i32;
    let mp_param = mp as i32;
    let sp_param = sp as i32;
    let character_id_param = character_id as i32;
    execute(
        conn,
        "PS_GameData",
        "UPDATE Chars SET HP = ?1, MP = ?2, SP = ?3 WHERE CharID = ?4",
        &[&hp_param, &mp_param, &sp_param, &character_id_param],
    )
    .context("failed to update character status")?;
    Ok(())
}

/// Writes both `Level` and `Exp` in one round trip -- used after a kill
/// awards exp, which touches Exp on every kill and Level on most of them,
/// so this avoids two separate UPDATE statements on what can be a hot path.
pub fn set_character_level_exp(
    conn: &mut Db,
    character_id: u32,
    level: u16,
    exp: u32,
) -> Result<()> {
    let level_param = level as i32;
    let exp_param = exp as i32;
    let character_id_param = character_id as i32;
    execute(
        conn,
        "PS_GameData",
        "UPDATE Chars SET Level = ?1, Exp = ?2 WHERE CharID = ?3",
        &[&level_param, &exp_param, &character_id_param],
    )
    .context("failed to update character level/exp")?;
    Ok(())
}

pub fn mob_respawn_seconds(attack_special_3: u8) -> Option<u64> {
    match attack_special_3 {
        0 => Some(40),
        1 => Some(40),
        2 => Some(60),
        3 => Some(3 * 60),
        4 => Some(8 * 60),
        5 => Some(12 * 60),
        6 => Some(15 * 60),
        7 => Some(30 * 60),
        8 => Some(45 * 60),
        9 => Some(60 * 60),
        10 => Some(12 * 60 * 60),
        11 => Some(24 * 60 * 60),
        12 => Some(3 * 24 * 60 * 60),
        13 => Some(4 * 24 * 60 * 60),
        14 => Some(6 * 24 * 60 * 60),
        15 => Some(7 * 24 * 60 * 60),
        _ => None,
    }
}

pub fn create_character_item(
    conn: &mut Db,
    character_id: u32,
    item: &DbItemDefinition,
    bag: u8,
    slot: u8,
    count: u8,
) -> Result<DbCharacterItem> {
    let sql = r#"
        INSERT INTO CharItems (
            CharID, ItemID, ItemUID, Type, TypeID, Bag, Slot, Quality,
            Gem1, Gem2, Gem3, Gem4, Gem5, Gem6, Craftname, Count, Maketime, Maketype, Del
        )
        VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            0, 0, 0, 0, 0, 0, '', ?9, datetime('now'), 'S', 0
        )
    "#;

    let character_id_i32 = character_id as i32;
    let item_id = i32::from(item.item_type) * 1000 + i32::from(item.type_id);
    let item_uid = random_item_uid();
    let item_type = i32::from(item.item_type);
    let type_id = i32::from(item.type_id);
    let bag_i32 = i32::from(bag);
    let slot_i32 = i32::from(slot);
    let quality = i32::from(item.quality);
    let count_i32 = i32::from(count.max(1));
    let row_id = insert_and_last_row_id(
        conn,
        "PS_GameData",
        sql,
        &[
            &character_id_i32,
            &item_id,
            &item_uid,
            &item_type,
            &type_id,
            &bag_i32,
            &slot_i32,
            &quality,
            &count_i32,
        ],
    )
    .context("failed to create character item")?;

    Ok(DbCharacterItem {
        row_id: row_id.max(0) as u32,
        bag,
        slot,
        item_type: item.item_type,
        type_id: item.type_id,
        quality: item.quality,
        gems: [0; 6],
        count: count.max(1),
        craft_name: String::new(),
        dyed: false,
    })
}

pub fn create_character_item_from_template(
    conn: &mut Db,
    character_id: u32,
    item: &DbCharacterItem,
    bag: u8,
    slot: u8,
    count: u8,
) -> Result<DbCharacterItem> {
    let sql = r#"
        INSERT INTO CharItems (
            CharID, ItemID, ItemUID, Type, TypeID, Bag, Slot, Quality,
            Gem1, Gem2, Gem3, Gem4, Gem5, Gem6, Craftname, Count, Maketime, Maketype, Del
        )
        VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, datetime('now'), 'S', 0
        )
    "#;

    let character_id_i32 = character_id as i32;
    let item_id = i32::from(item.item_type) * 1000 + i32::from(item.type_id);
    let item_uid = random_item_uid();
    let item_type = i32::from(item.item_type);
    let type_id = i32::from(item.type_id);
    let bag_i32 = i32::from(bag);
    let slot_i32 = i32::from(slot);
    let quality = i32::from(item.quality);
    let gem1 = i32::from(item.gems[0]);
    let gem2 = i32::from(item.gems[1]);
    let gem3 = i32::from(item.gems[2]);
    let gem4 = i32::from(item.gems[3]);
    let gem5 = i32::from(item.gems[4]);
    let gem6 = i32::from(item.gems[5]);
    let count_i32 = i32::from(count.max(1));
    let row_id = insert_and_last_row_id(
        conn,
        "PS_GameData",
        sql,
        &[
            &character_id_i32,
            &item_id,
            &item_uid,
            &item_type,
            &type_id,
            &bag_i32,
            &slot_i32,
            &quality,
            &gem1,
            &gem2,
            &gem3,
            &gem4,
            &gem5,
            &gem6,
            &item.craft_name,
            &count_i32,
        ],
    )
    .context("failed to create character item from template")?;

    let mut created = item.clone();
    created.row_id = row_id.max(0) as u32;
    created.bag = bag;
    created.slot = slot;
    created.count = count.max(1);
    Ok(created)
}

pub fn move_character_item(
    conn: &mut Db,
    character_id: u32,
    row_id: u32,
    bag: u8,
    slot: u8,
) -> Result<()> {
    let sql = r#"
        UPDATE CharItems
        SET Bag = ?3, Slot = ?4
        WHERE CharID = ?1
            AND RowID = ?2
            AND Del = 0
    "#;

    let character_id = character_id as i32;
    let row_id = row_id as i32;
    let bag = i32::from(bag);
    let slot = i32::from(slot);
    execute(
        conn,
        "PS_GameData",
        sql,
        &[&character_id, &row_id, &bag, &slot],
    )
    .context("failed to move character item")
}

pub fn set_character_item_count(
    conn: &mut Db,
    character_id: u32,
    row_id: u32,
    count: u8,
) -> Result<()> {
    if count == 0 {
        return delete_character_item(conn, character_id, row_id);
    }

    let sql = r#"
        UPDATE CharItems
        SET Count = ?3
        WHERE CharID = ?1
            AND RowID = ?2
            AND Del = 0
    "#;

    let character_id = character_id as i32;
    let row_id = row_id as i32;
    let count = i32::from(count);
    execute(conn, "PS_GameData", sql, &[&character_id, &row_id, &count])
        .context("failed to update character item count")
}

pub fn delete_character_item(conn: &mut Db, character_id: u32, row_id: u32) -> Result<()> {
    let sql = r#"
        UPDATE CharItems
        SET Count = 0, Del = 1
        WHERE CharID = ?1
            AND RowID = ?2
            AND Del = 0
    "#;

    let character_id = character_id as i32;
    let row_id = row_id as i32;
    execute(conn, "PS_GameData", sql, &[&character_id, &row_id])
        .context("failed to delete character item")
}

pub fn consume_character_item(
    conn: &mut Db,
    character_id: u32,
    row_id: u32,
    remaining_count: u8,
) -> Result<()> {
    if remaining_count == 0 {
        let sql = r#"
            UPDATE CharItems
            SET Count = 0, Del = 1
            WHERE CharID = ?1
                AND RowID = ?2
                AND Del = 0
        "#;

        let character_id = character_id as i32;
        let row_id = row_id as i32;
        return execute(conn, "PS_GameData", sql, &[&character_id, &row_id])
            .context("failed to remove consumed character item");
    }

    let sql = r#"
        UPDATE CharItems
        SET Count = ?3
        WHERE CharID = ?1
            AND RowID = ?2
            AND Del = 0
    "#;

    let character_id = character_id as i32;
    let row_id = row_id as i32;
    let remaining_count = i32::from(remaining_count);
    execute(
        conn,
        "PS_GameData",
        sql,
        &[&character_id, &row_id, &remaining_count],
    )
    .context("failed to update consumed character item count")
}

pub fn swap_character_items(
    conn: &mut Db,
    character_id: u32,
    first_row_id: u32,
    first_bag: u8,
    first_slot: u8,
    second_row_id: u32,
    second_bag: u8,
    second_slot: u8,
) -> Result<()> {
    let sql = r#"
        UPDATE CharItems
        SET
            Bag = CASE RowID
                WHEN ?2 THEN ?3
                WHEN ?5 THEN ?6
                ELSE Bag
            END,
            Slot = CASE RowID
                WHEN ?2 THEN ?4
                WHEN ?5 THEN ?7
                ELSE Slot
            END
        WHERE CharID = ?1
            AND RowID IN (?2, ?5)
            AND Del = 0
    "#;

    let character_id = character_id as i32;
    let first_row_id = first_row_id as i32;
    let first_bag = i32::from(first_bag);
    let first_slot = i32::from(first_slot);
    let second_row_id = second_row_id as i32;
    let second_bag = i32::from(second_bag);
    let second_slot = i32::from(second_slot);
    execute(
        conn,
        "PS_GameData",
        sql,
        &[
            &character_id,
            &first_row_id,
            &first_bag,
            &first_slot,
            &second_row_id,
            &second_bag,
            &second_slot,
        ],
    )
    .context("failed to swap character items")
}

/// Warehouse storage (`PS_GameData.dbo.UserStoredItems`) is keyed by
/// `UserUID` (account), not `CharID` -- shared across every character on the
/// account, matching retail Shaiya's per-account bank/warehouse. Row shape
/// mirrors `CharItems` closely (see `get_character_items`) minus the `Bag`
/// column, since there's only one storage "bag" per account.
pub fn get_stored_items(conn: &mut Db, account_id: u32) -> Result<Vec<DbCharacterItem>> {
    let sql = r#"
        SELECT
            RowID, Slot, Type, TypeID, Quality,
            Gem1, Gem2, Gem3, Gem4, Gem5, Gem6,
            Count, Craftname
        FROM UserStoredItems
        WHERE UserUID = ?1
            AND Del = 0
            AND Count > 0
        ORDER BY Slot
    "#;

    let account_id = account_id as i32;
    query_many(conn, "PS_GameData", sql, &[&account_id], |row| {
        row_to_character_item(row, Some(0))
    })
}

pub fn create_stored_item_from_template(
    conn: &mut Db,
    account_id: u32,
    item: &DbCharacterItem,
    slot: u8,
    count: u8,
) -> Result<DbCharacterItem> {
    let sql = r#"
        INSERT INTO UserStoredItems (
            ServerID, UserUID, ItemID, Type, TypeID, ItemUID, Slot, Quality,
            Gem1, Gem2, Gem3, Gem4, Gem5, Gem6, Craftname, Count, Maketime, Maketype, Del
        )
        VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, datetime('now'), 'S', 0
        )
    "#;

    let server_id = i32::from(SERVER_ID);
    let account_id_i32 = account_id as i32;
    let item_id = i32::from(item.item_type) * 1000 + i32::from(item.type_id);
    let item_type = i32::from(item.item_type);
    let type_id = i32::from(item.type_id);
    let item_uid = random_item_uid();
    let slot_i32 = i32::from(slot);
    let quality = i32::from(item.quality);
    let gem1 = i32::from(item.gems[0]);
    let gem2 = i32::from(item.gems[1]);
    let gem3 = i32::from(item.gems[2]);
    let gem4 = i32::from(item.gems[3]);
    let gem5 = i32::from(item.gems[4]);
    let gem6 = i32::from(item.gems[5]);
    let count_i32 = i32::from(count.max(1));
    let row_id = insert_and_last_row_id(
        conn,
        "PS_GameData",
        sql,
        &[
            &server_id,
            &account_id_i32,
            &item_id,
            &item_type,
            &type_id,
            &item_uid,
            &slot_i32,
            &quality,
            &gem1,
            &gem2,
            &gem3,
            &gem4,
            &gem5,
            &gem6,
            &item.craft_name,
            &count_i32,
        ],
    )
    .context("failed to create stored item")?;

    let mut created = item.clone();
    created.row_id = row_id.max(0) as u32;
    created.bag = 0;
    created.slot = slot;
    created.count = count.max(1);
    Ok(created)
}

pub fn move_stored_item(conn: &mut Db, account_id: u32, row_id: u32, slot: u8) -> Result<()> {
    let sql = r#"
        UPDATE UserStoredItems
        SET Slot = ?3
        WHERE UserUID = ?1
            AND RowID = ?2
            AND Del = 0
    "#;

    let account_id = account_id as i32;
    let row_id = row_id as i32;
    let slot = i32::from(slot);
    execute(conn, "PS_GameData", sql, &[&account_id, &row_id, &slot])
        .context("failed to move stored item")
}

pub fn swap_stored_items(
    conn: &mut Db,
    account_id: u32,
    first_row_id: u32,
    first_slot: u8,
    second_row_id: u32,
    second_slot: u8,
) -> Result<()> {
    let sql = r#"
        UPDATE UserStoredItems
        SET Slot = CASE RowID
                WHEN ?2 THEN ?3
                WHEN ?4 THEN ?5
                ELSE Slot
            END
        WHERE UserUID = ?1
            AND RowID IN (?2, ?4)
            AND Del = 0
    "#;

    let account_id = account_id as i32;
    let first_row_id = first_row_id as i32;
    let first_slot = i32::from(first_slot);
    let second_row_id = second_row_id as i32;
    let second_slot = i32::from(second_slot);
    execute(
        conn,
        "PS_GameData",
        sql,
        &[
            &account_id,
            &first_row_id,
            &first_slot,
            &second_row_id,
            &second_slot,
        ],
    )
    .context("failed to swap stored items")
}

pub fn set_stored_item_count(
    conn: &mut Db,
    account_id: u32,
    row_id: u32,
    count: u8,
) -> Result<()> {
    if count == 0 {
        return delete_stored_item(conn, account_id, row_id);
    }

    let sql = r#"
        UPDATE UserStoredItems
        SET Count = ?3
        WHERE UserUID = ?1
            AND RowID = ?2
            AND Del = 0
    "#;

    let account_id = account_id as i32;
    let row_id = row_id as i32;
    let count = i32::from(count);
    execute(conn, "PS_GameData", sql, &[&account_id, &row_id, &count])
        .context("failed to update stored item count")
}

pub fn delete_stored_item(conn: &mut Db, account_id: u32, row_id: u32) -> Result<()> {
    let sql = r#"
        UPDATE UserStoredItems
        SET Count = 0, Del = 1
        WHERE UserUID = ?1
            AND RowID = ?2
            AND Del = 0
    "#;

    let account_id = account_id as i32;
    let row_id = row_id as i32;
    execute(conn, "PS_GameData", sql, &[&account_id, &row_id])
        .context("failed to delete stored item")
}

pub fn get_character_quick_slots(conn: &mut Db, character_id: u32) -> Result<Vec<DbQuickSlot>> {
    let sql = r#"
        SELECT QuickBar, QuickSlot, Bag, Number
        FROM CharQuickSlots
        WHERE CharID = ?1
        ORDER BY QuickBar, QuickSlot
    "#;

    let character_id = character_id as i32;
    query_many(conn, "PS_GameData", sql, &[&character_id], |row| DbQuickSlot {
        quick_bar: tinyint_or_default(row, "QuickBar", 0),
        quick_slot: tinyint_or_default(row, "QuickSlot", 0),
        bag: tinyint_or_default(row, "Bag", 0),
        number: positive_smallint_as_u16(row, "Number", 0),
    })
}

pub fn save_character_quick_slots(
    conn: &mut Db,
    character_id: u32,
    quick_slots: &[DbQuickSlot],
) -> Result<()> {
    let character_id_param = character_id as i32;
    execute(
        conn,
        "PS_GameData",
        "DELETE FROM CharQuickSlots WHERE CharID = ?1",
        &[&character_id_param],
    )
    .context("failed to clear character quick slots")?;

    for quick_slot in quick_slots.iter().take(128) {
        let sql = r#"
            INSERT INTO CharQuickSlots (
                CharID, QuickBar, QuickSlot, Bag, Number
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
        "#;

        let quick_bar = i32::from(quick_slot.quick_bar);
        let quick_slot_index = i32::from(quick_slot.quick_slot);
        let bag = i32::from(quick_slot.bag);
        let number = i32::from(quick_slot.number);
        execute(
            conn,
            "PS_GameData",
            sql,
            &[&character_id_param, &quick_bar, &quick_slot_index, &bag, &number],
        )
        .context("failed to insert character quick slot")?;
    }

    Ok(())
}

pub fn get_character_skills(conn: &mut Db, character_id: u32) -> Result<Vec<DbCharacterSkill>> {
    // `CreateTime` was historically minute-precision (`smalldatetime`), so
    // the moment a cooldown started could be stored up to ~30s off from
    // when it really started. A 30s safety margin is subtracted here so
    // that worst-case rounding error can only make a skill appear ready a
    // little EARLY, never stuck on cooldown longer than it really should
    // be -- the direction that matters, since "ready a bit early" is a
    // minor inconvenience but "stuck longer than real" would look broken.
    let sql = r#"
        SELECT SkillID, SkillLevel, Number,
            CASE
                WHEN Delay - (strftime('%s','now') - strftime('%s', CreateTime)) - 30 > 0
                    THEN Delay - (strftime('%s','now') - strftime('%s', CreateTime)) - 30
                ELSE 0
            END AS RemainingCooldown
        FROM CharSkills
        WHERE CharID = ?1
            AND Del = 0
        ORDER BY Number
    "#;

    let character_id = character_id as i32;
    query_many(conn, "PS_GameData", sql, &[&character_id], |row| DbCharacterSkill {
        skill_id: positive_smallint_as_u16(row, "SkillID", 0),
        skill_level: tinyint_or_default(row, "SkillLevel", 1),
        number: tinyint_or_default(row, "Number", 0),
        delay: row.get::<_, i64>("RemainingCooldown").unwrap_or_default().max(0) as i32,
    })
}

pub fn get_skill_definition(
    conn: &mut Db,
    skill_id: u16,
    skill_level: u8,
) -> Result<Option<DbSkillDefinition>> {
    let sql = r#"
        SELECT
            SkillID, SkillLevel, ReqLevel, SkillPoint, PrevSkillID,
            Attackfighter, Defensefighter, Patrolrogue, Shootrogue,
            Attackmage, Defensemage,
            SP, MP, ResetTime, ReadyTime, DamageHP, AddDamageHP, HealHP, HealSP, HealMP,
            TypeDetail, KeepTime, TimeHealHP, TimeHealSP, TimeHealMP,
            AbilityType1, AbilityValue1, AbilityType2, AbilityValue2,
            AbilityType3, AbilityValue3, AbilityType4, AbilityValue4,
            AbilityType5, AbilityValue5, AbilityType6, AbilityValue6,
            AbilityType7, AbilityValue7, AbilityType8, AbilityValue8,
            AbilityType9, AbilityValue9, AbilityType10, AbilityValue10,
            TimeDamageHP, TimeDamageSP, TimeDamageMP,
            AttackRange, TypeAttack, TargetType, ApplyRange, MultiAttack
        FROM Skills
        WHERE SkillID = ?1
            AND SkillLevel = ?2
        LIMIT 1
    "#;

    let skill_id_i32 = i32::from(skill_id);
    let skill_level_i32 = i32::from(skill_level);
    let row = query_one(conn, "PS_GameDefs", sql, &[&skill_id_i32, &skill_level_i32], |row| {
        DbSkillDefinition {
            skill_id: row
                .get::<_, i64>("SkillID")
                .map(|value| value.max(0) as u16)
                .unwrap_or(skill_id),
            skill_level: tinyint_or_default(row, "SkillLevel", skill_level),
            req_level: row.get::<_, i64>("ReqLevel").unwrap_or_default().max(0) as u16,
            skill_point_cost: tinyint_or_default(row, "SkillPoint", 0),
            prev_skill_id: row.get::<_, i64>("PrevSkillID").unwrap_or_default().max(0) as u16,
            class_eligible: [
                tinyint_or_default(row, "Attackfighter", 0) != 0,
                tinyint_or_default(row, "Defensefighter", 0) != 0,
                tinyint_or_default(row, "Patrolrogue", 0) != 0,
                tinyint_or_default(row, "Shootrogue", 0) != 0,
                tinyint_or_default(row, "Attackmage", 0) != 0,
                tinyint_or_default(row, "Defensemage", 0) != 0,
            ],
            mp_cost: row.get::<_, i64>("MP").unwrap_or_default().max(0) as u16,
            sp_cost: row.get::<_, i64>("SP").unwrap_or_default().max(0) as u16,
            reset_time_seconds: row.get::<_, i64>("ResetTime").unwrap_or_default().max(0) as u16,
            damage_hp: row.get::<_, i64>("DamageHP").unwrap_or_default() as i32,
            add_damage_hp: row.get::<_, i64>("AddDamageHP").unwrap_or_default() as i32,
            heal_hp: row.get::<_, i64>("HealHP").unwrap_or_default() as i32,
            heal_sp: row.get::<_, i64>("HealSP").unwrap_or_default() as i32,
            heal_mp: row.get::<_, i64>("HealMP").unwrap_or_default() as i32,
            type_detail: positive_smallint_as_u16(row, "TypeDetail", 0),
            keep_time: positive_smallint_as_u16(row, "KeepTime", 0),
            time_heal_hp: row.get::<_, i64>("TimeHealHP").unwrap_or_default() as i32,
            time_heal_sp: row.get::<_, i64>("TimeHealSP").unwrap_or_default() as i32,
            time_heal_mp: row.get::<_, i64>("TimeHealMP").unwrap_or_default() as i32,
            ready_time: tinyint_or_default(row, "ReadyTime", 0),
            ability_type: [
                tinyint_or_default(row, "AbilityType1", 0),
                tinyint_or_default(row, "AbilityType2", 0),
                tinyint_or_default(row, "AbilityType3", 0),
                tinyint_or_default(row, "AbilityType4", 0),
                tinyint_or_default(row, "AbilityType5", 0),
                tinyint_or_default(row, "AbilityType6", 0),
                tinyint_or_default(row, "AbilityType7", 0),
                tinyint_or_default(row, "AbilityType8", 0),
                tinyint_or_default(row, "AbilityType9", 0),
                tinyint_or_default(row, "AbilityType10", 0),
            ],
            ability_value: [
                row.get::<_, i64>("AbilityValue1").unwrap_or_default() as i16,
                row.get::<_, i64>("AbilityValue2").unwrap_or_default() as i16,
                row.get::<_, i64>("AbilityValue3").unwrap_or_default() as i16,
                row.get::<_, i64>("AbilityValue4").unwrap_or_default() as i16,
                row.get::<_, i64>("AbilityValue5").unwrap_or_default() as i16,
                row.get::<_, i64>("AbilityValue6").unwrap_or_default() as i16,
                row.get::<_, i64>("AbilityValue7").unwrap_or_default() as i16,
                row.get::<_, i64>("AbilityValue8").unwrap_or_default() as i16,
                row.get::<_, i64>("AbilityValue9").unwrap_or_default() as i16,
                row.get::<_, i64>("AbilityValue10").unwrap_or_default() as i16,
            ],
            time_damage_hp: row.get::<_, i64>("TimeDamageHP").unwrap_or_default() as i32,
            time_damage_sp: row.get::<_, i64>("TimeDamageSP").unwrap_or_default() as i32,
            time_damage_mp: row.get::<_, i64>("TimeDamageMP").unwrap_or_default() as i32,
            attack_range: tinyint_or_default(row, "AttackRange", 0),
            type_attack: tinyint_or_default(row, "TypeAttack", 0),
            target_type: tinyint_or_default(row, "TargetType", 0),
            apply_range: tinyint_or_default(row, "ApplyRange", 0),
            multi_attack: tinyint_or_default(row, "MultiAttack", 0),
        }
    })?;
    Ok(row)
}

/// `CharSkills.Delay` holds the cooldown's DURATION in seconds and
/// `CreateTime` (otherwise unused after a skill is learned -- only written
/// once at learn time and never read back) is repurposed as "cooldown
/// started at", so `get_character_skills` can always recompute an accurate
/// "seconds remaining" regardless of how much real time passed since this
/// was set (including across a relog or a long time offline).
pub fn set_character_skill_cooldown(
    conn: &mut Db,
    character_id: u32,
    skill_id: u16,
    duration_seconds: u16,
) -> Result<()> {
    let sql = r#"
        UPDATE CharSkills
        SET Delay = ?3, CreateTime = datetime('now')
        WHERE CharID = ?1
            AND SkillID = ?2
    "#;

    let character_id_param = character_id as i32;
    let skill_id_param = i32::from(skill_id);
    let duration_param = i32::from(duration_seconds);
    execute(
        conn,
        "PS_GameData",
        sql,
        &[&character_id_param, &skill_id_param, &duration_param],
    )
    .context("failed to update character skill cooldown")
}

#[derive(Clone, Copy, Debug)]
pub struct DbCharacterAppliedBuff {
    pub skill_id: u16,
    pub skill_level: u8,
    pub left_reset_time_seconds: u32,
}

/// Reads every buff row persisted for this character in `CharApplySkills`.
/// Unlike skill cooldowns, buffs freeze while offline rather than
/// continuing to count down (confirmed by the user), so `LeftResetTime`
/// is a plain snapshot of remaining seconds -- no timestamp trick is
/// needed to recompute elapsed offline time.
pub fn get_character_apply_skills(conn: &mut Db, character_id: u32) -> Result<Vec<DbCharacterAppliedBuff>> {
    let sql = r#"
        SELECT SkillID, SkillLevel, LeftResetTime
        FROM CharApplySkills
        WHERE CharID = ?1
    "#;

    let character_id_param = character_id as i32;
    query_many(conn, "PS_GameData", sql, &[&character_id_param], |row| {
        DbCharacterAppliedBuff {
            skill_id: positive_smallint_as_u16(row, "SkillID", 0),
            skill_level: tinyint_or_default(row, "SkillLevel", 0),
            left_reset_time_seconds: row
                .get::<_, i64>("LeftResetTime")
                .unwrap_or_default()
                .max(0) as u32,
        }
    })
}

/// Upserts one buff's remaining seconds -- `CharApplySkills` has no unique
/// constraint on (CharID, SkillID), so this deletes any existing row for
/// the pair before inserting the fresh one (same pattern already used by
/// `save_character_quick_slots`).
pub fn set_character_apply_skill(
    conn: &mut Db,
    character_id: u32,
    skill_id: u16,
    skill_level: u8,
    left_reset_time_seconds: u32,
) -> Result<()> {
    clear_character_apply_skill(conn, character_id, skill_id)?;

    let sql = r#"
        INSERT INTO CharApplySkills (CharID, SkillID, SkillLevel, LeftResetTime)
        VALUES (?1, ?2, ?3, ?4)
    "#;
    let character_id_param = character_id as i32;
    let skill_id_param = i32::from(skill_id);
    let skill_level_param = i32::from(skill_level);
    let left_reset_time_param = left_reset_time_seconds as i32;
    execute(
        conn,
        "PS_GameData",
        sql,
        &[&character_id_param, &skill_id_param, &skill_level_param, &left_reset_time_param],
    )
    .context("failed to insert character apply skill")
}

pub fn clear_character_apply_skill(conn: &mut Db, character_id: u32, skill_id: u16) -> Result<()> {
    let character_id_param = character_id as i32;
    let skill_id_param = i32::from(skill_id);
    execute(
        conn,
        "PS_GameData",
        "DELETE FROM CharApplySkills WHERE CharID = ?1 AND SkillID = ?2",
        &[&character_id_param, &skill_id_param],
    )
    .context("failed to clear character apply skill")
}

pub fn learn_character_skill(
    conn: &mut Db,
    character_id: u32,
    skill_id: u16,
    skill_level: u8,
) -> Result<DbCharacterSkill> {
    if let Some(existing) = get_character_skills(conn, character_id)?
        .into_iter()
        .find(|skill| skill.skill_id == skill_id)
    {
        let sql = r#"
            UPDATE CharSkills
            SET SkillLevel = ?3, Delay = 0, Del = 0
            WHERE CharID = ?1
                AND SkillID = ?2
        "#;

        let character_id = character_id as i32;
        let skill_id_i32 = skill_id as i32;
        let skill_level_i32 = i32::from(skill_level);
        execute(
            conn,
            "PS_GameData",
            sql,
            &[&character_id, &skill_id_i32, &skill_level_i32],
        )
        .context("failed to update character skill")?;

        return Ok(DbCharacterSkill {
            skill_id,
            skill_level,
            number: existing.number,
            delay: 0,
        });
    }

    let next_number = next_character_skill_number(conn, character_id)?;
    let sql = r#"
        INSERT INTO CharSkills (
            CharID, SkillID, SkillLevel, Number, Delay, CreateTime, Del
        )
        VALUES (?1, ?2, ?3, ?4, 0, datetime('now'), 0)
    "#;

    let character_id_i32 = character_id as i32;
    let skill_id_i32 = skill_id as i32;
    let skill_level_i32 = i32::from(skill_level);
    let number_i32 = i32::from(next_number);
    execute(
        conn,
        "PS_GameData",
        sql,
        &[
            &character_id_i32,
            &skill_id_i32,
            &skill_level_i32,
            &number_i32,
        ],
    )
    .context("failed to insert character skill")?;

    Ok(DbCharacterSkill {
        skill_id,
        skill_level,
        number: next_number,
        delay: 0,
    })
}

fn next_character_skill_number(conn: &mut Db, character_id: u32) -> Result<u8> {
    // SQLite's MAX() over an empty set returns NULL cleanly (no tinyint-
    // typing overflow gotcha like SQL Server had, where ISNULL's
    // replacement value got type-matched to MAX(Number)'s own narrow
    // column type before the outer cast ever ran) -- COALESCE(MAX(Number),
    // -1) is a direct, safe base case: no skills yet means "start at 0"
    // (next = -1 + 1).
    let sql = r#"
        SELECT COALESCE(MAX(Number), -1) AS MaxNumber
        FROM CharSkills
        WHERE CharID = ?1
    "#;

    let character_id_param = character_id as i32;
    let max_number = query_one(conn, "PS_GameData", sql, &[&character_id_param], |row| {
        row.get::<_, i64>("MaxNumber").unwrap_or(-1)
    })?
    .unwrap_or(-1);
    let next = max_number.saturating_add(1);
    if next > i64::from(u8::MAX) {
        anyhow::bail!(
            "character {character_id} already has the maximum of {} skill bar entries",
            u8::MAX as i32 + 1
        );
    }
    Ok(next as u8)
}

pub fn create_character(
    conn: &mut Db,
    account_id: u32,
    character: &NewCharacter,
) -> Result<DbCharacter> {
    let account = get_account_by_id(conn, account_id)?.context("account not found")?;
    // Base HP/MP/SP for a fresh level-1 character: a hardcoded flat 100 for
    // every class (see character::base_pool_for_level -- level 1 = 100),
    // deliberately NOT sourced from PS_GameDefs.dbo.StatusDefs per the
    // project's design.
    let (start_hp, start_mp, start_sp) = (100u32, 100u32, 100u32);

    let sql = r#"
        INSERT INTO Chars (
            ServerID, UserID, UserUID, CharName, Slot, Family, Grow, Hair, Face, Size, Job, Sex,
            Level, StatPoint, SkillPoint, Str, Dex, Rec, Int, Luc, Wis, HP, MP, SP,
            Map, Dir, Exp, Money, PosX, PosY, Posz, Hg, Vg, Cg, Og, Ig,
            K1, K2, K3, K4, KillLevel, DeadLevel, RegDate, RenameCnt, RemainTime, LoginStatus
        )
        VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
            1, 10, 0, 10, 10, 10, 10, 10, 10, ?17, ?18, ?19,
            ?13, 0, 0, 0, ?14, ?15, ?16, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, datetime('now'), 0, 0, 0
        )
    "#;

    let server_id = i32::from(SERVER_ID);
    let account_id_i32 = account_id as i32;
    let slot = i32::from(character.slot);
    let race = i32::from(character.race);
    let mode = i32::from(character.mode);
    let hair = i32::from(character.hair);
    let face = i32::from(character.face);
    let height = i32::from(character.height);
    let character_class = i32::from(character.character_class);
    let gender = i32::from(character.gender);
    let map_id = i32::from(character.map_id);
    let pos_x = f64::from(character.pos_x);
    let pos_y = f64::from(character.pos_y);
    let pos_z = f64::from(character.pos_z);
    let start_hp = start_hp as i32;
    let start_mp = start_mp as i32;
    let start_sp = start_sp as i32;
    let character_id = insert_and_last_row_id(
        conn,
        "PS_GameData",
        sql,
        &[
            &server_id,
            &account.username.as_str(),
            &account_id_i32,
            &character.name.as_str(),
            &slot,
            &race,
            &mode,
            &hair,
            &face,
            &height,
            &character_class,
            &gender,
            &map_id,
            &pos_x,
            &pos_y,
            &pos_z,
            &start_hp,
            &start_mp,
            &start_sp,
        ],
    )
    .context("failed to create character")?;

    let character_id = character_id.max(0) as u32;
    get_character_by_id(conn, account_id, character_id)?.context("created character not found")
}

pub fn save_character_position(
    conn: &mut Db,
    character_id: u32,
    map_id: u16,
    x: f32,
    y: f32,
    z: f32,
) -> Result<()> {
    let sql = r#"
        UPDATE Chars
        SET Map = ?2, PosX = ?3, PosY = ?4, Posz = ?5
        WHERE CharID = ?1
    "#;

    let character_id = character_id as i32;
    let map_id = i32::from(map_id);
    let x = f64::from(x);
    let y = f64::from(y);
    let z = f64::from(z);
    execute(
        conn,
        "PS_GameData",
        sql,
        &[&character_id, &map_id, &x, &y, &z],
    )
    .context("failed to save character position")
}

fn account_from_row(row: &rusqlite::Row<'_>) -> DbAccount {
    DbAccount {
        id: row.get::<_, i64>("UserUID").unwrap_or_default() as u32,
        username: row
            .get::<_, String>("UserID")
            .unwrap_or_default()
            .trim_end()
            .to_string(),
        faction: tinyint_or_default(row, "Country", FACTION_NONE),
        max_grow: tinyint_or_default(row, "MaxGrow", MAX_GROW_ULTIMATE),
        is_admin: bit_or_default(row, "Admin", false)
            || tinyint_or_default(row, "AdminLevel", 0) > 0,
        admin_level: tinyint_or_default(row, "AdminLevel", 0),
        point: row.get::<_, i64>("Point").unwrap_or_default().max(0) as u32,
    }
}

pub fn set_account_points(conn: &mut Db, account_id: u32, points: u32) -> Result<u32> {
    let account_id_param = account_id as i32;
    let points_param = points.min(i32::MAX as u32) as i32;
    execute(
        conn,
        "PS_UserData",
        "UPDATE Users_Master SET Point = ?1 WHERE UserUID = ?2",
        &[&points_param, &account_id_param],
    )?;
    Ok(points_param as u32)
}

/// Item Mall / cash shop catalog entry (`PS_GameDefs.dbo.ProductList`).
/// `items` is derived from the table's 24 `ItemIDn`/`ItemCountn` column
/// pairs (skipping empty ones), converting each `ItemIDn` the same way
/// `create_character_item` does going the other way (`item_id = type*1000 +
/// type_id`).
#[derive(Clone, Debug)]
pub struct DbProduct {
    pub product_code: String,
    pub buy_cost: u32,
    pub items: Vec<(u8, u8, u8)>,
}

/// One gift sitting in an account's item-mall gift box
/// (PS_Billing.dbo.Users_Product3). `slot` is the DB `Slot` (1..=240); the
/// wire packets use `npcSlot = slot - 1`. `ItemID` decodes with the same
/// `Type * 1000 + TypeID` convention as ProductList.
#[derive(Clone, Copy, Debug)]
pub struct DbGiftBoxEntry {
    pub slot: u8,
    pub item_type: u8,
    pub type_id: u8,
    pub count: u8,
}

/// Loads an account's entire gift box, ordered by slot. Keyed by UserUID:
/// gifts are addressed to a *character* name at send time, but stored (and
/// claimable) per that character's account.
pub fn get_gift_box(conn: &mut Db, account_id: u32) -> Result<Vec<DbGiftBoxEntry>> {
    let sql = r#"
        SELECT Slot, ItemID, ItemCount
        FROM Users_Product3
        WHERE UserUID = ?1
        ORDER BY Slot
    "#;

    let account_id_param = account_id as i32;
    let rows = query_many(conn, "PS_Billing", sql, &[&account_id_param], |row| {
        (
            flexible_u32(row, "Slot", 0),
            flexible_u32(row, "ItemID", 0),
            flexible_u32(row, "ItemCount", 0),
        )
    })?;

    Ok(rows
        .into_iter()
        .filter_map(|(slot, item_id, count)| {
            if slot == 0 || slot > 240 || item_id == 0 || count == 0 {
                return None;
            }
            Some(DbGiftBoxEntry {
                slot: slot as u8,
                item_type: (item_id / 1000).min(u32::from(u8::MAX)) as u8,
                type_id: (item_id % 1000).min(u32::from(u8::MAX)) as u8,
                count: count.min(u32::from(u8::MAX)) as u8,
            })
        })
        .collect())
}

/// Stores one gift into `target_account_id`'s gift box at `slot` (1..=240).
/// `BuyDate` is stamped server-side; `RowID` is the table's identity.
pub fn insert_gift(
    conn: &mut Db,
    target_account_id: u32,
    slot: u8,
    item_type: u8,
    type_id: u8,
    count: u8,
    product_code: &str,
) -> Result<()> {
    let account_id_param = target_account_id as i32;
    let slot_param = i32::from(slot);
    let item_id_param = i32::from(item_type) * 1000 + i32::from(type_id);
    let count_param = i32::from(count);
    execute(
        conn,
        "PS_Billing",
        "INSERT INTO Users_Product3 (UserUID, Slot, ItemID, ItemCount, ProductCode, BuyDate) \
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
        &[&account_id_param, &slot_param, &item_id_param, &count_param, &product_code],
    )
    .context("failed to insert gift")?;
    Ok(())
}

/// Permanently removes the gift at `slot` from `account_id`'s box. Returns
/// whether a row was actually removed -- the guard that makes a double-claim
/// of the same slot impossible (the second delete matches nothing). A removed
/// gift can never be put back, matching the retail behavior.
pub fn delete_gift(conn: &mut Db, account_id: u32, slot: u8) -> Result<bool> {
    let account_id_param = account_id as i32;
    let slot_param = i32::from(slot);
    let rows = execute_rows(
        conn,
        "PS_Billing",
        "DELETE FROM Users_Product3 WHERE UserUID = ?1 AND Slot = ?2",
        &[&account_id_param, &slot_param],
    )
    .context("failed to delete gift")?;
    Ok(rows > 0)
}

pub fn get_product_by_code(conn: &mut Db, product_code: &str) -> Result<Option<DbProduct>> {
    let item_columns: String = (1..=24)
        .map(|i| format!("ItemID{i}, ItemCount{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT ProductCode, BuyCost, {item_columns} FROM ProductList WHERE ProductCode = ?1 LIMIT 1"
    );

    let row = query_one(conn, "PS_GameDefs", &sql, &[&product_code], |row| {
        let mut items = Vec::new();
        for i in 1..=24 {
            let item_id: i64 = row.get(format!("ItemID{i}").as_str()).unwrap_or(0);
            let count = tinyint_or_default(row, format!("ItemCount{i}").as_str(), 0);
            if item_id > 0 && count > 0 {
                let item_type = (item_id / 1000).clamp(0, i64::from(u8::MAX)) as u8;
                let type_id = (item_id % 1000).clamp(0, i64::from(u8::MAX)) as u8;
                items.push((item_type, type_id, count));
            }
        }

        DbProduct {
            product_code: row
                .get::<_, String>("ProductCode")
                .unwrap_or_default()
                .trim_end()
                .to_string(),
            buy_cost: row.get::<_, i64>("BuyCost").unwrap_or_default().max(0) as u32,
            items,
        }
    })?;
    Ok(row)
}

/// Single source of truth for every column row_to_character() reads.
/// get_characters_for_account and character_select_sql both build their
/// query from this instead of maintaining their own column list --
/// row_to_character silently falls back to defaults if a column it expects
/// isn't in the result set at all, so any two queries that feed it should
/// select exactly the same columns. This has already broken three times
/// (StatPoint/SkillPoint, then Money) from the two queries drifting apart;
/// this constant is what stops a fourth time.
const CHARACTER_COLUMNS: &str = r#"
    CharID, UserUID, Slot, CharName, Level, Map, PosX, PosY, Posz,
    Family, Grow, Hair, Face, Size, Job, Sex,
    Str, Rec, Int, Wis, Dex, Luc, StatPoint, SkillPoint,
    HP, MP, SP, Exp, Money, K1, K2, K3, K4, RenameCnt
"#;

fn character_select_sql() -> String {
    format!(
        r#"
        SELECT
            {CHARACTER_COLUMNS}
        FROM Chars
    "#
    )
}

fn row_to_character(row: &rusqlite::Row<'_>) -> DbCharacter {
    DbCharacter {
        id: row.get::<_, i64>("CharID").unwrap_or_default() as u32,
        account_id: row.get::<_, i64>("UserUID").unwrap_or_default() as u32,
        slot: tinyint_or_default(row, "Slot", 0),
        name: row
            .get::<_, String>("CharName")
            .unwrap_or_default()
            .trim_end()
            .to_string(),
        level: positive_smallint_as_u16(row, "Level", 1),
        map_id: positive_smallint_as_u16(row, "Map", 1),
        pos_x: row.get::<_, f64>("PosX").unwrap_or(500.0) as f32,
        pos_y: row.get::<_, f64>("PosY").unwrap_or(50.0) as f32,
        pos_z: row.get::<_, f64>("Posz").unwrap_or(500.0) as f32,
        race: tinyint_or_default(row, "Family", 0),
        mode: tinyint_or_default(row, "Grow", 0),
        hair: tinyint_or_default(row, "Hair", 0),
        face: tinyint_or_default(row, "Face", 0),
        height: tinyint_or_default(row, "Size", 0),
        character_class: tinyint_or_default(row, "Job", 0),
        gender: tinyint_or_default(row, "Sex", 0),
        strength: positive_smallint_as_u16(row, "Str", DEFAULT_STAT),
        reaction: positive_smallint_as_u16(row, "Rec", DEFAULT_STAT),
        intelligence: positive_smallint_as_u16(row, "Int", DEFAULT_STAT),
        wisdom: positive_smallint_as_u16(row, "Wis", DEFAULT_STAT),
        dexterity: positive_smallint_as_u16(row, "Dex", DEFAULT_STAT),
        luck: positive_smallint_as_u16(row, "Luc", DEFAULT_STAT),
        stat_point: positive_smallint_as_u16(row, "StatPoint", 0),
        skill_point: positive_smallint_as_u16(row, "SkillPoint", 0),
        // Chars.HP/MP/SP hold a per-character HP/MP/SP value written at
        // create/level-up (now a hardcoded flat 100/level, see
        // character::base_pool_for_level -- NOT StatusDefs). Read into both
        // max and current: the authoritative base MAX is recomputed from
        // level in CharacterStats::from_character, and current uses this as
        // its base then clamps to that max, so every login/select starts at
        // full. There is no separate "current" column in the retail schema.
        max_hp: numeric_u32(row, "HP", DEFAULT_HP),
        max_mp: numeric_u32(row, "MP", DEFAULT_MP),
        max_sp: numeric_u32(row, "SP", DEFAULT_SP),
        current_hp: numeric_u32(row, "HP", DEFAULT_HP),
        current_mp: numeric_u32(row, "MP", DEFAULT_MP),
        current_sp: numeric_u32(row, "SP", DEFAULT_SP),
        exp: row.get::<_, i64>("Exp").unwrap_or_default().max(0) as u32,
        // Money is stored in a signed 32-bit SQL column but treated as an
        // unsigned quantity up to MONEY_CAP, so it's read back via a bit
        // reinterpretation cast rather than clamped -- clamping negative
        // i32 bit patterns to 0 would zero out any balance above i32::MAX.
        money: row.get::<_, i64>("Money").unwrap_or_default() as i32 as u32,
        kills: row.get::<_, i64>("K1").unwrap_or_default().max(0) as u32,
        deaths: row.get::<_, i64>("K2").unwrap_or_default().max(0) as u32,
        victories: row.get::<_, i64>("K3").unwrap_or_default().max(0) as u32,
        defeats: row.get::<_, i64>("K4").unwrap_or_default().max(0) as u32,
        // Set true only by get_characters_for_account (which selects Del);
        // CHARACTER_COLUMNS itself doesn't include it, so it defaults false
        // for every playable-character query.
        pending_delete: false,
        // RenameCnt IS in CHARACTER_COLUMNS, so this is read for every
        // character query -- a nonzero count means a rename is pending.
        rename_pending: flexible_u32(row, "RenameCnt", 0) > 0,
    }
}

fn tinyint_or_default(row: &rusqlite::Row<'_>, column: &str, default: u8) -> u8 {
    row.get::<_, i64>(column)
        .map(|value| value.clamp(0, i64::from(u8::MAX)) as u8)
        .unwrap_or(default)
}

fn bit_or_default(row: &rusqlite::Row<'_>, column: &str, default: bool) -> bool {
    row.get::<_, i64>(column)
        .map(|value| value != 0)
        .unwrap_or(default)
}

fn positive_smallint_as_u16(row: &rusqlite::Row<'_>, column: &str, default: u16) -> u16 {
    row.get::<_, i64>(column)
        .map(|value| value.clamp(0, i64::from(u16::MAX)) as u16)
        .unwrap_or(default)
}

fn positive_smallint_as_u32(row: &rusqlite::Row<'_>, column: &str) -> u32 {
    row.get::<_, i64>(column)
        .map(|value| value.max(0) as u32)
        .unwrap_or_default()
}

fn numeric_u16(row: &rusqlite::Row<'_>, column: &str, default: u16) -> u16 {
    row.get::<_, i64>(column)
        .map(|value| value.clamp(0, i64::from(u16::MAX)) as u16)
        .unwrap_or(default)
}

fn numeric_u32(row: &rusqlite::Row<'_>, column: &str, default: u32) -> u32 {
    row.get::<_, i64>(column)
        .map(|value| value.max(0) as u32)
        .unwrap_or(default)
}

/// Reads an integer column, regardless of the value SQLite happens to have
/// stored for it (SQLite is dynamically typed, so a single i64 read covers
/// every integer width the old SQL Server schema used).
fn flexible_u32(row: &rusqlite::Row<'_>, column: &str, default: u32) -> u32 {
    row.get::<_, i64>(column)
        .map(|value| value.max(0) as u32)
        .unwrap_or(default)
}

/// Reads a numeric column as f32, trying a float read first and falling
/// back to an integer read (a column declared REAL that happens to hold a
/// whole number can still come back through either path depending on
/// how it was written).
fn flexible_f32(row: &rusqlite::Row<'_>, column: &str, default: f32) -> f32 {
    if let Ok(value) = row.get::<_, f64>(column) {
        return value as f32;
    }
    if let Ok(value) = row.get::<_, i64>(column) {
        return value.max(0) as f32;
    }
    default
}

fn is_magic_weapon(item_type: u8) -> bool {
    item_type == 12
}

impl DbEquipmentStats {
    fn add_item_row(&mut self, item_type: u8, row: &rusqlite::Row<'_>) {
        let max_effect = positive_smallint_as_u32(row, "Effect1");
        let min_effect = positive_smallint_as_u32(row, "Effect2");

        if is_magic_weapon(item_type) {
            self.min_magic = self.min_magic.saturating_add(min_effect);
            self.max_magic = self.max_magic.saturating_add(max_effect);
        } else {
            self.min_attack = self.min_attack.saturating_add(min_effect);
            self.max_attack = self.max_attack.saturating_add(max_effect);
        }

        self.defense = self
            .defense
            .saturating_add(positive_smallint_as_u32(row, "Effect3"));
        self.resistance = self
            .resistance
            .saturating_add(positive_smallint_as_u32(row, "Effect4"));
        self.max_hp = self
            .max_hp
            .saturating_add(positive_smallint_as_u32(row, "ConstHP"));
        self.max_sp = self
            .max_sp
            .saturating_add(positive_smallint_as_u32(row, "ConstSP"));
        self.max_mp = self
            .max_mp
            .saturating_add(positive_smallint_as_u32(row, "ConstMP"));
        self.strength = self
            .strength
            .saturating_add(positive_smallint_as_u32(row, "ConstStr"));
        self.dexterity = self
            .dexterity
            .saturating_add(positive_smallint_as_u32(row, "ConstDex"));
        self.reaction = self
            .reaction
            .saturating_add(positive_smallint_as_u32(row, "ConstRec"));
        self.intelligence = self
            .intelligence
            .saturating_add(positive_smallint_as_u32(row, "ConstInt"));
        self.wisdom = self
            .wisdom
            .saturating_add(positive_smallint_as_u32(row, "ConstWis"));
        self.luck = self
            .luck
            .saturating_add(positive_smallint_as_u32(row, "ConstLuc"));
    }
}
