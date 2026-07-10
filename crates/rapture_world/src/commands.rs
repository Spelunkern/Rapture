use anyhow::Result;
use rapture_common::crypto::AesCtr;
use rapture_common::db::{self, CharAttribute, DbCharacter, DbCharacterItem, DbItemDefinition};
use rapture_common::packet::{Opcode, PacketWriter};
use std::net::TcpStream;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::character::*;
use crate::chat::*;
use crate::inventory::*;
use crate::movement::*;
use crate::session::*;
use crate::svmap::*;
use crate::world_entities::*;

/// GM commands are matched as bare words -- the client strips a leading
/// `/` before the message ever reaches us (confirmed in rapture_world.log),
/// so the GM still types `/getitem` etc. as the UI convention, but the
/// parser here never sees that slash.
pub(crate) fn parse_getitem_command(message: &str) -> Option<Result<(u8, u8, u32)>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("getitem") {
        return None;
    }

    let Some(item_type) = parts.next() else {
        return Some(Err(anyhow::anyhow!(
            "Usage: /getitem <type> <typeId> [count]"
        )));
    };
    let Some(type_id) = parts.next() else {
        return Some(Err(anyhow::anyhow!(
            "Usage: /getitem <type> <typeId> [count]"
        )));
    };
    // Count is optional -- defaults to 1 if omitted.
    let count = parts.next();
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!(
            "Usage: /getitem <type> <typeId> [count]"
        )));
    }

    let item_type = item_type.parse::<u8>();
    let type_id = type_id.parse::<u8>();
    let count = count.map_or(Ok(1), |count| count.parse::<u32>());
    match (item_type, type_id, count) {
        (Ok(item_type), Ok(type_id), Ok(count)) if count > 0 => {
            Some(Ok((item_type, type_id, count)))
        }
        _ => Some(Err(anyhow::anyhow!(
            "Usage: /getitem <type> <typeId> [count]"
        ))),
    }
}

/// Where a GM move command should land: either an exact spot the caller
/// asked for, or "figure out somewhere sensible" (empty/incomplete/invalid
/// coordinates on /cmove fall back to this instead of failing outright).
#[derive(Clone, Copy)]
pub(crate) enum MoveTarget {
    Exact(f32, f32, f32),
    Fallback,
}

/// `/cmove <mapId> [x y z]`. Coordinates are optional; if absent,
/// incomplete (not exactly 3 tokens), or fail to parse as numbers, this
/// resolves to `MoveTarget::Fallback` rather than an error -- validity
/// against the destination map (bounds/walkability) is checked later by
/// resolve_move_position, since that requires map data this parser doesn't
/// have.
pub(crate) fn parse_cmove_command(message: &str) -> Option<Result<(u16, MoveTarget)>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("cmove") {
        return None;
    }

    let Some(map_id) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: /cmove <mapId> [x y z]")));
    };
    let Ok(map_id) = map_id.parse::<u16>() else {
        return Some(Err(anyhow::anyhow!("Usage: /cmove <mapId> [x y z]")));
    };
    if map_id == 0 {
        return Some(Err(anyhow::anyhow!("Usage: /cmove <mapId> [x y z]")));
    }

    let rest: Vec<&str> = parts.collect();
    let target = if rest.len() == 3 {
        match (
            rest[0].parse::<f32>(),
            rest[1].parse::<f32>(),
            rest[2].parse::<f32>(),
        ) {
            (Ok(x), Ok(y), Ok(z)) => MoveTarget::Exact(x, y, z),
            _ => MoveTarget::Fallback,
        }
    } else {
        MoveTarget::Fallback
    };

    Some(Ok((map_id, target)))
}

/// `/bmove <x> <y> <z>`. Same-map teleport; unlike /cmove the coordinates
/// are mandatory input (this command has no other purpose without them),
/// but an out-of-bounds/unwalkable target still falls back gracefully
/// (via resolve_move_position) instead of being rejected.
pub(crate) fn parse_bmove_command(message: &str) -> Option<Result<(f32, f32, f32)>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("bmove") {
        return None;
    }

    let Some(x) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: /bmove <x> <y> <z>")));
    };
    let Some(y) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: /bmove <x> <y> <z>")));
    };
    let Some(z) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: /bmove <x> <y> <z>")));
    };
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!("Usage: /bmove <x> <y> <z>")));
    }

    match (x.parse::<f32>(), y.parse::<f32>(), z.parse::<f32>()) {
        (Ok(x), Ok(y), Ok(z)) => Some(Ok((x, y, z))),
        _ => Some(Err(anyhow::anyhow!("Usage: /bmove <x> <y> <z>"))),
    }
}

pub(crate) struct MoveResolution {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) z: f32,
    pub(crate) used_fallback: bool,
}

/// Resolves a GM move command's actual destination, applying the fallback
/// chain: exact coordinates if given and walkable -> nearest/any spawn area
/// for the traveler's faction -> first walkable tile on the map's mask ->
/// (as an absolute last resort, if the map has no mask to validate against
/// at all) whatever was requested, or the reference position.
///
/// `nearest_to` is `Some((x, z))` for same-map moves (/bmove), where the
/// traveler's current position is a meaningful reference for "nearest"
/// spawn area; `None` for cross-map moves (/cmove), where it isn't.
pub(crate) fn resolve_move_position(
    map_id: u16,
    target: MoveTarget,
    faction: u8,
    nearest_to: Option<(f32, f32)>,
    fallback_y: f32,
) -> MoveResolution {
    if let MoveTarget::Exact(x, y, z) = target {
        if is_map_walkable(map_id, x, z) {
            return MoveResolution {
                x,
                y,
                z,
                used_fallback: false,
            };
        }
    }

    let spawn_area = match nearest_to {
        Some((x, z)) => find_nearest_spawn_area(map_id, faction, x, z),
        None => find_any_spawn_area(map_id, faction),
    };
    if let Some(spawn_area) = spawn_area {
        let (x, y, z) = spawn_area.center();
        return MoveResolution {
            x,
            y,
            z,
            used_fallback: true,
        };
    }

    if let Some((x, z)) = find_any_walkable_position(map_id) {
        return MoveResolution {
            x,
            y: fallback_y,
            z,
            used_fallback: true,
        };
    }

    // No mask registered for this map either -- nothing to validate
    // against, so trust the request if there was one, else fall back to
    // the reference position rather than leaving the traveler nowhere.
    let (x, z) = nearest_to.unwrap_or((0.0, 0.0));
    match target {
        MoveTarget::Exact(x, y, z) => MoveResolution {
            x,
            y,
            z,
            used_fallback: false,
        },
        MoveTarget::Fallback => MoveResolution {
            x,
            y: fallback_y,
            z,
            used_fallback: true,
        },
    }
}

