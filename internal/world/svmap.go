package world

import (
	"encoding/binary"
	"errors"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/Spelunkern/Rapture/internal/config"
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
)

// .svmap is the original server map file: walkability mask, ladders,
// monster areas, NPCs, portals, spawn areas and named areas.

type svmapReader struct {
	data []byte
	pos  int
}

var errShortSvmap = errors.New("unexpected end of svmap")

func (r *svmapReader) take(n int) ([]byte, error) {
	if n < 0 || r.pos+n > len(r.data) {
		return nil, fmt.Errorf("%w at offset %d reading %d bytes", errShortSvmap, r.pos, n)
	}
	b := r.data[r.pos : r.pos+n]
	r.pos += n
	return b, nil
}

func (r *svmapReader) i32() (int32, error) {
	b, err := r.take(4)
	if err != nil {
		return 0, err
	}
	return int32(binary.LittleEndian.Uint32(b)), nil
}

func (r *svmapReader) u32() (uint32, error) {
	v, err := r.i32()
	return uint32(v), err
}

func (r *svmapReader) u16() (uint16, error) {
	b, err := r.take(2)
	if err != nil {
		return 0, err
	}
	return binary.LittleEndian.Uint16(b), nil
}

func (r *svmapReader) f32() (float32, error) {
	v, err := r.u32()
	return math.Float32frombits(v), err
}

type vec3 struct{ x, y, z float32 }

func (r *svmapReader) vec3() (vec3, error) {
	var v vec3
	var err error
	if v.x, err = r.f32(); err != nil {
		return v, err
	}
	if v.y, err = r.f32(); err != nil {
		return v, err
	}
	v.z, err = r.f32()
	return v, err
}

func (r *svmapReader) count(label string) (int, error) {
	n, err := r.i32()
	if err != nil {
		return 0, err
	}
	if n < 0 {
		return 0, fmt.Errorf("negative %s count in svmap: %d", label, n)
	}
	return int(n), nil
}

type svmapLoad struct {
	mapSize  uint32
	cellSize uint32
	mask     []byte
	entities MapEntities
}

func mapDataDir() string { return filepath.Join(config.DataDir(), "maps") }

func ensureMapDataDir() error {
	dir := mapDataDir()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	readme := filepath.Join(dir, "README.txt")
	if _, err := os.Stat(readme); os.IsNotExist(err) {
		return os.WriteFile(readme, []byte("Place original Shaiya .svmap files here using the map id as filename, for example 1.svmap or 42.svmap.\r\nRapture loads NPCs and monster spawn areas from these files when rapture_world starts.\r\n"), 0o644)
	}
	return nil
}

// LoadSvmaps loads every data/maps/<id>.svmap into the world.
func LoadSvmaps(log *logx.Logger) error {
	if err := ensureMapDataDir(); err != nil {
		return err
	}
	entries, err := os.ReadDir(mapDataDir())
	if err != nil {
		return err
	}
	defs := db.MustDefs()
	maps, npcs, mobs := 0, 0, 0
	for _, entry := range entries {
		name := entry.Name()
		if !strings.EqualFold(filepath.Ext(name), ".svmap") {
			continue
		}
		id, err := strconv.ParseUint(strings.TrimSuffix(name, filepath.Ext(name)), 10, 16)
		if err != nil {
			log.Printf("Rapture World skipped svmap with non-numeric filename: %s", name)
			continue
		}
		load, err := loadSvmapFile(uint16(id), filepath.Join(mapDataDir(), name), defs)
		if err != nil {
			log.Printf("Rapture World failed to load svmap %s: %v", name, err)
			continue
		}
		RegisterMapGrid(uint16(id), load.mapSize, load.mask)
		AddEntities(uint16(id), load.entities)
		maps++
		npcs += len(load.entities.NPCs)
		mobs += len(load.entities.Mobs)
		log.Printf("Rapture World loaded svmap map=%d map_size=%d cell_size=%d npcs=%d mobs=%d portals=%d spawn_areas=%d",
			id, load.mapSize, load.cellSize, len(load.entities.NPCs), len(load.entities.Mobs),
			len(load.entities.Portals), len(load.entities.SpawnAreas))
	}
	log.Printf("Rapture World svmap load complete maps=%d npcs=%d mobs=%d", maps, npcs, mobs)
	return nil
}

func loadSvmapFile(mapID uint16, path string, defs *db.Defs) (svmapLoad, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return svmapLoad{}, err
	}
	r := &svmapReader{data: data}
	size, err := r.i32()
	if err != nil {
		return svmapLoad{}, err
	}
	if size <= 0 {
		return svmapLoad{}, fmt.Errorf("invalid map size %d", size)
	}
	mask, err := r.take(int(size) * int(size) / 8)
	if err != nil {
		return svmapLoad{}, err
	}
	cellSize, err := r.i32()
	if err != nil {
		return svmapLoad{}, err
	}
	if cellSize < 0 {
		return svmapLoad{}, fmt.Errorf("invalid cell size %d", cellSize)
	}
	load := svmapLoad{mapSize: uint32(size), cellSize: uint32(cellSize), mask: append([]byte(nil), mask...)}
	ladders, err := r.count("ladder")
	if err != nil {
		return load, err
	}
	if _, err := r.take(ladders * 12); err != nil {
		return load, err
	}
	if load.entities.Mobs, err = readMonsterAreas(r, load.mapSize, load.mask, defs); err != nil {
		return load, err
	}
	if load.entities.NPCs, err = readNpcs(r); err != nil {
		return load, err
	}
	if load.entities.Portals, err = readPortals(r); err != nil {
		return load, err
	}
	if load.entities.SpawnAreas, err = readSpawnAreas(r); err != nil {
		return load, err
	}
	named, err := r.count("named area")
	if err != nil {
		return load, err
	}
	_, err = r.take(named * 32)
	return load, err
}

