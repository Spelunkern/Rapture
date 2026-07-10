use anyhow::{Context, Result, bail};
use rapture_common::db::{self, DbMobDefinition};
use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use crate::session::Logger;
use crate::world_entities::{
    MapEntities, MapMobArea, MapPortal, MapSpawnArea, NpcPatrolPoint, add_entities,
    build_static_mob, build_static_npc_with_patrol, is_walkable_in_mask, register_map_grid,
};

pub(crate) const MAP_DATA_DIR: &str = "data/maps";

#[derive(Clone, Copy)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

#[derive(Clone, Copy)]
struct BoundingBox {
    lower: Vec3,
    upper: Vec3,
}

struct SvmapLoad {
    map_size: u32,
    cell_size: u32,
    map_mask: Vec<u8>,
    entities: MapEntities,
}

struct SvmapReader<'a> {
    inner: Cursor<&'a [u8]>,
}

impl<'a> SvmapReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            inner: Cursor::new(data),
        }
    }

    fn position(&self) -> u64 {
        self.inner.position()
    }

    fn read_exact<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut bytes = [0u8; N];
        self.inner.read_exact(&mut bytes).with_context(|| {
            format!(
                "unexpected end of svmap at offset {} while reading {} byte(s)",
                self.position(),
                N
            )
        })?;
        Ok(bytes)
    }

    fn read_i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.read_exact()?))
    }

    fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.read_exact()?))
    }

    fn read_f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.read_exact()?))
    }

    fn read_vec3(&mut self) -> Result<Vec3> {
        Ok(Vec3 {
            x: self.read_f32()?,
            y: self.read_f32()?,
            z: self.read_f32()?,
        })
    }

    fn read_box(&mut self) -> Result<BoundingBox> {
        Ok(BoundingBox {
            lower: self.read_vec3()?,
            upper: self.read_vec3()?,
        })
    }

    fn skip(&mut self, len: usize) -> Result<()> {
        let mut sink = vec![0u8; len];
        self.inner.read_exact(&mut sink).with_context(|| {
            format!(
                "unexpected end of svmap at offset {} while skipping {} byte(s)",
                self.position(),
                len
            )
        })?;
        Ok(())
    }

    fn read_bytes(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut bytes = vec![0u8; len];
        self.inner.read_exact(&mut bytes).with_context(|| {
            format!(
                "unexpected end of svmap at offset {} while reading {} byte(s)",
                self.position(),
                len
            )
        })?;
        Ok(bytes)
    }

    fn read_count(&mut self, label: &str) -> Result<usize> {
        let count = self.read_i32()?;
        if count < 0 {
            bail!("negative {label} count in svmap: {count}");
        }
        Ok(count as usize)
    }
}

pub(crate) fn ensure_map_data_dir() -> Result<()> {
    fs::create_dir_all(MAP_DATA_DIR).context("failed to create Rapture map data directory")?;
    let readme = Path::new(MAP_DATA_DIR).join("README.txt");
    if !readme.exists() {
        fs::write(
            &readme,
            "Place original Shaiya .svmap files here using the map id as filename, for example 1.svmap or 42.svmap.\r\nRapture loads NPCs and monster spawn areas from these files when rapture_world starts.\r\n",
        )
        .with_context(|| format!("failed to write {}", readme.display()))?;
    }
    Ok(())
}