pub(crate) fn parse_town_command(message: &str) -> bool {
    let mut parts = message.split_whitespace();
    let Some(command) = parts.next() else {
        return false;
    };
    if !command.eq_ignore_ascii_case("/town") && !command.eq_ignore_ascii_case("town") {
        return false;
    }

    parts.next().is_none()
}

/// Base radius (meters) when the optional `area` argument is omitted on
/// mob-related commands; each unit of `area` adds one more meter.
const MOB_SPAWN_BASE_RADIUS: f32 = 25.0;
const MOB_CLEAR_BASE_RADIUS: f32 = 25.0;
/// Same idea for NPC removal, but NPCs are far sparser/more deliberately
/// placed than mobs, so the base radius is much tighter.
const NPC_CLEAR_BASE_RADIUS: f32 = 5.0;

/// `mmake <mobId> <count> [area]`. Spawns `count` mobs of `mobId` scattered
/// randomly within (25 + area) meters of the GM's current position. `count`
/// is a u8 so it's naturally capped at 255 by the parse itself.
pub(crate) fn parse_mmake_command(message: &str) -> Option<Result<(u16, u8, f32)>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("mmake") {
        return None;
    }

    let Some(mob_id) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: mmake <mobId> <count> [area]")));
    };
    let Some(count) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: mmake <mobId> <count> [area]")));
    };
    let area = parts.next();
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!("Usage: mmake <mobId> <count> [area]")));
    }

    let mob_id = mob_id.parse::<u16>();
    let count = count.parse::<u8>();
    let area = area.map_or(Ok(0.0), str::parse::<f32>);
    match (mob_id, count, area) {
        (Ok(mob_id), Ok(count), Ok(area)) if count > 0 && area >= 0.0 => {
            Some(Ok((mob_id, count, MOB_SPAWN_BASE_RADIUS + area)))
        }
        _ => Some(Err(anyhow::anyhow!("Usage: mmake <mobId> <count> [area]"))),
    }
}

/// `mera [area]`. Removes every mob (map-defined or GM-spawned) within
/// (25 + area) meters of the GM's current position.
pub(crate) fn parse_mera_command(message: &str) -> Option<Result<f32>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("mera") {
        return None;
    }
    let area = parts.next();
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!("Usage: mera [area]")));
    }
    match area.map_or(Ok(0.0), str::parse::<f32>) {
        Ok(area) if area >= 0.0 => Some(Ok(MOB_CLEAR_BASE_RADIUS + area)),
        _ => Some(Err(anyhow::anyhow!("Usage: mera [area]"))),
    }
}

/// `nmake <type> <typeId>`. Always spawns exactly one NPC at the GM's
/// current position (no count/area) -- since it never leaves that spot,
/// the GM's own Y is already the correct terrain height, no separate
/// terrain lookup needed.
pub(crate) fn parse_nmake_command(message: &str) -> Option<Result<(u8, u16)>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("nmake") {
        return None;
    }
    let Some(npc_type) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: nmake <type> <typeId>")));
    };
    let Some(type_id) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: nmake <type> <typeId>")));
    };
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!("Usage: nmake <type> <typeId>")));
    }
    match (npc_type.parse::<u8>(), type_id.parse::<u16>()) {
        (Ok(npc_type), Ok(type_id)) => Some(Ok((npc_type, type_id))),
        _ => Some(Err(anyhow::anyhow!("Usage: nmake <type> <typeId>"))),
    }
}

/// `nera [area]`. Removes every NPC within (5 + area) meters of the GM's
/// current position.
pub(crate) fn parse_nera_command(message: &str) -> Option<Result<f32>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("nera") {
        return None;
    }
    let area = parts.next();
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!("Usage: nera [area]")));
    }
    match area.map_or(Ok(0.0), str::parse::<f32>) {
        Ok(area) if area >= 0.0 => Some(Ok(NPC_CLEAR_BASE_RADIUS + area)),
        _ => Some(Err(anyhow::anyhow!("Usage: nera [area]"))),
    }
}

/// `mobreload <mapId>`. Clears every mob on the map immediately, then
/// respawns the .svmap-defined set after a 5s delay.
pub(crate) fn parse_mobreload_command(message: &str) -> Option<Result<u16>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("mobreload") {
        return None;
    }
    let Some(map_id) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: mobreload <mapId>")));
    };
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!("Usage: mobreload <mapId>")));
    }
    match map_id.parse::<u16>() {
        Ok(map_id) if map_id > 0 => Some(Ok(map_id)),
        _ => Some(Err(anyhow::anyhow!("Usage: mobreload <mapId>"))),
    }
}

/// `npcreload <mapId>`. Same idea as mobreload, for NPCs.
pub(crate) fn parse_npcreload_command(message: &str) -> Option<Result<u16>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("npcreload") {
        return None;
    }
    let Some(map_id) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: npcreload <mapId>")));
    };
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!("Usage: npcreload <mapId>")));
    }
    match map_id.parse::<u16>() {
        Ok(map_id) if map_id > 0 => Some(Ok(map_id)),
        _ => Some(Err(anyhow::anyhow!("Usage: npcreload <mapId>"))),
    }
}

pub(crate) fn parse_gm_getitem_command(payload: &[u8]) -> Result<(u8, u8, u32)> {
    if payload.len() < 5 {
        anyhow::bail!("GM getitem packet is too short");
    }

    let item_type = payload[2];
    let type_id = payload[3];
    let count = u32::from(payload[4]);
    if count == 0 {
        anyhow::bail!("GM getitem count must be greater than zero");
    }

    Ok((item_type, type_id, count))
}

