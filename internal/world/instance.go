package world

import (
	"fmt"
	"path/filepath"
	"sync"

	"github.com/Spelunkern/Rapture/internal/db"
)

// Instanced zones (Map.ini CreateType R guild ranking, H guild house, P
// party) are copies of a map with their own mobs, NPCs and players
// (CWorld::GetZoneByMapID / CZone). Rapture keys the world by a uint16 zone
// id: real maps keep their map id, instances get a virtual id from
// instanceZoneBase up that aliases the real map for its data. The client and
// the database only ever see the real map (realMap).

const instanceZoneBase = 1000

var (
	instanceMu    sync.Mutex
	instanceMaps  = map[uint16]uint16{} // zone -> real map
	nextInstance  = uint16(instanceZoneBase)
	instanceOwner = map[uint16]uint32{} // zone -> CZone m_dwCreateOwnerID
)

// realMap is the map a zone shows (itself for real maps).
func realMap(zone uint16) uint16 {
	if zone < instanceZoneBase {
		return zone
	}
	instanceMu.Lock()
	defer instanceMu.Unlock()
	if m, ok := instanceMaps[zone]; ok {
		return m
	}
	return zone
}

func isInstanceZone(zone uint16) bool { return zone >= instanceZoneBase }

func init() { db.MapForSave = realMap }

// mapCreateType is the Map.ini CreateType of a zone's map ("" if none).
func mapCreateType(zone uint16) string {
	if z, ok := mapConfig.Zones[realMap(zone)]; ok {
		return z.CreateType
	}
	return ""
}

// createInstance builds a fresh copy of mapID from its svmap.
func createInstance(mapID uint16, owner uint32) (uint16, error) {
	load, err := loadSvmapFile(mapID, filepath.Join(mapDataDir(), fmt.Sprintf("%d.svmap", mapID)), db.MustDefs())
	if err != nil {
		return 0, err
	}
	instanceMu.Lock()
	zone := nextInstance
	for {
		if _, used := instanceMaps[zone]; !used {
			break
		}
		zone++
		if zone < instanceZoneBase {
			zone = instanceZoneBase
		}
	}
	nextInstance = zone + 1
	if nextInstance < instanceZoneBase {
		nextInstance = instanceZoneBase
	}
	instanceMaps[zone], instanceOwner[zone] = mapID, owner
	instanceMu.Unlock()
	RegisterMapGrid(zone, load.mapSize, load.mask)
	AddEntities(zone, load.entities)
	return zone, nil
}

// destroyInstance drops an (empty) instance's entities and grid.
func destroyInstance(zone uint16) {
	if !isInstanceZone(zone) {
		return
	}
	instanceMu.Lock()
	delete(instanceMaps, zone)
	delete(instanceOwner, zone)
	instanceMu.Unlock()
	shardsMu.Lock()
	delete(shards, zone)
	shardsMu.Unlock()
	gridsMu.Lock()
	delete(grids, zone)
	gridsMu.Unlock()
	clearMapItems(zone)
}

// instanceZones lists the live instances of mapID.
func instanceZones(mapID uint16) []uint16 {
	instanceMu.Lock()
	defer instanceMu.Unlock()
	var out []uint16
	for z, m := range instanceMaps {
		if m == mapID {
			out = append(out, z)
		}
	}
	return out
}