pub(crate) fn load_svmap_entities(logger: &Logger) -> Result<()> {
    ensure_map_data_dir()?;
    let mob_definitions = load_mob_definitions(logger);

    let mut loaded_maps = 0usize;
    let mut loaded_npcs = 0usize;
    let mut loaded_mobs = 0usize;

    for entry in fs::read_dir(MAP_DATA_DIR).context("failed to read Rapture map data directory")? {
        let entry = entry.context("failed to read Rapture map data entry")?;
        let path = entry.path();
        if !is_svmap_file(&path) {
            continue;
        }

        let Some(map_id) = map_id_from_path(&path) else {
            logger.line(format!(
                "Rapture World skipped svmap with non-numeric filename: {}",
                path.display()
            ));
            continue;
        };

        match load_svmap_file(map_id, &path, &mob_definitions) {
            Ok(load) => {
                let npc_count = load.entities.npcs.len();
                let mob_count = load.entities.mobs.len();
                let portal_count = load.entities.portals.len();
                let spawn_area_count = load.entities.spawn_areas.len();
                register_map_grid(map_id, load.map_size, load.cell_size, load.map_mask.clone());
                add_entities(map_id, load.entities);
                loaded_maps += 1;
                loaded_npcs += npc_count;
                loaded_mobs += mob_count;
                logger.line(format!(
                    "Rapture World loaded svmap map={} map_size={} cell_size={} mask_bytes={} npcs={} mobs={} portals={} spawn_areas={} path={}",
                    map_id,
                    load.map_size,
                    load.cell_size,
                    load.map_mask.len(),
                    npc_count,
                    mob_count,
                    portal_count,
                    spawn_area_count,
                    path.display()
                ));
            }
            Err(error) => {
                logger.line(format!(
                    "Rapture World failed to load svmap {}: {error:?}",
                    path.display()
                ));
            }
        }
    }

    logger.line(format!(
        "Rapture World svmap load complete maps={} npcs={} mobs={}",
        loaded_maps, loaded_npcs, loaded_mobs
    ));
    Ok(())
}

fn load_mob_definitions(logger: &Logger) -> HashMap<u16, DbMobDefinition> {
    match db::connect().and_then(|mut conn| db::get_mob_definitions(&mut conn)) {
        Ok(definitions) => {
            logger.line(format!(
                "Rapture World loaded {} mob definition(s) from SQL Server",
                definitions.len()
            ));
            definitions
        }
        Err(error) => {
            logger.line(format!(
                "Rapture World failed to load mob definitions from SQL Server, using fallback HP: {error:?}"
            ));
            HashMap::new()
        }
    }
}

/// Re-reads a single map's .svmap file fresh, without touching the live
/// registry. Used by the `mobreload`/`npcreload` GM commands: the caller
/// decides when to actually clear/apply the result, so a missing/corrupt
/// file never wipes out mobs or NPCs that are already live on the map.
fn reload_svmap_file(map_id: u16, logger: &Logger) -> Result<SvmapLoad> {
    let path = PathBuf::from(MAP_DATA_DIR).join(format!("{map_id}.svmap"));
    let mob_definitions = load_mob_definitions(logger);
    load_svmap_file(map_id, &path, &mob_definitions)
}

/// Fresh mob list for `map_id` straight from its .svmap file, plus a
/// grid re-registration (harmless if unchanged, and picks up a map that
/// wasn't loaded at server startup).
pub(crate) fn load_svmap_mobs(
    map_id: u16,
    logger: &Logger,
) -> Result<Vec<crate::world_entities::MapMob>> {
    let load = reload_svmap_file(map_id, logger)?;
    register_map_grid(map_id, load.map_size, load.cell_size, load.map_mask);
    Ok(load.entities.mobs)
}

/// Fresh NPC list for `map_id` straight from its .svmap file. See
/// load_svmap_mobs.
pub(crate) fn load_svmap_npcs(
    map_id: u16,
    logger: &Logger,
) -> Result<Vec<crate::world_entities::MapNpc>> {
    let load = reload_svmap_file(map_id, logger)?;
    register_map_grid(map_id, load.map_size, load.cell_size, load.map_mask);
    Ok(load.entities.npcs)
}

fn is_svmap_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svmap"))
}

fn map_id_from_path(path: &Path) -> Option<u16> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.parse::<u16>().ok())
}