pub(crate) struct GetItemCommandResult {
    pub(crate) add_item_packets: Vec<Vec<u8>>,
    pub(crate) message: String,
    pub(crate) created_total: u32,
}

pub(crate) fn create_getitem_stack(
    db_conn: &mut db::Db,
    character_id: u32,
    definition: &DbItemDefinition,
    bag: u8,
    slot: u8,
    count: u8,
) -> Result<DbCharacterItem> {
    db::create_character_item(db_conn, character_id, definition, bag, slot, count)
}

pub(crate) fn handle_getitem_request(
    db_conn: &mut db::Db,
    character: &DbCharacter,
    items: &mut Vec<DbCharacterItem>,
    item_type: u8,
    type_id: u8,
    requested_count: u32,
    logger: &Logger,
) -> Result<GetItemCommandResult> {
    let Some(definition) = db::get_item_definition(db_conn, item_type, type_id)? else {
        return Ok(GetItemCommandResult {
            add_item_packets: Vec::new(),
            message: format!("Item {item_type} {type_id} was not found."),
            created_total: 0,
        });
    };

    let mut remaining = requested_count;
    let mut created_total = 0u32;
    let mut add_item_packets = Vec::new();
    let max_stack = effective_stack_limit(&definition);

    while remaining > 0 {
        let Some(stack_index) = items.iter().position(|item| {
            item.bag != 0
                && item.item_type == definition.item_type
                && item.type_id == definition.type_id
                && item.quality == definition.quality
                && item.gems == [0; 6]
                && item.craft_name.is_empty()
                && item.count < max_stack
        }) else {
            break;
        };

        let free_space = max_stack.saturating_sub(items[stack_index].count);
        let moved_count = remaining.min(u32::from(free_space)) as u8;
        if moved_count == 0 {
            break;
        }

        let new_count = items[stack_index].count.saturating_add(moved_count);
        db::set_character_item_count(db_conn, character.id, items[stack_index].row_id, new_count)?;
        items[stack_index].count = new_count;
        add_item_packets.push(build_add_item(&items[stack_index])?);
        created_total = created_total.saturating_add(u32::from(moved_count));
        remaining = remaining.saturating_sub(u32::from(moved_count));
    }

    let free_slots = find_free_inventory_slots(items);
    if remaining > 0 && free_slots.is_empty() && add_item_packets.is_empty() {
        return Ok(GetItemCommandResult {
            add_item_packets: Vec::new(),
            message: "Inventory is full.".to_string(),
            created_total: 0,
        });
    }

    for (bag, slot) in free_slots {
        if remaining == 0 {
            break;
        }

        let stack_count = remaining.min(u32::from(max_stack));
        let created_item = create_getitem_stack(
            db_conn,
            character.id,
            &definition,
            bag,
            slot,
            stack_count as u8,
        )?;
        add_item_packets.push(build_add_item(&created_item)?);
        items.push(created_item);
        created_total = created_total.saturating_add(stack_count);
        remaining = remaining.saturating_sub(stack_count);
    }

    logger.line(format!(
        "Rapture World getitem character_id={} type={} type_id={} requested={} created={}",
        character.id, item_type, type_id, requested_count, created_total
    ));

    let message = if remaining == 0 {
        format!("Created {created_total} item(s): {item_type} {type_id}.")
    } else {
        format!(
            "Created {created_total} of {requested_count} item(s): {item_type} {type_id}. Inventory is full."
        )
    };

    Ok(GetItemCommandResult {
        add_item_packets,
        message,
        created_total,
    })
}

pub(crate) fn handle_getitem_command(
    db_conn: &mut db::Db,
    character: &DbCharacter,
    items: &mut Vec<DbCharacterItem>,
    message: &str,
    logger: &Logger,
) -> Result<Option<GetItemCommandResult>> {
    let Some(command) = parse_getitem_command(message) else {
        return Ok(None);
    };

    let (item_type, type_id, requested_count) = match command {
        Ok(command) => command,
        Err(error) => {
            return Ok(Some(GetItemCommandResult {
                add_item_packets: Vec::new(),
                message: error.to_string(),
                created_total: 0,
            }));
        }
    };

    Ok(Some(handle_getitem_request(
        db_conn,
        character,
        items,
        item_type,
        type_id,
        requested_count,
        logger,
    )?))
}

pub(crate) fn build_gm_command_success() -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::GmCommandError);
    packet.write_u16(0);
    packet.into_bytes()
}

pub(crate) fn build_gm_command_error(rejected_opcode: u16) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::GmCommandError);
    packet.write_u16(rejected_opcode);
    packet.into_bytes()
}

/// `char on` / `char off`. Returns the resulting *visible* state directly
/// (true = "char off", visible to non-admins too; false = "char on", back
/// to the GM's normal invisible state) so the caller doesn't have to
/// re-derive it from which word was typed.
pub(crate) fn parse_char_command(message: &str) -> Option<Result<bool>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("char") {
        return None;
    }
    let Some(state) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: char on|off")));
    };
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!("Usage: char on|off")));
    }
    if state.eq_ignore_ascii_case("off") {
        Some(Ok(true))
    } else if state.eq_ignore_ascii_case("on") {
        Some(Ok(false))
    } else {
        Some(Err(anyhow::anyhow!("Usage: char on|off")))
    }
}

fn parse_char_attribute(token: &str) -> Option<CharAttribute> {
    Some(match token.to_ascii_lowercase().as_str() {
        "level" => CharAttribute::Level,
        "grow" => CharAttribute::Grow,
        "money" => CharAttribute::Money,
        "statpoint" => CharAttribute::StatPoint,
        "skillpoint" => CharAttribute::SkillPoint,
        "str" => CharAttribute::Str,
        "dex" => CharAttribute::Dex,
        "rec" => CharAttribute::Rec,
        "int" => CharAttribute::Int,
        "luc" => CharAttribute::Luc,
        "wis" => CharAttribute::Wis,
        "kills" => CharAttribute::Kills,
        "deaths" => CharAttribute::Deaths,
        _ => return None,
    })
}

