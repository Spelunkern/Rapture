use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Recovers from a poisoned lock instead of panicking, so one thread's
/// panic while holding this lock can't cascade into every other thread
/// that needs the connection pool.
fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
use tiberius::{AuthMethod, Client, Config, Row, ToSql};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::sync::Semaphore;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

const DEFAULT_SQL_SERVER_HOST: &str = "localhost";
const DEFAULT_SQL_SERVER_PORT: u16 = 1433;
/// Default pool size; override with the RAPTURE_SQLSERVER_POOL_SIZE env var.
const POOL_MAX_CONNECTIONS: usize = 16;

type SqlClient = Client<Compat<TcpStream>>;
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
}

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
    pub delay: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct DbMobDefinition {
    pub mob_id: u16,
    pub max_hp: u32,
    pub money_min: u32,
    pub money_max: u32,
    pub respawn_seconds: Option<u64>,
    pub exp: u32,
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

/// A lightweight handle to the process-wide SQL Server connection pool.
///
/// Actual TCP/TDS connections are owned by the global `Pool`, not by this
/// struct, so `Db` is cheap to create per client session and never opens a
/// fresh connection per query.
pub struct Db;

pub fn connect() -> Result<Db> {
    // Touch the global runtime/pool eagerly so configuration errors (bad
    // host/port env vars) surface immediately instead of on first query.
    let _ = runtime();
    let _ = pool();
    Ok(Db)
}

struct Pool {
    host: String,
    port: u16,
    idle: Mutex<Vec<SqlClient>>,
    permits: Semaphore,
}

impl Pool {
    fn new() -> Self {
        let host = std::env::var("RAPTURE_SQLSERVER_HOST")
            .unwrap_or_else(|_| DEFAULT_SQL_SERVER_HOST.to_string());
        let port = std::env::var("RAPTURE_SQLSERVER_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(DEFAULT_SQL_SERVER_PORT);
        let max_connections = std::env::var("RAPTURE_SQLSERVER_POOL_SIZE")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|&value| value > 0)
            .unwrap_or(POOL_MAX_CONNECTIONS);

        Pool {
            host,
            port,
            idle: Mutex::new(Vec::new()),
            permits: Semaphore::new(max_connections),
        }
    }

    async fn open_connection(&self) -> Result<SqlClient> {
        let mut config = Config::new();
        config.host(&self.host);
        config.port(self.port);
        // Every query in this module fully qualifies tables as
        // `<Database>.dbo.<Table>`, so the initial catalog below is only a
        // connection default and never restricts which database is reached.
        config.database("PS_GameData");
        config.authentication(AuthMethod::Integrated);
        config.trust_cert();

        let tcp = TcpStream::connect(config.get_addr())
            .await
            .context("failed to open SQL Server TCP connection")?;
        tcp.set_nodelay(true)?;

        Client::connect(config, tcp.compat_write())
            .await
            .context("failed to connect to SQL Server")
    }

    async fn acquire(&'static self) -> Result<PooledConn> {
        let permit = self
            .permits
            .acquire()
            .await
            .context("SQL Server connection pool semaphore closed unexpectedly")?;
        permit.forget();

        let existing = lock_or_recover(&self.idle).pop();
        let client = match existing {
            Some(client) => client,
            None => match self.open_connection().await {
                Ok(client) => client,
                Err(error) => {
                    self.permits.add_permits(1);
                    return Err(error);
                }
            },
        };

        Ok(PooledConn {
            pool: self,
            client: Some(client),
            healthy: false,
        })
    }

    fn release(&'static self, client: SqlClient, healthy: bool) {
        if healthy {
            lock_or_recover(&self.idle).push(client);
        }
        self.permits.add_permits(1);
    }
}

struct PooledConn {
    pool: &'static Pool,
    client: Option<SqlClient>,
    healthy: bool,
}

impl PooledConn {
    fn client_mut(&mut self) -> &mut SqlClient {
        self.client
            .as_mut()
            .expect("pooled connection already released")
    }

    fn mark_healthy(&mut self) {
        self.healthy = true;
    }
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            self.pool.release(client, self.healthy);
        }
    }
}

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| Runtime::new().expect("failed to create SQL Server runtime"))
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(Pool::new)
}

/// Runs `f` against a pooled connection, returning the connection to the
/// pool only if `f` completed successfully. A connection that errored
/// (e.g. dropped by the server) is discarded so the next acquisition opens
/// a fresh one instead of reusing a possibly broken socket.
async fn with_pooled_client<T>(f: impl AsyncFnOnce(&mut SqlClient) -> Result<T>) -> Result<T> {
    let mut conn = pool().acquire().await?;
    let result = f(conn.client_mut()).await;
    if result.is_ok() {
        conn.mark_healthy();
    }
    result
}

pub fn init_schema(_conn: &mut Db) -> Result<()> {
    Ok(())
}

pub fn authenticate_account(
    conn: &mut Db,
    username: &str,
    password: &str,
) -> Result<Option<DbAccount>> {
    let sql = r#"
        SELECT TOP (1)
            u.UserUID,
            u.UserID,
            u.Admin,
            u.AdminLevel,
            ISNULL(g.Country, 2) AS Country,
            ISNULL(g.MaxGrow, 3) AS MaxGrow
        FROM PS_UserData.dbo.Users_Master AS u
        LEFT JOIN PS_GameData.dbo.UserMaxGrow AS g
            ON g.ServerID = @P1
            AND g.UserUID = u.UserUID
            AND g.Del = 0
        WHERE u.UserID = @P2
            AND u.Pw = @P3
            AND u.Leave = 0
    "#;

    let server_id = i32::from(SERVER_ID);
    let row = query_one(
        conn,
        "PS_UserData",
        sql,
        &[&server_id, &username, &password],
    )?;
    Ok(row.map(account_from_row))
}