fn load_svmap_file(
    map_id: u16,
    path: &PathBuf,
    mob_definitions: &HashMap<u16, DbMobDefinition>,
) -> Result<SvmapLoad> {
    let data = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut reader = SvmapReader::new(&data);

    let map_size = reader.read_i32()?;
    if map_size <= 0 {
        bail!("invalid map size {map_size}");
    }

    let mask_len = (map_size as usize)
        .checked_mul(map_size as usize)
        .and_then(|value| value.checked_div(8))
        .context("invalid map mask length")?;
    let map_mask = reader.read_bytes(mask_len)?;

    let cell_size = reader.read_i32()?;
    if cell_size < 0 {
        bail!("invalid cell size {cell_size}");
    }

    skip_ladders(&mut reader)?;
    let mobs = read_monster_areas(
        map_id,
        map_size as u32,
        &map_mask,
        mob_definitions,
        &mut reader,
    )?;
    let npcs = read_npcs(&mut reader)?;
    let portals = read_portals(&mut reader)?;
    let spawn_areas = read_spawn_areas(&mut reader)?;
    skip_named_areas(&mut reader)?;

    Ok(SvmapLoad {
        map_size: map_size as u32,
        cell_size: cell_size as u32,
        map_mask,
        entities: MapEntities {
            npcs,
            mobs,
            portals,
            spawn_areas,
        },
    })
}

fn skip_ladders(reader: &mut SvmapReader<'_>) -> Result<()> {
    let count = reader.read_count("ladder")?;
    reader.skip(count.saturating_mul(12))
}

fn read_monster_areas(
    _map_id: u16,
    map_size: u32,
    map_mask: &[u8],
    mob_definitions: &HashMap<u16, DbMobDefinition>,
    reader: &mut SvmapReader<'_>,
) -> Result<Vec<crate::world_entities::MapMob>> {
    let area_count = reader.read_count("monster area")?;
    let mut mobs = Vec::new();

    for _ in 0..area_count {
        let area = reader.read_box()?;
        let mob_area = MapMobArea {
            lower_x: area.lower.x,
            lower_z: area.lower.z,
            upper_x: area.upper.x,
            upper_z: area.upper.z,
        }
        .normalized();
        let monster_count = reader.read_count("monster spawn")?;
        let mut area_spawn_index = 0u32;

        for _ in 0..monster_count {
            let mob_id = reader.read_u32()?;
            let count = reader.read_u32()?;
            if mob_id > u16::MAX as u32 {
                continue;
            }

            let max_hp = mob_definitions
                .get(&(mob_id as u16))
                .map(|definition| definition.max_hp)
                .unwrap_or(100);
            let money_min = mob_definitions
                .get(&(mob_id as u16))
                .map(|definition| definition.money_min)
                .unwrap_or(0);
            let money_max = mob_definitions
                .get(&(mob_id as u16))
                .map(|definition| definition.money_max)
                .unwrap_or(0);
            let respawn_seconds = mob_definitions
                .get(&(mob_id as u16))
                .and_then(|definition| definition.respawn_seconds);
            let exp = mob_definitions
                .get(&(mob_id as u16))
                .map(|definition| definition.exp)
                .unwrap_or(0);

            for _ in 0..count {
                let position = position_in_area(area, area_spawn_index, map_size, map_mask);
                mobs.push(build_static_mob(
                    mob_id as u16,
                    position.x,
                    position.z,
                    max_hp,
                    money_min,
                    money_max,
                    exp,
                    Some(mob_area),
                    respawn_seconds,
                ));
                area_spawn_index = area_spawn_index.saturating_add(1);
            }
        }
    }

    Ok(mobs)
}

fn position_in_area(area: BoundingBox, index: u32, map_size: u32, map_mask: &[u8]) -> Vec3 {
    for attempt in 0..256 {
        let candidate = position_in_area_candidate(area, index.wrapping_add(attempt));
        if is_walkable_in_mask(map_size, map_mask, candidate.x, candidate.z) {
            return candidate;
        }
    }

    position_in_area_candidate(area, index)
}