const SET_USAGE: &str = "Usage: set [charname] <level|grow|money|statpoint|skillpoint|str|dex|rec|int|luc|wis|kills|deaths> <amount>";

/// `set [charname] <property> <amount>`. `charname` is optional -- if the
/// first argument after `set` isn't a recognized property name, it's
/// treated as a target character name and the property follows it;
/// otherwise the target is the caller themselves.
pub(crate) fn parse_set_command(
    message: &str,
) -> Option<Result<(Option<String>, CharAttribute, i64)>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("set") {
        return None;
    }

    let Some(first) = parts.next() else {
        return Some(Err(anyhow::anyhow!(SET_USAGE)));
    };

    let (charname, property_token) = match parse_char_attribute(first) {
        Some(_) => (None, first),
        None => {
            let Some(second) = parts.next() else {
                return Some(Err(anyhow::anyhow!(SET_USAGE)));
            };
            (Some(first.to_string()), second)
        }
    };

    let Some(attribute) = parse_char_attribute(property_token) else {
        return Some(Err(anyhow::anyhow!(SET_USAGE)));
    };

    let Some(amount) = parts.next() else {
        return Some(Err(anyhow::anyhow!(SET_USAGE)));
    };
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!(SET_USAGE)));
    }

    match amount.parse::<i64>() {
        Ok(amount) => Some(Ok((charname, attribute, amount))),
        Err(_) => Some(Err(anyhow::anyhow!(SET_USAGE))),
    }
}

/// Mirrors a `set` command's applied value into the in-memory character
/// snapshot when the target is the caller's own active character, so the
/// rest of the session (and anyone it broadcasts to) sees it without
/// needing a relog.
pub(crate) fn apply_attribute_in_memory(
    character: &mut DbCharacter,
    attribute: CharAttribute,
    value: i32,
) {
    match attribute {
        CharAttribute::Level => character.level = value as u16,
        CharAttribute::Grow => character.mode = value as u8,
        CharAttribute::Str => character.strength = value as u16,
        CharAttribute::Dex => character.dexterity = value as u16,
        CharAttribute::Rec => character.reaction = value as u16,
        CharAttribute::Int => character.intelligence = value as u16,
        CharAttribute::Luc => character.luck = value as u16,
        CharAttribute::Wis => character.wisdom = value as u16,
        CharAttribute::StatPoint => character.stat_point = value as u16,
        CharAttribute::SkillPoint => character.skill_point = value as u16,
        CharAttribute::Kills => character.kills = value as u32,
        CharAttribute::Deaths => character.deaths = value as u32,
        CharAttribute::Money => character.money = value as u32,
    }
}

/// `amove <charname>`. Moves the GM to the target's current location.
pub(crate) fn parse_amove_command(message: &str) -> Option<Result<String>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("amove") {
        return None;
    }
    let Some(charname) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: amove <charname>")));
    };
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!("Usage: amove <charname>")));
    }
    Some(Ok(charname.to_string()))
}

/// `asummon <charname>`. Brings the target to the GM's current location.
/// Only meaningful for a currently-online target (nothing to move
/// otherwise), enforced where this is handled, not here.
pub(crate) fn parse_asummon_command(message: &str) -> Option<Result<String>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("asummon") {
        return None;
    }
    let Some(charname) = parts.next() else {
        return Some(Err(anyhow::anyhow!("Usage: asummon <charname>")));
    };
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!("Usage: asummon <charname>")));
    }
    Some(Ok(charname.to_string()))
}

/// `iclear [charname]`. Instantly removes every inventory (non-equipped)
/// item. Defaults to the caller if no name is given.
pub(crate) fn parse_iclear_command(message: &str) -> Option<Result<Option<String>>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("iclear") {
        return None;
    }
    let charname = parts.next().map(str::to_string);
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!("Usage: iclear [charname]")));
    }
    Some(Ok(charname))
}

/// `eclear [charname]`. Same as iclear, but for currently equipped items.
pub(crate) fn parse_eclear_command(message: &str) -> Option<Result<Option<String>>> {
    let mut parts = message.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("eclear") {
        return None;
    }
    let charname = parts.next().map(str::to_string);
    if parts.next().is_some() {
        return Some(Err(anyhow::anyhow!("Usage: eclear [charname]")));
    }
    Some(Ok(charname))
}

/// Executes an already-parsed `mmake` command: scatters `count` mobs of
/// `mob_id` within `radius` of the caller and broadcasts their spawn to
/// nearby players. Returns the chat response text.
pub(crate) fn execute_mmake(
    character: &DbCharacter,
    mob_id: u16,
    count: u8,
    radius: f32,
    logger: &Logger,
) -> Result<String> {
    let mobs = spawn_mobs_scattered(
        character.map_id,
        mob_id,
        count,
        character.pos_x,
        character.pos_z,
        radius,
    );
    logger.line(format!(
        "Rapture World mmake character_id={} mob_id={} count={count} radius={radius:.1} map={}",
        character.id, mob_id, character.map_id
    ));
    for player in players_on_map_near(character.map_id, character.pos_x, character.pos_z) {
        for mob in &mobs {
            let packet = build_mob_enter(mob, true)?;
            player.send_packet(&packet, "0x0601 mmake spawn mob", logger)?;
        }
    }
    Ok(format!(
        "Spawned {} mob(s) ({mob_id}) within {radius:.0}m.",
        mobs.len()
    ))
}

/// Executes an already-parsed `mera` command: removes every mob within
/// `radius` of the caller and broadcasts their despawn.
pub(crate) fn execute_mera(character: &DbCharacter, radius: f32, logger: &Logger) -> Result<String> {
    let removed = remove_mobs_in_radius(character.map_id, character.pos_x, character.pos_z, radius);
    logger.line(format!(
        "Rapture World mera character_id={} radius={radius:.1} removed={} map={}",
        character.id,
        removed.len(),
        character.map_id
    ));
    for player in players_on_map_near(character.map_id, character.pos_x, character.pos_z) {
        for &mob_id in &removed {
            let packet = build_mob_leave(mob_id)?;
            player.send_packet(&packet, "0x0602 mera despawn mob", logger)?;
        }
    }
    Ok(format!("Removed {} mob(s) within {radius:.0}m.", removed.len()))
}