pub fn get_account_by_id(conn: &mut Db, account_id: u32) -> Result<Option<DbAccount>> {
    let sql = r#"
        SELECT TOP (1)
            u.UserUID,
            u.UserID,
            u.Admin,
            u.AdminLevel,
            ISNULL(g.Country, 2) AS Country,
            ISNULL(g.MaxGrow, 3) AS MaxGrow
        FROM PS_UserData.dbo.Users_Master AS u
        LEFT JOIN PS_GameData.dbo.UserMaxGrow AS g
            ON g.ServerID = @P1
            AND g.UserUID = u.UserUID
            AND g.Del = 0
        WHERE u.UserUID = @P2
            AND u.Leave = 0
    "#;

    let server_id = i32::from(SERVER_ID);
    let account_id = account_id as i32;
    let row = query_one(conn, "PS_UserData", sql, &[&server_id, &account_id])?;
    Ok(row.map(account_from_row))
}

pub fn update_account_faction(conn: &mut Db, account_id: u32, faction: u8) -> Result<()> {
    let sql = r#"
        MERGE PS_GameData.dbo.UserMaxGrow WITH (HOLDLOCK) AS target
        USING (SELECT @P1 AS ServerID, @P2 AS UserUID) AS source
            ON target.ServerID = source.ServerID
            AND target.UserUID = source.UserUID
        WHEN MATCHED THEN
            UPDATE SET Country = @P3, MaxGrow = CASE WHEN target.MaxGrow < @P4 THEN @P4 ELSE target.MaxGrow END, Del = 0
        WHEN NOT MATCHED THEN
            INSERT (ServerID, UserUID, Country, MaxGrow, Del)
            VALUES (@P1, @P2, @P3, @P4, 0);
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
pub fn get_characters_for_account(conn: &mut Db, account_id: u32) -> Result<Vec<DbCharacter>> {
    let sql = format!(
        r#"
        SELECT
            {CHARACTER_COLUMNS}
        FROM PS_GameData.dbo.Chars
        WHERE UserUID = @P1 AND Del = 0
        ORDER BY Slot
    "#
    );
    let account_id_param = account_id as i32;
    let _ = conn;
    let rows = runtime().block_on(with_pooled_client(async |client: &mut SqlClient| {
        client
            .query(&sql, &[&account_id_param])
            .await
            .context("failed to load account characters")?
            .into_results()
            .await
            .context("failed to read account characters")
    }))?;

    Ok(rows
        .into_iter()
        .flatten()
        .map(|row| row_to_character(&row))
        .collect())
}

pub fn get_character_by_slot(
    conn: &mut Db,
    account_id: u32,
    slot: u8,
) -> Result<Option<DbCharacter>> {
    let sql = format!(
        "{} WHERE UserUID = @P1 AND Slot = @P2 AND Del = 0",
        character_select_sql()
    );
    let account_id = account_id as i32;
    let slot = i32::from(slot);
    let row = query_one(conn, "PS_GameData", &sql, &[&account_id, &slot])?;
    Ok(row.as_ref().map(row_to_character))
}

pub fn get_character_by_id(
    conn: &mut Db,
    account_id: u32,
    character_id: u32,
) -> Result<Option<DbCharacter>> {
    let sql = format!(
        "{} WHERE UserUID = @P1 AND CharID = @P2 AND Del = 0",
        character_select_sql()
    );
    let account_id = account_id as i32;
    let character_id = character_id as i32;
    let row = query_one(conn, "PS_GameData", &sql, &[&account_id, &character_id])?;
    Ok(row.as_ref().map(row_to_character))
}

/// Looks up a character by name regardless of which account owns it --
/// used by GM commands (set/amove/asummon/iclear/eclear) that target
/// another player by name.
pub fn get_character_by_name(conn: &mut Db, name: &str) -> Result<Option<DbCharacter>> {
    let sql = format!(
        "{} WHERE CharName = @P1 AND Del = 0",
        character_select_sql()
    );
    let row = query_one(conn, "PS_GameData", &sql, &[&name])?;
    Ok(row.as_ref().map(row_to_character))
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
    let sql = format!("UPDATE PS_GameData.dbo.Chars SET {column} = @P1 WHERE CharID = @P2");
    let character_id_param = character_id as i32;
    execute(conn, "PS_GameData", &sql, &[&clamped, &character_id_param])
        .context("failed to update character attribute")?;
    Ok(clamped)
}

/// Adds `amount` gold, clamped to MONEY_CAP. Money is stored in a signed
/// 32-bit SQL column but treated as unsigned up to MONEY_CAP, so this reads
/// the current value with a bit-reinterpretation cast (not a clamp-to-0),
/// does the add/clamp in Rust, then writes back the same way -- doing this
/// arithmetic in T-SQL directly isn't viable because assigning a bigint
/// above i32::MAX to an int column raises an overflow error instead of
/// wrapping, so the sign-bit trick has to happen on this side instead.
pub fn add_character_money(conn: &mut Db, character_id: u32, amount: u32) -> Result<u32> {
    let character_id_param = character_id as i32;
    let row = query_one(
        conn,
        "PS_GameData",
        "SELECT Money FROM PS_GameData.dbo.Chars WHERE CharID = @P1",
        &[&character_id_param],
    )?
    .context("character not found while adding money")?;
    let current = row.get::<i32, _>(0).unwrap_or_default() as u32;
    let new_money = current.saturating_add(amount).min(MONEY_CAP);
    let stored = new_money as i32;
    execute(
        conn,
        "PS_GameData",
        "UPDATE PS_GameData.dbo.Chars SET Money = @P1 WHERE CharID = @P2",
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
        "SELECT COUNT(*) AS Cnt FROM PS_GameData.dbo.CharItems WHERE CharID = @P1 AND Bag = 0 AND Del = 0 AND Count > 0"
    } else {
        "SELECT COUNT(*) AS Cnt FROM PS_GameData.dbo.CharItems WHERE CharID = @P1 AND Bag <> 0 AND Del = 0 AND Count > 0"
    };
    let character_id_param = character_id as i32;
    let row = query_one(conn, "PS_GameData", sql, &[&character_id_param])?;
    Ok(row
        .and_then(|row| row.get::<i32, _>("Cnt"))
        .unwrap_or(0)
        .max(0) as u32)
}

/// Soft-deletes every item in bag 0 (equipped, `equipped_only = true`) or
/// every item outside bag 0 (inventory, `equipped_only = false`) for a
/// character. Used by the GM `eclear`/`iclear` commands.
pub fn clear_character_items(conn: &mut Db, character_id: u32, equipped_only: bool) -> Result<()> {
    let sql = if equipped_only {
        r#"
            UPDATE PS_GameData.dbo.CharItems
            SET Count = 0, Del = 1
            WHERE CharID = @P1 AND Bag = 0 AND Del = 0
        "#
    } else {
        r#"
            UPDATE PS_GameData.dbo.CharItems
            SET Count = 0, Del = 1
            WHERE CharID = @P1 AND Bag <> 0 AND Del = 0
        "#
    };
    let character_id_param = character_id as i32;
    execute(conn, "PS_GameData", sql, &[&character_id_param])
        .context("failed to clear character items")
}

pub fn get_character_items(conn: &mut Db, character_id: u32) -> Result<Vec<DbCharacterItem>> {
    let sql = r#"
        SELECT
            RowID, Bag, Slot, Type, TypeID, Quality,
            Gem1, Gem2, Gem3, Gem4, Gem5, Gem6,
            Count, Craftname
        FROM PS_GameData.dbo.CharItems
        WHERE CharID = @P1
            AND Del = 0
            AND Count > 0
        ORDER BY Bag, Slot
    "#;

    let character_id = character_id as i32;
    let _ = conn;
    runtime().block_on(with_pooled_client(async |client: &mut SqlClient| {
        let rows = client
            .query(sql, &[&character_id])
            .await
            .context("failed to load character items")?
            .into_results()
            .await
            .context("failed to read character items")?;

        Ok(rows
            .into_iter()
            .flatten()
            .map(|row| DbCharacterItem {
                row_id: row.get::<i32, _>("RowID").unwrap_or_default().max(0) as u32,
                bag: tinyint_or_default(&row, "Bag", 0),
                slot: tinyint_or_default(&row, "Slot", 0),
                item_type: tinyint_or_default(&row, "Type", 0),
                type_id: tinyint_or_default(&row, "TypeID", 0),
                quality: positive_smallint_as_u16(&row, "Quality", 0),
                gems: [
                    tinyint_or_default(&row, "Gem1", 0),
                    tinyint_or_default(&row, "Gem2", 0),
                    tinyint_or_default(&row, "Gem3", 0),
                    tinyint_or_default(&row, "Gem4", 0),
                    tinyint_or_default(&row, "Gem5", 0),
                    tinyint_or_default(&row, "Gem6", 0),
                ],
                count: tinyint_or_default(&row, "Count", 1),
                craft_name: row
                    .get::<&str, _>("Craftname")
                    .unwrap_or_default()
                    .trim_end()
                    .to_string(),
                dyed: false,
            })
            .collect())
    }))
}

pub fn get_equipment_stats(conn: &mut Db, items: &[DbCharacterItem]) -> Result<DbEquipmentStats> {
    let equipped_items = items
        .iter()
        .filter(|item| item.bag == 0)
        .map(|item| (i32::from(item.item_type), i32::from(item.type_id)))
        .collect::<Vec<_>>();

    if equipped_items.is_empty() {
        return Ok(DbEquipmentStats::default());
    }

    let sql = r#"
        SELECT TOP (1)
            Effect1, Effect2, Effect3, Effect4,
            ConstHP, ConstSP, ConstMP,
            ConstStr, ConstDex, ConstRec, ConstInt, ConstWis, ConstLuc
        FROM PS_GameDefs.dbo.Items
        WHERE Type = @P1
            AND TypeID = @P2
    "#;

    let _ = conn;
    runtime().block_on(with_pooled_client(async |client: &mut SqlClient| {
        let mut stats = DbEquipmentStats::default();

        for (item_type, type_id) in equipped_items {
            let row = client
                .query(sql, &[&item_type, &type_id])
                .await
                .context("failed to load item definition")?
                .into_row()
                .await
                .context("failed to read item definition")?;

            if let Some(row) = row {
                stats.add_item_row(item_type as u8, &row);
            }
        }

        Ok(stats)
    }))
}

pub fn get_consumable_effect(
    conn: &mut Db,
    item_type: u8,
    type_id: u8,
) -> Result<Option<DbConsumableEffect>> {
    let sql = r#"
        SELECT TOP (1)
            Special, ConstHP, ConstSP, ConstMP, ReqIg
        FROM PS_GameDefs.dbo.Items
        WHERE Type = @P1
            AND TypeID = @P2
    "#;

    let item_type = i32::from(item_type);
    let type_id = i32::from(type_id);
    let row = query_one(conn, "PS_GameDefs", sql, &[&item_type, &type_id])?;
    Ok(row.map(|row| DbConsumableEffect {
        special: tinyint_or_default(&row, "Special", 0),
        hp: positive_smallint_as_u32(&row, "ConstHP"),
        sp: positive_smallint_as_u32(&row, "ConstSP"),
        mp: positive_smallint_as_u32(&row, "ConstMP"),
        req_ig: tinyint_or_default(&row, "ReqIg", 0),
    }))
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
        SELECT TOP (1)
            Type, TypeID, AttackTime, [Range]
        FROM PS_GameDefs.dbo.Items
        WHERE Type = @P1
            AND TypeID = @P2
    "#;

    let item_type_i32 = i32::from(item_type);
    let type_id_i32 = i32::from(type_id);
    let row = query_one(conn, "PS_GameDefs", sql, &[&item_type_i32, &type_id_i32])?;
    Ok(row.and_then(|row| {
        let attack_time = tinyint_or_default(&row, "AttackTime", 0).min(10);
        let (original_delay_ms, test_delay_ms) = weapon_attack_delay_ms(attack_time)?;
        let range = f32::from(numeric_u16(&row, "Range", 2));
        let range = if range <= 0.0 { 2.0 } else { range };

        Some(DbWeaponCombat {
            item_type: tinyint_or_default(&row, "Type", item_type),
            type_id: tinyint_or_default(&row, "TypeID", type_id),
            attack_time,
            range,
            original_delay_ms,
            test_delay_ms,
        })
    }))
}

pub fn get_item_definition(
    conn: &mut Db,
    item_type: u8,
    type_id: u8,
) -> Result<Option<DbItemDefinition>> {
    let sql = r#"
        SELECT TOP (1)
            Type, TypeID, Quality, Count
        FROM PS_GameDefs.dbo.Items
        WHERE Type = @P1
            AND TypeID = @P2
    "#;

    let item_type_i32 = i32::from(item_type);
    let type_id_i32 = i32::from(type_id);
    let row = query_one(conn, "PS_GameDefs", sql, &[&item_type_i32, &type_id_i32])?;
    Ok(row.map(|row| DbItemDefinition {
        item_type: tinyint_or_default(&row, "Type", item_type),
        type_id: tinyint_or_default(&row, "TypeID", type_id),
        quality: positive_smallint_as_u16(&row, "Quality", 0),
        max_stack: tinyint_or_default(&row, "Count", 1).max(1),
    }))
}

pub fn get_mob_drops(conn: &mut Db) -> Result<HashMap<u16, Vec<DbMobDrop>>> {
    let sql = r#"
        SELECT MobID, ItemOrder, Grade, DropRate
        FROM PS_GameDefs.dbo.MobItems
        WHERE Grade > 0
            AND DropRate > 0
        ORDER BY MobID, ItemOrder
    "#;

    let _ = conn;
    runtime().block_on(with_pooled_client(async |client: &mut SqlClient| {
        let rows = client
            .query(sql, &[])
            .await
            .context("failed to load mob drops")?
            .into_results()
            .await
            .context("failed to read mob drops")?;

        let mut drops = HashMap::<u16, Vec<DbMobDrop>>::new();
        for row in rows.into_iter().flatten() {
            let mob_id = numeric_u16(&row, "MobID", 0);
            let grade = numeric_u16(&row, "Grade", 0);
            let drop_rate = numeric_u32(&row, "DropRate", 0);
            if mob_id == 0 || grade == 0 || drop_rate == 0 {
                continue;
            }
            drops.entry(mob_id).or_default().push(DbMobDrop {
                mob_id,
                item_order: tinyint_or_default(&row, "ItemOrder", 0),
                grade,
                drop_rate,
            });
        }

        Ok(drops)
    }))
}

pub fn get_item_definitions_by_grade(conn: &mut Db) -> Result<HashMap<u16, Vec<DbItemDefinition>>> {
    let sql = r#"
        SELECT Type, TypeID, Quality, Count, Grade
        FROM PS_GameDefs.dbo.Items
        WHERE Grade > 0
            AND Type > 0
            AND TypeID > 0
        ORDER BY Grade, Type, TypeID
    "#;

    let _ = conn;
    runtime().block_on(with_pooled_client(async |client: &mut SqlClient| {
        let rows = client
            .query(sql, &[])
            .await
            .context("failed to load grade item definitions")?
            .into_results()
            .await
            .context("failed to read grade item definitions")?;

        let mut definitions = HashMap::<u16, Vec<DbItemDefinition>>::new();
        for row in rows.into_iter().flatten() {
            let grade = numeric_u16(&row, "Grade", 0);
            if grade == 0 {
                continue;
            }
            definitions
                .entry(grade)
                .or_default()
                .push(DbItemDefinition {
                    item_type: tinyint_or_default(&row, "Type", 0),
                    type_id: tinyint_or_default(&row, "TypeID", 0),
                    quality: positive_smallint_as_u16(&row, "Quality", 0),
                    max_stack: tinyint_or_default(&row, "Count", 1).max(1),
                });
        }

        Ok(definitions)
    }))
}

pub fn get_mob_definitions(conn: &mut Db) -> Result<HashMap<u16, DbMobDefinition>> {
    let sql = r#"
        SELECT MobID, HP, Money1, Money2, AttackSpecial3, Exp
        FROM PS_GameDefs.dbo.Mobs
    "#;

    let _ = conn;
    runtime().block_on(with_pooled_client(async |client: &mut SqlClient| {
        let rows = client
            .query(sql, &[])
            .await
            .context("failed to load mob definitions")?
            .into_results()
            .await
            .context("failed to read mob definitions")?;

        Ok(rows
            .into_iter()
            .flatten()
            .filter_map(|row| {
                let mob_id = numeric_u16(&row, "MobID", 0);
                if mob_id == 0 {
                    return None;
                }
                let max_hp = numeric_u32(&row, "HP", 100).max(1);
                let money_min = numeric_u32(&row, "Money1", 0);
                let money_max = numeric_u32(&row, "Money2", 0);
                let respawn_seconds =
                    mob_respawn_seconds(tinyint_or_default(&row, "AttackSpecial3", 0));
                let exp = numeric_u32(&row, "Exp", 0);
                Some((
                    mob_id,
                    DbMobDefinition {
                        mob_id,
                        max_hp,
                        money_min,
                        money_max,
                        respawn_seconds,
                        exp,
                    },
                ))
            })
            .collect())
    }))
}

/// Exp required to advance FROM `(Level, Grow)` to the next level, from
/// PS_GameDefs.dbo.ExpDefs. Keyed by the table's own 1-4 `Grow` value --
/// NOT the same range as `DbCharacter::mode`/`CharAttribute::Grow` (0-3):
/// callers must look up with `mode + 1`.
pub fn get_exp_definitions(conn: &mut Db) -> Result<HashMap<(u16, u8), u32>> {
    let sql = r#"
        SELECT Level, Grow, Exp
        FROM PS_GameDefs.dbo.ExpDefs
    "#;

    let _ = conn;
    runtime().block_on(with_pooled_client(async |client: &mut SqlClient| {
        let rows = client
            .query(sql, &[])
            .await
            .context("failed to load exp definitions")?
            .into_results()
            .await
            .context("failed to read exp definitions")?;

        Ok(rows
            .into_iter()
            .flatten()
            .map(|row| {
                let level = positive_smallint_as_u16(&row, "Level", 0);
                let grow = tinyint_or_default(&row, "Grow", 1);
                let exp = numeric_u32(&row, "Exp", 0);
                ((level, grow), exp)
            })
            .collect())
    }))
}

/// Base max HP/MP/SP per (Level, Job) from PS_GameDefs.dbo.StatusDefs --
/// `Job` matches `DbCharacter::character_class` directly (same column
/// name and range in both `Chars` and `StatusDefs`, confirmed against the
/// live DB, no offset needed unlike ExpDefs.Grow). These are absolute
/// values for that level, not deltas -- a level-up REPLACES the
/// character's base HP/MP/SP with the new level's row instead of adding to
/// it.
pub fn get_status_definitions(conn: &mut Db) -> Result<HashMap<(u16, u8), (u32, u32, u32)>> {
    let sql = r#"
        SELECT Level, Job, HP, MP, SP
        FROM PS_GameDefs.dbo.StatusDefs
    "#;

    let _ = conn;
    runtime().block_on(with_pooled_client(async |client: &mut SqlClient| {
        let rows = client
            .query(sql, &[])
            .await
            .context("failed to load status definitions")?
            .into_results()
            .await
            .context("failed to read status definitions")?;

        Ok(rows
            .into_iter()
            .flatten()
            .map(|row| {
                let level = positive_smallint_as_u16(&row, "Level", 0);
                let job = tinyint_or_default(&row, "Job", 0);
                let hp = numeric_u32(&row, "HP", 0);
                let mp = numeric_u32(&row, "MP", 0);
                let sp = numeric_u32(&row, "SP", 0);
                ((level, job), (hp, mp, sp))
            })
            .collect())
    }))
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
        "UPDATE PS_GameData.dbo.Chars SET HP = @P1, MP = @P2, SP = @P3 WHERE CharID = @P4",
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
        "UPDATE PS_GameData.dbo.Chars SET Level = @P1, Exp = @P2 WHERE CharID = @P3",
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
        INSERT INTO PS_GameData.dbo.CharItems (
            CharID, ItemID, ItemUID, Type, TypeID, Bag, Slot, Quality,
            Gem1, Gem2, Gem3, Gem4, Gem5, Gem6, Craftname, Count, Maketime, Maketype, Del
        )
        OUTPUT INSERTED.RowID
        VALUES (
            @P1, @P2,
            CAST(ABS(CHECKSUM(NEWID())) AS bigint) * 2147483648 + CAST(ABS(CHECKSUM(NEWID())) AS bigint),
            @P3, @P4, @P5, @P6, @P7,
            0, 0, 0, 0, 0, 0, '', @P8, GETDATE(), 'S', 0
        )
    "#;

    let character_id_i32 = character_id as i32;
    let item_id = i32::from(item.item_type) * 1000 + i32::from(item.type_id);
    let item_type = i32::from(item.item_type);
    let type_id = i32::from(item.type_id);
    let bag_i32 = i32::from(bag);
    let slot_i32 = i32::from(slot);
    let quality = i32::from(item.quality);
    let count_i32 = i32::from(count.max(1));
    let row = query_one(
        conn,
        "PS_GameData",
        sql,
        &[
            &character_id_i32,
            &item_id,
            &item_type,
            &type_id,
            &bag_i32,
            &slot_i32,
            &quality,
            &count_i32,
        ],
    )?
    .context("character item was not created")?;

    Ok(DbCharacterItem {
        row_id: row.get::<i32, _>(0).unwrap_or_default().max(0) as u32,
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
        INSERT INTO PS_GameData.dbo.CharItems (
            CharID, ItemID, ItemUID, Type, TypeID, Bag, Slot, Quality,
            Gem1, Gem2, Gem3, Gem4, Gem5, Gem6, Craftname, Count, Maketime, Maketype, Del
        )
        OUTPUT INSERTED.RowID
        VALUES (
            @P1, @P2,
            CAST(ABS(CHECKSUM(NEWID())) AS bigint) * 2147483648 + CAST(ABS(CHECKSUM(NEWID())) AS bigint),
            @P3, @P4, @P5, @P6, @P7,
            @P8, @P9, @P10, @P11, @P12, @P13, @P14, @P15, GETDATE(), 'S', 0
        )
    "#;

    let character_id_i32 = character_id as i32;
    let item_id = i32::from(item.item_type) * 1000 + i32::from(item.type_id);
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
    let row = query_one(
        conn,
        "PS_GameData",
        sql,
        &[
            &character_id_i32,
            &item_id,
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
    )?
    .context("character item was not created from template")?;

    let mut created = item.clone();
    created.row_id = row.get::<i32, _>(0).unwrap_or_default().max(0) as u32;
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
        UPDATE PS_GameData.dbo.CharItems
        SET Bag = @P3, Slot = @P4
        WHERE CharID = @P1
            AND RowID = @P2
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
        UPDATE PS_GameData.dbo.CharItems
        SET Count = @P3
        WHERE CharID = @P1
            AND RowID = @P2
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
        UPDATE PS_GameData.dbo.CharItems
        SET Count = 0, Del = 1
        WHERE CharID = @P1
            AND RowID = @P2
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
            UPDATE PS_GameData.dbo.CharItems
            SET Count = 0, Del = 1
            WHERE CharID = @P1
                AND RowID = @P2
                AND Del = 0
        "#;

        let character_id = character_id as i32;
        let row_id = row_id as i32;
        return execute(conn, "PS_GameData", sql, &[&character_id, &row_id])
            .context("failed to remove consumed character item");
    }

    let sql = r#"
        UPDATE PS_GameData.dbo.CharItems
        SET Count = @P3
        WHERE CharID = @P1
            AND RowID = @P2
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
        UPDATE PS_GameData.dbo.CharItems
        SET
            Bag = CASE RowID
                WHEN @P2 THEN @P3
                WHEN @P5 THEN @P6
                ELSE Bag
            END,
            Slot = CASE RowID
                WHEN @P2 THEN @P4
                WHEN @P5 THEN @P7
                ELSE Slot
            END
        WHERE CharID = @P1
            AND RowID IN (@P2, @P5)
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

pub fn get_character_quick_slots(conn: &mut Db, character_id: u32) -> Result<Vec<DbQuickSlot>> {
    let sql = r#"
        SELECT QuickBar, QuickSlot, Bag, Number
        FROM PS_GameData.dbo.CharQuickSlots
        WHERE CharID = @P1
        ORDER BY QuickBar, QuickSlot
    "#;

    let character_id = character_id as i32;
    let _ = conn;
    runtime().block_on(with_pooled_client(async |client: &mut SqlClient| {
        let rows = client
            .query(sql, &[&character_id])
            .await
            .context("failed to load character quick slots")?
            .into_results()
            .await
            .context("failed to read character quick slots")?;

        Ok(rows
            .into_iter()
            .flatten()
            .map(|row| DbQuickSlot {
                quick_bar: tinyint_or_default(&row, "QuickBar", 0),
                quick_slot: tinyint_or_default(&row, "QuickSlot", 0),
                bag: tinyint_or_default(&row, "Bag", 0),
                number: positive_smallint_as_u16(&row, "Number", 0),
            })
            .collect())
    }))
}

pub fn save_character_quick_slots(
    conn: &mut Db,
    character_id: u32,
    quick_slots: &[DbQuickSlot],
) -> Result<()> {
    let character_id = character_id as i32;
    execute(
        conn,
        "PS_GameData",
        "DELETE FROM PS_GameData.dbo.CharQuickSlots WHERE CharID = @P1",
        &[&character_id],
    )
    .context("failed to clear character quick slots")?;

    for quick_slot in quick_slots.iter().take(128) {
        let sql = r#"
            INSERT INTO PS_GameData.dbo.CharQuickSlots (
                CharID, QuickBar, QuickSlot, Bag, Number
            )
            VALUES (@P1, @P2, @P3, @P4, @P5)
        "#;

        let quick_bar = i32::from(quick_slot.quick_bar);
        let quick_slot_index = i32::from(quick_slot.quick_slot);
        let bag = i32::from(quick_slot.bag);
        let number = i32::from(quick_slot.number);
        execute(
            conn,
            "PS_GameData",
            sql,
            &[&character_id, &quick_bar, &quick_slot_index, &bag, &number],
        )
        .context("failed to insert character quick slot")?;
    }

    Ok(())
}

pub fn get_character_skills(conn: &mut Db, character_id: u32) -> Result<Vec<DbCharacterSkill>> {
    let sql = r#"
        SELECT SkillID, SkillLevel, Number, Delay
        FROM PS_GameData.dbo.CharSkills
        WHERE CharID = @P1
            AND Del = 0
        ORDER BY Number
    "#;

    let character_id = character_id as i32;
    let _ = conn;
    runtime().block_on(with_pooled_client(async |client: &mut SqlClient| {
        let rows = client
            .query(sql, &[&character_id])
            .await
            .context("failed to load character skills")?
            .into_results()
            .await
            .context("failed to read character skills")?;

        Ok(rows
            .into_iter()
            .flatten()
            .map(|row| DbCharacterSkill {
                skill_id: positive_smallint_as_u16(&row, "SkillID", 0),
                skill_level: tinyint_or_default(&row, "SkillLevel", 1),
                number: tinyint_or_default(&row, "Number", 0),
                delay: row.get::<i16, _>("Delay").unwrap_or_default().max(0) as i32,
            })
            .collect())
    }))
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
            UPDATE PS_GameData.dbo.CharSkills
            SET SkillLevel = @P3, Delay = 0, Del = 0
            WHERE CharID = @P1
                AND SkillID = @P2
        "#;

        let character_id = character_id as i32;
        let skill_id = skill_id as i32;
        let skill_level = i32::from(skill_level);
        execute(
            conn,
            "PS_GameData",
            sql,
            &[&character_id, &skill_id, &skill_level],
        )
        .context("failed to update character skill")?;

        return Ok(DbCharacterSkill {
            skill_id: skill_id as u16,
            skill_level: skill_level as u8,
            number: existing.number,
            delay: 0,
        });
    }

    let next_number = next_character_skill_number(conn, character_id)?;
    let sql = r#"
        INSERT INTO PS_GameData.dbo.CharSkills (
            CharID, SkillID, SkillLevel, Number, Delay, CreateTime, Del
        )
        VALUES (@P1, @P2, @P3, @P4, 0, GETDATE(), 0)
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
    let sql = r#"
        SELECT CAST(ISNULL(MAX(Number), -1) AS int) AS MaxNumber
        FROM PS_GameData.dbo.CharSkills
        WHERE CharID = @P1
    "#;

    let character_id_param = character_id as i32;
    let row = query_one(conn, "PS_GameData", sql, &[&character_id_param])?;
    let max_number = row
        .and_then(|row| row.get::<i32, _>("MaxNumber"))
        .unwrap_or(-1);
    let next = max_number.saturating_add(1);
    if next > i32::from(u8::MAX) {
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
    // Real base HP/MP/SP for a fresh level-1 character of this class, from
    // PS_GameDefs.dbo.StatusDefs -- falls back to a flat 100 (the old
    // hardcoded value) only if that table has no row for this job, which
    // shouldn't happen in practice.
    let (start_hp, start_mp, start_sp) = query_one(
        conn,
        "PS_GameDefs",
        "SELECT TOP (1) HP, MP, SP FROM PS_GameDefs.dbo.StatusDefs WHERE Level = 1 AND Job = @P1",
        &[&i32::from(character.character_class)],
    )?
    .map(|row| {
        (
            numeric_u32(&row, "HP", 100),
            numeric_u32(&row, "MP", 100),
            numeric_u32(&row, "SP", 100),
        )
    })
    .unwrap_or((100, 100, 100));

    let sql = r#"
        INSERT INTO PS_GameData.dbo.Chars (
            ServerID, UserID, UserUID, CharName, Slot, Family, Grow, Hair, Face, Size, Job, Sex,
            Level, StatPoint, SkillPoint, Str, Dex, Rec, Int, Luc, Wis, HP, MP, SP,
            Map, Dir, Exp, Money, PosX, PosY, Posz, Hg, Vg, Cg, Og, Ig,
            K1, K2, K3, K4, KillLevel, DeadLevel, RegDate, RenameCnt, RemainTime, LoginStatus
        )
        OUTPUT INSERTED.CharID
        VALUES (
            @P1, @P2, @P3, @P4, @P5, @P6, @P7, @P8, @P9, @P10, @P11, @P12,
            1, 10, 0, 10, 10, 10, 10, 10, 10, @P17, @P18, @P19,
            @P13, 0, 0, 0, @P14, @P15, @P16, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, GETDATE(), 0, 0, 0
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
    let start_hp = start_hp as i32;
    let start_mp = start_mp as i32;
    let start_sp = start_sp as i32;
    let row = query_one(
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
            &character.pos_x,
            &character.pos_y,
            &character.pos_z,
            &start_hp,
            &start_mp,
            &start_sp,
        ],
    )?
    .context("character was not created")?;

    let character_id = row.get::<i32, _>(0).unwrap_or_default() as u32;
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
        UPDATE PS_GameData.dbo.Chars
        SET Map = @P2, PosX = @P3, PosY = @P4, Posz = @P5
        WHERE CharID = @P1
    "#;

    let character_id = character_id as i32;
    let map_id = i32::from(map_id);
    execute(
        conn,
        "PS_GameData",
        sql,
        &[&character_id, &map_id, &x, &y, &z],
    )
    .context("failed to save character position")
}

fn query_one(
    conn: &mut Db,
    database: &str,
    sql: &str,
    params: &[&dyn ToSql],
) -> Result<Option<Row>> {
    let _ = (conn, database);
    runtime().block_on(with_pooled_client(async |client: &mut SqlClient| {
        let row = client
            .query(sql, params)
            .await
            .context("failed to execute SQL query")?
            .into_row()
            .await
            .context("failed to read SQL row")?;
        Ok(row)
    }))
}

fn execute(conn: &mut Db, database: &str, sql: &str, params: &[&dyn ToSql]) -> Result<()> {
    let _ = (conn, database);
    runtime().block_on(with_pooled_client(async |client: &mut SqlClient| {
        client
            .execute(sql, params)
            .await
            .context("failed to execute SQL command")?;
        Ok(())
    }))
}

fn account_from_row(row: Row) -> DbAccount {
    DbAccount {
        id: row.get::<i32, _>("UserUID").unwrap_or_default() as u32,
        username: row
            .get::<&str, _>("UserID")
            .unwrap_or_default()
            .trim_end()
            .to_string(),
        faction: tinyint_or_default(&row, "Country", FACTION_NONE),
        max_grow: tinyint_or_default(&row, "MaxGrow", MAX_GROW_ULTIMATE),
        is_admin: bit_or_default(&row, "Admin", false)
            || tinyint_or_default(&row, "AdminLevel", 0) > 0,
        admin_level: tinyint_or_default(&row, "AdminLevel", 0),
    }
}

/// Single source of truth for every column row_to_character() reads.
/// get_characters_for_account and character_select_sql both build their
/// query from this instead of maintaining their own column list --
/// row_to_character silently panics (via tiberius's Row::get) if a column
/// it expects isn't in the result set at all, so any two queries that feed
/// it MUST select exactly the same columns. This has already broken three
/// times (StatPoint/SkillPoint, then Money) from the two queries drifting
/// apart; this constant is what stops a fourth time.
const CHARACTER_COLUMNS: &str = r#"
    CharID, UserUID, Slot, CharName, Level, Map, PosX, PosY, Posz,
    Family, Grow, Hair, Face, Size, Job, Sex,
    Str, Rec, Int, Wis, Dex, Luc, StatPoint, SkillPoint,
    HP, MP, SP, Exp, Money, K1, K2, K3, K4
"#;

fn character_select_sql() -> String {
    format!(
        r#"
        SELECT TOP (1)
            {CHARACTER_COLUMNS}
        FROM PS_GameData.dbo.Chars
    "#
    )
}

fn row_to_character(row: &Row) -> DbCharacter {
    DbCharacter {
        id: row.get::<i32, _>("CharID").unwrap_or_default() as u32,
        account_id: row.get::<i32, _>("UserUID").unwrap_or_default() as u32,
        slot: tinyint_or_default(row, "Slot", 0),
        name: row
            .get::<&str, _>("CharName")
            .unwrap_or_default()
            .trim_end()
            .to_string(),
        level: positive_smallint_as_u16(row, "Level", 1),
        map_id: positive_smallint_as_u16(row, "Map", 1),
        pos_x: row.get::<f32, _>("PosX").unwrap_or(500.0),
        pos_y: row.get::<f32, _>("PosY").unwrap_or(50.0),
        pos_z: row.get::<f32, _>("Posz").unwrap_or(500.0),
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
        // Chars.HP/MP/SP store the character's base max (matching
        // PS_GameDefs.dbo.StatusDefs at their Level/Job -- see
        // db::get_status_definitions), not a separately-tracked "current"
        // value: there is no other HP/MP/SP column in the retail schema,
        // so every login/character-select starts at full, exactly like
        // this project already assumed when this used to hardcode
        // DEFAULT_HP/MP/SP for both fields (now reads the real per-
        // character value instead of that flat placeholder).
        max_hp: numeric_u32(row, "HP", DEFAULT_HP),
        max_mp: numeric_u32(row, "MP", DEFAULT_MP),
        max_sp: numeric_u32(row, "SP", DEFAULT_SP),
        current_hp: numeric_u32(row, "HP", DEFAULT_HP),
        current_mp: numeric_u32(row, "MP", DEFAULT_MP),
        current_sp: numeric_u32(row, "SP", DEFAULT_SP),
        exp: row.get::<i32, _>("Exp").unwrap_or_default().max(0) as u32,
        // Money is stored in a signed 32-bit SQL column but treated as an
        // unsigned quantity up to MONEY_CAP, so it's read back via a bit
        // reinterpretation cast rather than clamped -- clamping negative
        // i32 bit patterns to 0 would zero out any balance above i32::MAX.
        money: row.get::<i32, _>("Money").unwrap_or_default() as u32,
        kills: row.get::<i32, _>("K1").unwrap_or_default().max(0) as u32,
        deaths: row.get::<i32, _>("K2").unwrap_or_default().max(0) as u32,
        victories: row.get::<i32, _>("K3").unwrap_or_default().max(0) as u32,
        defeats: row.get::<i32, _>("K4").unwrap_or_default().max(0) as u32,
    }
}

fn tinyint_or_default(row: &Row, column: &str, default: u8) -> u8 {
    row.get::<u8, _>(column).unwrap_or(default)
}

fn bit_or_default(row: &Row, column: &str, default: bool) -> bool {
    row.get::<bool, _>(column).unwrap_or(default)
}

fn positive_smallint_as_u16(row: &Row, column: &str, default: u16) -> u16 {
    row.get::<i16, _>(column)
        .map(|value| value.max(0) as u16)
        .unwrap_or(default)
}

fn positive_smallint_as_u32(row: &Row, column: &str) -> u32 {
    row.get::<i16, _>(column)
        .map(|value| value.max(0) as u32)
        .unwrap_or_default()
}

fn numeric_u16(row: &Row, column: &str, default: u16) -> u16 {
    if let Some(value) = row.try_get::<i16, _>(column).ok().flatten() {
        return value.max(0) as u16;
    }
    if let Some(value) = row.try_get::<i32, _>(column).ok().flatten() {
        return value.max(0).min(i32::from(u16::MAX)) as u16;
    }
    default
}

fn numeric_u32(row: &Row, column: &str, default: u32) -> u32 {
    if let Some(value) = row.try_get::<i32, _>(column).ok().flatten() {
        return value.max(0) as u32;
    }
    if let Some(value) = row.try_get::<i16, _>(column).ok().flatten() {
        return value.max(0) as u32;
    }
    default
}

fn is_magic_weapon(item_type: u8) -> bool {
    item_type == 12
}

impl DbEquipmentStats {
    fn add_item_row(&mut self, item_type: u8, row: &Row) {
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