fn position_in_area_candidate(area: BoundingBox, index: u32) -> Vec3 {
    let column = index % 8;
    let row = index / 8;
    let x_ratio = (column as f32 + 0.5) / 8.0;
    let z_ratio = ((row % 8) as f32 + 0.5) / 8.0;

    Vec3 {
        x: area.lower.x + (area.upper.x - area.lower.x) * x_ratio,
        y: (area.lower.y + area.upper.y) * 0.5,
        z: area.lower.z + (area.upper.z - area.lower.z) * z_ratio,
    }
}

fn read_npcs(reader: &mut SvmapReader<'_>) -> Result<Vec<crate::world_entities::MapNpc>> {
    let npc_count = reader.read_count("npc")?;
    let mut npcs = Vec::new();

    for _ in 0..npc_count {
        let npc_type = reader.read_i32()?;
        let npc_id = reader.read_i32()?;
        let position_count = reader.read_count("npc position")?;
        let mut patrol_points = Vec::with_capacity(position_count);

        for _ in 0..position_count {
            let position = reader.read_vec3()?;
            let yaw = reader.read_f32()?;

            if !(0..=u8::MAX as i32).contains(&npc_type) || !(0..=u16::MAX as i32).contains(&npc_id)
            {
                continue;
            }

            patrol_points.push(NpcPatrolPoint {
                x: position.x,
                y: position.y,
                z: position.z,
                angle: yaw_to_angle(yaw),
            });
        }

        if let Some(npc) =
            build_static_npc_with_patrol(npc_type as u8, npc_id as u16, patrol_points)
        {
            npcs.push(npc);
        }
    }

    Ok(npcs)
}

fn yaw_to_angle(yaw: f32) -> u16 {
    if !yaw.is_finite() {
        return 0;
    }

    let normalized = yaw.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
    let angle = (normalized * 65536.0).round() as u16;
    angle.wrapping_add(32768)
}

fn read_portals(reader: &mut SvmapReader<'_>) -> Result<Vec<MapPortal>> {
    let count = reader.read_count("portal")?;
    let mut portals = Vec::new();

    for index in 0..count {
        let position = reader.read_vec3()?;
        let faction_or_portal_id = reader.read_i32()?;
        let min_level = u16::from_le_bytes(reader.read_exact()?);
        let max_level = u16::from_le_bytes(reader.read_exact()?);
        let target_map_id = reader.read_u32()?;
        let target_position = reader.read_vec3()?;

        if target_map_id > u16::MAX as u32 || index > u8::MAX as usize {
            continue;
        }

        portals.push(MapPortal {
            index: index as u8,
            portal_id: faction_or_portal_id,
            faction_or_portal_id,
            min_level,
            max_level,
            target_map_id: target_map_id as u16,
            pos_x: position.x,
            pos_y: position.y,
            pos_z: position.z,
            target_x: target_position.x,
            target_y: target_position.y,
            target_z: target_position.z,
        });
    }

    Ok(portals)
}

fn read_spawn_areas(reader: &mut SvmapReader<'_>) -> Result<Vec<MapSpawnArea>> {
    let count = reader.read_count("spawn area")?;
    let mut spawn_areas = Vec::new();

    for _ in 0..count {
        let _unknown1 = reader.read_i32()?;
        let faction = reader.read_i32()?;
        let _unknown2 = reader.read_i32()?;
        let area = reader.read_box()?;
        spawn_areas.push(MapSpawnArea {
            faction,
            lower_x: area.lower.x,
            lower_y: area.lower.y,
            lower_z: area.lower.z,
            upper_x: area.upper.x,
            upper_y: area.upper.y,
            upper_z: area.upper.z,
        });
    }

    Ok(spawn_areas)
}

fn skip_named_areas(reader: &mut SvmapReader<'_>) -> Result<()> {
    let count = reader.read_count("named area")?;
    reader.skip(count.saturating_mul(32))
}