/// Executes an already-parsed `nmake` command: spawns one NPC at the
/// caller's position and broadcasts its spawn.
pub(crate) fn execute_nmake(
    character: &DbCharacter,
    npc_type: u8,
    type_id: u16,
    logger: &Logger,
) -> Result<String> {
    let npc = spawn_npc(
        character.map_id,
        npc_type,
        type_id,
        character.pos_x,
        character.pos_y,
        character.pos_z,
        0,
    );
    logger.line(format!(
        "Rapture World nmake character_id={} npc_id={} type={npc_type} type_id={type_id} map={} pos=({:.2}, {:.2}, {:.2})",
        character.id, npc.id, character.map_id, npc.pos_x, npc.pos_y, npc.pos_z
    ));
    let packet = build_npc_enter(&npc)?;
    for player in players_on_map_near(character.map_id, character.pos_x, character.pos_z) {
        player.send_packet(&packet, "0x0E01 nmake spawn npc", logger)?;
    }
    Ok(format!("Spawned NPC {npc_type} {type_id}."))
}

/// Executes an already-parsed `nera` command: removes every NPC within
/// `radius` of the caller and broadcasts their despawn.
pub(crate) fn execute_nera(character: &DbCharacter, radius: f32, logger: &Logger) -> Result<String> {
    let removed = remove_npcs_in_radius(character.map_id, character.pos_x, character.pos_z, radius);
    logger.line(format!(
        "Rapture World nera character_id={} radius={radius:.1} removed={} map={}",
        character.id,
        removed.len(),
        character.map_id
    ));
    for player in players_on_map_near(character.map_id, character.pos_x, character.pos_z) {
        for &npc_id in &removed {
            let packet = build_npc_leave(npc_id)?;
            player.send_packet(&packet, "0x0E02 nera despawn npc", logger)?;
        }
    }
    Ok(format!("Removed {} NPC(s) within {radius:.0}m.", removed.len()))
}

/// Executes an already-parsed `mobreload` command: clears every mob on
/// `map_id`, then respawns the .svmap-defined set after a 5s delay on a
/// background thread (`logger_arc` outlives this call, unlike `logger`).
pub(crate) fn execute_mobreload(
    character: &DbCharacter,
    map_id: u16,
    logger: &Logger,
    logger_arc: &Arc<Logger>,
) -> Result<String> {
    match load_svmap_mobs(map_id, logger) {
        Ok(fresh_mobs) => {
            let removed = clear_mobs_on_map(map_id);
            let scheduled_count = fresh_mobs.len();
            logger.line(format!(
                "Rapture World mobreload character_id={} map={map_id} removed={} scheduled={scheduled_count}",
                character.id,
                removed.len()
            ));
            for player in players_on_map(map_id) {
                for &mob_id in &removed {
                    let packet = build_mob_leave(mob_id)?;
                    player.send_packet(&packet, "0x0602 mobreload despawn mob", logger)?;
                }
            }
            let logger_arc = Arc::clone(logger_arc);
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(5));
                let recipients = players_on_map(map_id);
                for mob in &fresh_mobs {
                    if let Ok(packet) = build_mob_enter(mob, true) {
                        for player in &recipients {
                            let _ =
                                player.send_packet(&packet, "0x0601 mobreload respawn mob", &logger_arc);
                        }
                    }
                }
                add_mobs(map_id, fresh_mobs);
            });
            Ok(format!(
                "Reloading map {map_id}: removed {} mob(s), respawning {scheduled_count} in 5s.",
                removed.len()
            ))
        }
        Err(error) => Ok(format!("Failed to load svmap for map {map_id}: {error}")),
    }
}

/// Executes an already-parsed `npcreload` command: same idea as
/// `execute_mobreload`, for NPCs.
pub(crate) fn execute_npcreload(
    character: &DbCharacter,
    map_id: u16,
    logger: &Logger,
    logger_arc: &Arc<Logger>,
) -> Result<String> {
    match load_svmap_npcs(map_id, logger) {
        Ok(fresh_npcs) => {
            let removed = clear_npcs_on_map(map_id);
            let scheduled_count = fresh_npcs.len();
            logger.line(format!(
                "Rapture World npcreload character_id={} map={map_id} removed={} scheduled={scheduled_count}",
                character.id,
                removed.len()
            ));
            for player in players_on_map(map_id) {
                for &npc_id in &removed {
                    let packet = build_npc_leave(npc_id)?;
                    player.send_packet(&packet, "0x0E02 npcreload despawn npc", logger)?;
                }
            }
            let logger_arc = Arc::clone(logger_arc);
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(5));
                let recipients = players_on_map(map_id);
                for npc in &fresh_npcs {
                    if let Ok(packet) = build_npc_enter(npc) {
                        for player in &recipients {
                            let _ =
                                player.send_packet(&packet, "0x0E01 npcreload respawn npc", &logger_arc);
                        }
                    }
                }
                add_npcs(map_id, fresh_npcs);
            });
            Ok(format!(
                "Reloading map {map_id}: removed {} NPC(s), respawning {scheduled_count} in 5s.",
                removed.len()
            ))
        }
        Err(error) => Ok(format!("Failed to load svmap for map {map_id}: {error}")),
    }
}