func readMonsterAreas(r *svmapReader, mapSize uint32, mask []byte, defs *db.Defs) ([]MapMob, error) {
	areas, err := r.count("monster area")
	if err != nil {
		return nil, err
	}
	var mobs []MapMob
	for range areas {
		lower, err := r.vec3()
		if err != nil {
			return nil, err
		}
		upper, err := r.vec3()
		if err != nil {
			return nil, err
		}
		area := MobArea{lower.x, lower.z, upper.x, upper.z}.normalized()
		spawns, err := r.count("monster spawn")
		if err != nil {
			return nil, err
		}
		index := uint32(0)
		for range spawns {
			mobID, err := r.u32()
			if err != nil {
				return nil, err
			}
			count, err := r.u32()
			if err != nil {
				return nil, err
			}
			if mobID > math.MaxUint16 {
				continue
			}
			def := defs.Mobs[uint16(mobID)]
			var respawn uint64
			if def != nil {
				respawn = def.RespawnSeconds
			}
			// stMobRespawn: one respawn group per area entry.
			g := &mobGroup{count: int(count), alive: int(count)}
			for range count {
				p := positionInArea(lower, upper, index, mapSize, mask)
				a := area
				m := NewStaticMob(def, uint16(mobID), p.x, p.z, &a, respawn)
				m.group = g
				mobs = append(mobs, m)
				index++
			}
		}
	}
	return mobs, nil
}

func positionInArea(lower, upper vec3, index, mapSize uint32, mask []byte) vec3 {
	for attempt := uint32(0); attempt < 256; attempt++ {
		c := areaCandidate(lower, upper, index+attempt)
		if walkableInMask(mapSize, mask, c.x, c.z) {
			return c
		}
	}
	return areaCandidate(lower, upper, index)
}

func areaCandidate(lower, upper vec3, index uint32) vec3 {
	xr := (float32(index%8) + 0.5) / 8
	zr := (float32((index/8)%8) + 0.5) / 8
	return vec3{lower.x + (upper.x-lower.x)*xr, (lower.y + upper.y) * 0.5, lower.z + (upper.z-lower.z)*zr}
}

func readNpcs(r *svmapReader) ([]MapNpc, error) {
	n, err := r.count("npc")
	if err != nil {
		return nil, err
	}
	var npcs []MapNpc
	for range n {
		npcType, err := r.i32()
		if err != nil {
			return nil, err
		}
		npcID, err := r.i32()
		if err != nil {
			return nil, err
		}
		points, err := r.count("npc position")
		if err != nil {
			return nil, err
		}
		valid := npcType >= 0 && npcType <= 255 && npcID >= 0 && npcID <= 65535
		var patrol []NpcPatrolPoint
		for range points {
			p, err := r.vec3()
			if err != nil {
				return nil, err
			}
			yaw, err := r.f32()
			if err != nil {
				return nil, err
			}
			if valid {
				patrol = append(patrol, NpcPatrolPoint{p.x, p.y, p.z, yawToAngle(yaw)})
			}
		}
		if valid {
			if npc, ok := NewPatrolNpc(uint8(npcType), uint16(npcID), patrol); ok {
				npcs = append(npcs, npc)
			}
		}
	}
	return npcs, nil
}

// yawToAngle is the client's conversion: degrees = -yaw*180/pi in [0, 360).
func yawToAngle(yaw float32) uint16 {
	if math.IsNaN(float64(yaw)) || math.IsInf(float64(yaw), 0) {
		return 0
	}
	degrees := 0.0 - float64(yaw)*57.29578
	if degrees < 0 {
		degrees += 360
	}
	return uint16(degrees)
}

func readPortals(r *svmapReader) ([]MapPortal, error) {
	n, err := r.count("portal")
	if err != nil {
		return nil, err
	}
	var portals []MapPortal
	for index := range n {
		pos, err := r.vec3()
		if err != nil {
			return nil, err
		}
		faction, err := r.i32()
		if err != nil {
			return nil, err
		}
		minLevel, err := r.u16()
		if err != nil {
			return nil, err
		}
		maxLevel, err := r.u16()
		if err != nil {
			return nil, err
		}
		target, err := r.u32()
		if err != nil {
			return nil, err
		}
		tpos, err := r.vec3()
		if err != nil {
			return nil, err
		}
		if target > math.MaxUint16 || index > 255 {
			continue
		}
		portals = append(portals, MapPortal{Index: uint8(index), PortalID: faction, FactionOrPortalID: faction,
			MinLevel: minLevel, MaxLevel: maxLevel, TargetMapID: uint16(target), PosX: pos.x, PosY: pos.y,
			PosZ: pos.z, TargetX: tpos.x, TargetY: tpos.y, TargetZ: tpos.z})
	}
	return portals, nil
}

func readSpawnAreas(r *svmapReader) ([]MapSpawnArea, error) {
	n, err := r.count("spawn area")
	if err != nil {
		return nil, err
	}
	var areas []MapSpawnArea
	for range n {
		if _, err := r.i32(); err != nil {
			return nil, err
		}
		faction, err := r.i32()
		if err != nil {
			return nil, err
		}
		if _, err := r.i32(); err != nil {
			return nil, err
		}
		lower, err := r.vec3()
		if err != nil {
			return nil, err
		}
		upper, err := r.vec3()
		if err != nil {
			return nil, err
		}
		areas = append(areas, MapSpawnArea{faction, lower.x, lower.y, lower.z, upper.x, upper.y, upper.z})
	}
	return areas, nil
}