/// Executes an already-parsed `char` (GM visibility toggle) command.
/// `visible = true` retroactively introduces the caller to every non-admin
/// viewer who can currently see them (turning invisible again only takes
/// effect for future introductions -- see the doc comment at the call
/// site in main.rs for why).
pub(crate) fn execute_char_visibility(
    character: &DbCharacter,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    visible: bool,
    logger: &Logger,
) -> Result<String> {
    if let Some(player) = connected_player.as_ref() {
        player.set_gm_visible(visible);
        if visible {
            let others = players_on_map_excluding_near(
                character.map_id,
                character.id,
                character.pos_x,
                character.pos_z,
            );
            let entered = player.entered_map_packet()?;
            let shape = player.shape_packet()?;
            let move_packet = player.move_packet()?;
            let overlay = player.overlay_packet()?;
            let sitting = player
                .is_sitting()
                .then(|| player.sit_motion_packet())
                .transpose()?;
            for other in &others {
                if other.is_admin {
                    continue;
                }
                other.send_packet(&entered, "0x0201 char off introduce", logger)?;
                other.send_packet(&shape, "0x0303 char off introduce", logger)?;
                other.send_packet(&move_packet, "0x0501 char off introduce", logger)?;
                other.send_packet(&overlay, "0x0240 char off introduce", logger)?;
                if let Some(sit_packet) = &sitting {
                    other.send_packet(sit_packet, "0x0506 char off introduce", logger)?;
                }
            }
        }
    }
    logger.line(format!(
        "Rapture World char character_id={} visible={visible}",
        character.id
    ));
    Ok(if visible {
        "You are now visible to non-admins.".to_string()
    } else {
        "You are invisible to non-admins again.".to_string()
    })
}

/// Executes an already-parsed `town` command: teleports the caller to the
/// nearest town spawn area for their faction on the current map. Unlike
/// cmove/bmove/amove this does NOT go through `apply_teleport` (it never
/// introduces the caller to other viewers on "arrival" since the map never
/// changes -- only position does), matching the original inline behavior.
/// Returns the packets to send (in order) plus the chat response text.
#[allow(clippy::type_complexity)]
pub(crate) fn execute_town(
    character: &mut DbCharacter,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    selected_faction: u8,
    entity_visibility: &mut EntityVisibility,
    db_conn: &mut db::Db,
    position_dirty: &mut bool,
    last_position_save: &mut Instant,
    logger: &Logger,
) -> Result<(Vec<(Vec<u8>, &'static str)>, String)> {
    let mut packets = Vec::new();
    let response = if let Some(spawn_area) =
        find_nearest_spawn_area(character.map_id, selected_faction, character.pos_x, character.pos_z)
    {
        let (town_x, town_y, town_z) = spawn_area.center();
        packets.extend(build_entity_visibility_clear(entity_visibility)?);

        character.pos_x = town_x;
        character.pos_y = town_y;
        character.pos_z = town_z;

        if let Some(player) = connected_player.as_ref() {
            player.update_character(character.clone());
            player.update_motion(0, 0);
        }
        db::save_character_position(
            db_conn,
            character.id,
            character.map_id,
            character.pos_x,
            character.pos_y,
            character.pos_z,
        )?;
        *position_dirty = false;
        *last_position_save = Instant::now();

        packets.push((
            build_character_map_position(Some(character))?,
            "0x020B town teleport",
        ));
        packets.push((
            build_character_move(Some(character), 0, 0)?,
            "0x0501 town teleport move",
        ));
        packets.extend(build_entity_visibility_delta(
            character.map_id,
            character.pos_x,
            character.pos_z,
            entity_visibility,
        )?);

        logger.line(format!(
            "Rapture World town teleport character_id={} map={} faction={} spawn_faction={} pos=({:.2}, {:.2}, {:.2})",
            character.id,
            character.map_id,
            selected_faction,
            spawn_area.faction,
            character.pos_x,
            character.pos_y,
            character.pos_z
        ));

        format!(
            "Returned to nearest town spawn ({:.2}, {:.2}, {:.2}).",
            character.pos_x, character.pos_y, character.pos_z
        )
    } else {
        logger.line(format!(
            "Rapture World town command found no spawn area map={} faction={} character_id={}",
            character.map_id, selected_faction, character.id
        ));
        "No town spawn area was found for this map/faction.".to_string()
    };

    Ok((packets, response))
}

/// Executes an already-parsed, already-admin-checked `cmove` command:
/// teleports the caller to `map_id` (falling back to a sensible spawn area
/// if `target` isn't an exact walkable spot). Returns the chat response
/// text.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_cmove(
    character: &mut DbCharacter,
    map_id: u16,
    target: MoveTarget,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    is_admin: bool,
    selected_faction: u8,
    entity_visibility: &mut EntityVisibility,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    position_dirty: &mut bool,
    last_position_save: &mut Instant,
    logger: &Logger,
) -> Result<String> {
    let resolution = resolve_move_position(map_id, target, selected_faction, None, character.pos_y);

    apply_teleport(
        character,
        connected_player,
        is_admin,
        entity_visibility,
        db_conn,
        stream,
        aes_send,
        use_expanded_send_key,
        position_dirty,
        last_position_save,
        map_id,
        resolution.x,
        resolution.y,
        resolution.z,
        "cmove",
        logger,
    )?;

    logger.line(format!(
        "Rapture World cmove character_id={} map={} pos=({:.2}, {:.2}, {:.2}) fallback={}",
        character.id,
        character.map_id,
        character.pos_x,
        character.pos_y,
        character.pos_z,
        resolution.used_fallback
    ));

    Ok(if resolution.used_fallback {
        format!(
            "Moved to map {map_id} ({:.2}, {:.2}, {:.2}) (fallback position).",
            resolution.x, resolution.y, resolution.z
        )
    } else {
        format!(
            "Moved to map {map_id} ({:.2}, {:.2}, {:.2}).",
            resolution.x, resolution.y, resolution.z
        )
    })
}

/// Executes an already-parsed, already-admin-checked `bmove` command:
/// teleports the caller within their current map to `(x, y, z)` (falling
/// back to a sensible spawn area if that spot isn't walkable). Returns the
/// chat response text.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_bmove(
    character: &mut DbCharacter,
    x: f32,
    y: f32,
    z: f32,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    is_admin: bool,
    selected_faction: u8,
    entity_visibility: &mut EntityVisibility,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    position_dirty: &mut bool,
    last_position_save: &mut Instant,
    logger: &Logger,
) -> Result<String> {
    let resolution = resolve_move_position(
        character.map_id,
        MoveTarget::Exact(x, y, z),
        selected_faction,
        Some((character.pos_x, character.pos_z)),
        character.pos_y,
    );

    let map_id = character.map_id;
    apply_teleport(
        character,
        connected_player,
        is_admin,
        entity_visibility,
        db_conn,
        stream,
        aes_send,
        use_expanded_send_key,
        position_dirty,
        last_position_save,
        map_id,
        resolution.x,
        resolution.y,
        resolution.z,
        "bmove",
        logger,
    )?;

    logger.line(format!(
        "Rapture World bmove character_id={} map={} pos=({:.2}, {:.2}, {:.2}) fallback={}",
        character.id,
        character.map_id,
        character.pos_x,
        character.pos_y,
        character.pos_z,
        resolution.used_fallback
    ));

    Ok(if resolution.used_fallback {
        format!(
            "Moved to ({:.2}, {:.2}, {:.2}) (fallback position).",
            resolution.x, resolution.y, resolution.z
        )
    } else {
        format!(
            "Moved to ({:.2}, {:.2}, {:.2}).",
            resolution.x, resolution.y, resolution.z
        )
    })
}

/// Executes an already-parsed, already-admin-checked `set` command: sets
/// `attribute` to `amount` on the caller (no charname given) or another
/// character by name. If the target is the caller's own active character,
/// mirrors the change into the live in-memory snapshot and pushes the
/// recalculated stats live (0x0526/0x051F/0x050B, plus 0x0213 for money) so
/// the client shows it immediately instead of only after a relog.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_set(
    character: &DbCharacter,
    active_character: &mut Option<DbCharacter>,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &[DbCharacterItem],
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    charname: Option<String>,
    attribute: CharAttribute,
    amount: i64,
    logger: &Logger,
) -> Result<String> {
    let target = match &charname {
        Some(name) => db::get_character_by_name(db_conn, name)?,
        None => Some(character.clone()),
    };
    match target {
        Some(target_char) => {
            let applied = db::set_character_attribute(db_conn, target_char.id, attribute, amount)?;
            logger.line(format!(
                "Rapture World set character_id={} attribute={} value={applied} (by character_id={})",
                target_char.id,
                attribute.name(),
                character.id
            ));
            if target_char.id == character.id {
                if let Some(active) = active_character.as_mut() {
                    apply_attribute_in_memory(active, attribute, applied);
                    if let Some(player) = connected_player.as_ref() {
                        player.update_character(active.clone());
                    }
                    // Push the recalculated stats live so the client shows
                    // the change immediately -- without this, the change is
                    // correct in the DB/session but invisible until relog
                    // (0x0526/0x051F/0x050B are the "live stat update"
                    // packets, distinct from 0x0105 CharacterDetails which
                    // is only sent once at character select).
                    if let Some(send) = aes_send.as_mut() {
                        if matches!(attribute, CharAttribute::Money) {
                            send_encrypted(
                                stream,
                                send,
                                use_expanded_send_key,
                                build_set_money(active.money)?,
                                "0x0213 set money update",
                                logger,
                            )?;
                        }
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
                }
            }
            Ok(format!(
                "Set {} on {} to {applied}.",
                attribute.name(),
                target_char.name
            ))
        }
        None => Ok(format!(
            "Character '{}' not found.",
            charname.unwrap_or_default()
        )),
    }
}

/// Executes an already-parsed, already-admin-checked `amove` command:
/// teleports the caller to an online-or-offline target character's current
/// location. Returns (caller's character id, chat response text).
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_amove(
    active_character: &mut Option<DbCharacter>,
    charname: String,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    is_admin: bool,
    entity_visibility: &mut EntityVisibility,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    position_dirty: &mut bool,
    last_position_save: &mut Instant,
    logger: &Logger,
) -> Result<(u32, String)> {
    let target = find_player_by_name(&charname)
        .map(|player| player.character_snapshot())
        .or(db::get_character_by_name(db_conn, &charname)?);
    match target {
        Some(target_char) => {
            let Some(character) = active_character.as_mut() else {
                return Ok((0, "No active character.".to_string()));
            };
            apply_teleport(
                character,
                connected_player,
                is_admin,
                entity_visibility,
                db_conn,
                stream,
                aes_send,
                use_expanded_send_key,
                position_dirty,
                last_position_save,
                target_char.map_id,
                target_char.pos_x,
                target_char.pos_y,
                target_char.pos_z,
                "amove",
                logger,
            )?;
            logger.line(format!(
                "Rapture World amove character_id={} to character_id={} map={} pos=({:.2}, {:.2}, {:.2})",
                character.id,
                target_char.id,
                character.map_id,
                character.pos_x,
                character.pos_y,
                character.pos_z
            ));
            Ok((
                character.id,
                format!("Moved to {}'s location.", target_char.name),
            ))
        }
        None => {
            let character_id = active_character.as_ref().map(|c| c.id).unwrap_or(0);
            Ok((character_id, format!("Character '{charname}' not found.")))
        }
    }
}

/// Executes an already-parsed, already-admin-checked `asummon` command:
/// queues a teleport on the target's OWN ConnectedPlayer instead of
/// injecting packets/mutating their state from this thread -- their own
/// connection picks it up on its next loop iteration (see the
/// pending-teleport check in main.rs) and runs it through the exact same
/// `apply_teleport` a self /bmove or /cmove does, so their own
/// active_character/entity_visibility locals stay correct.
pub(crate) fn execute_asummon(
    character: &DbCharacter,
    charname: &str,
    logger: &Logger,
) -> Result<String> {
    match find_player_by_name(charname) {
        Some(target_player) => {
            if target_player.character_id == character.id {
                Ok("You can't summon yourself.".to_string())
            } else {
                target_player.queue_teleport(
                    character.map_id,
                    character.pos_x,
                    character.pos_y,
                    character.pos_z,
                );
                logger.line(format!(
                    "Rapture World asummon character_id={} queued teleport for target_character_id={} to map={} pos=({:.2}, {:.2}, {:.2})",
                    character.id, target_player.character_id, character.map_id, character.pos_x, character.pos_y, character.pos_z
                ));
                Ok(format!(
                    "Summoned {} to your location.",
                    target_player.character_name
                ))
            }
        }
        None => Ok(format!("Character '{charname}' is not online.")),
    }
}

/// Resolves an `iclear`/`eclear` charname argument (`None` = self) to
/// (target_id, target_name), matching case-insensitively against the
/// caller's own name before falling back to a DB lookup.
fn resolve_clear_target(
    character: &DbCharacter,
    charname: &Option<String>,
    db_conn: &mut db::Db,
) -> Result<Option<(u32, String)>> {
    match charname {
        None => Ok(Some((character.id, character.name.clone()))),
        Some(name) if name.eq_ignore_ascii_case(&character.name) => {
            Ok(Some((character.id, character.name.clone())))
        }
        Some(name) => Ok(db::get_character_by_name(db_conn, name)?.map(|found| (found.id, found.name))),
    }
}

/// Executes an already-parsed, already-admin-checked `iclear` command:
/// instantly removes every non-equipped inventory item from the caller (no
/// charname given) or another character by name (online or offline).
pub(crate) fn execute_iclear(
    character: &DbCharacter,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    active_items: &mut Vec<DbCharacterItem>,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    charname: Option<String>,
    logger: &Logger,
) -> Result<String> {
    let resolved = resolve_clear_target(character, &charname, db_conn)?;
    match resolved {
        Some((target_id, target_name)) => {
            let is_self = target_id == character.id;
            let online_target = if is_self {
                None
            } else {
                find_player_by_name(&target_name)
            };
            let remote_items = match &online_target {
                Some(_) => Some(db::get_character_items(db_conn, target_id)?),
                None => None,
            };
            let count = db::count_character_items(db_conn, target_id, false)?;
            db::clear_character_items(db_conn, target_id, false)?;
            logger.line(format!(
                "Rapture World iclear character_id={target_id} removed={count} (by character_id={})",
                character.id
            ));
            if is_self {
                if let Some(send) = aes_send.as_mut() {
                    for item in active_items.iter().filter(|item| item.bag != 0) {
                        send_encrypted(
                            stream,
                            send,
                            use_expanded_send_key,
                            build_remove_item(item, true)?,
                            "0x0206 iclear remove item",
                            logger,
                        )?;
                    }
                }
                active_items.retain(|item| item.bag == 0);
                if let Some(player) = connected_player.as_ref() {
                    player.update_items(active_items.clone());
                }
            } else if let (Some(target_player), Some(items)) = (online_target, remote_items) {
                for item in items.iter().filter(|item| item.bag != 0) {
                    let packet = build_remove_item(item, true)?;
                    target_player.send_packet(&packet, "0x0206 iclear remove item", logger)?;
                }
            }
            Ok(format!(
                "Cleared {count} inventory item(s) from {target_name}."
            ))
        }
        None => Ok(format!(
            "Character '{}' not found.",
            charname.unwrap_or_default()
        )),
    }
}

/// Executes an already-parsed, already-admin-checked `eclear` command:
/// same as `execute_iclear`, but for currently equipped items -- also
/// rebroadcasts the now-bare equipment slots to other viewers when the
/// target is the caller (a remote target's equipment change is
/// best-effort only: their own client is updated, but other players
/// looking at them aren't rebroadcast to, since this thread doesn't have
/// their live map/visibility state).
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_eclear(
    character: &DbCharacter,
    connected_player: &Option<Arc<ConnectedPlayer>>,
    is_admin: bool,
    active_items: &mut Vec<DbCharacterItem>,
    db_conn: &mut db::Db,
    stream: &mut TcpStream,
    aes_send: &mut Option<AesCtr>,
    use_expanded_send_key: bool,
    charname: Option<String>,
    logger: &Logger,
) -> Result<String> {
    let resolved = resolve_clear_target(character, &charname, db_conn)?;
    match resolved {
        Some((target_id, target_name)) => {
            let is_self = target_id == character.id;
            let online_target = if is_self {
                None
            } else {
                find_player_by_name(&target_name)
            };
            let remote_items = match &online_target {
                Some(_) => Some(db::get_character_items(db_conn, target_id)?),
                None => None,
            };
            let count = db::count_character_items(db_conn, target_id, true)?;
            db::clear_character_items(db_conn, target_id, true)?;
            logger.line(format!(
                "Rapture World eclear character_id={target_id} removed={count} (by character_id={})",
                character.id
            ));
            if is_self {
                let cleared_slots: Vec<u8> = active_items
                    .iter()
                    .filter(|item| item.bag == 0 && item.slot < 8)
                    .map(|item| item.slot)
                    .collect();
                if let Some(send) = aes_send.as_mut() {
                    for item in active_items.iter().filter(|item| item.bag == 0) {
                        send_encrypted(
                            stream,
                            send,
                            use_expanded_send_key,
                            build_remove_item(item, true)?,
                            "0x0206 eclear remove item",
                            logger,
                        )?;
                    }
                }
                active_items.retain(|item| item.bag != 0);
                if let Some(send) = aes_send.as_mut() {
                    for &slot in &cleared_slots {
                        send_encrypted(
                            stream,
                            send,
                            use_expanded_send_key,
                            build_character_equipment(character, active_items, slot)?,
                            &format!("0x0507 character equipment slot {slot}"),
                            logger,
                        )?;
                    }
                }
                if !cleared_slots.is_empty() {
                    if let Some(player) = connected_player.as_ref() {
                        player.update_items(active_items.clone());
                        let others = players_on_map_excluding_near(
                            character.map_id,
                            character.id,
                            character.pos_x,
                            character.pos_z,
                        );
                        for &slot in &cleared_slots {
                            let equipment_packet = player.equipment_packet(slot)?;
                            for other in &others {
                                if !can_see_character(other.is_admin, is_admin, player.gm_visible())
                                {
                                    continue;
                                }
                                other.send_packet(
                                    &equipment_packet,
                                    &format!("0x0507 character equipment slot {slot} (broadcast)"),
                                    logger,
                                )?;
                            }
                        }
                    }
                }
            } else if let (Some(target_player), Some(items)) = (online_target, remote_items) {
                for item in items.iter().filter(|item| item.bag == 0) {
                    let packet = build_remove_item(item, true)?;
                    target_player.send_packet(&packet, "0x0206 eclear remove item", logger)?;
                }
            }
            Ok(format!(
                "Cleared {count} equipped item(s) from {target_name}."
            ))
        }
        None => Ok(format!(
            "Character '{}' not found.",
            charname.unwrap_or_default()
        )),
    }
}
