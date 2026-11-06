package world

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"

	"github.com/Spelunkern/Rapture/internal/config"
	"github.com/Spelunkern/Rapture/internal/logx"
)

// The original PS_Game configuration (data/game/, copied from the EP4.5
// server's PS_Game/Data): Map.ini (start positions, map types, rebirth
// points) and ItemEnchant.ini (lapisia success rates and bonuses).

func gameDataDir() string { return filepath.Join(config.DataDir(), "game") }

type iniFile map[string]map[string]string

var blockComment = regexp.MustCompile(`(?s)/\*.*?\*/`)

// readIni parses the original server's ini dialect: [SECTION], key = value,
// with ;, // and /* */ comments. Section and key names are upper-cased.
func readIni(path string) (iniFile, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	text := blockComment.ReplaceAllString(string(raw), "")
	out := iniFile{}
	section := ""
	for _, line := range strings.Split(text, "\n") {
		if i := strings.Index(line, "//"); i >= 0 {
			line = line[:i]
		}
		if i := strings.Index(line, ";"); i >= 0 {
			line = line[:i]
		}
		line = strings.TrimSpace(line)
		switch {
		case line == "":
		case strings.HasPrefix(line, "[") && strings.HasSuffix(line, "]"):
			section = strings.ToUpper(strings.TrimSpace(line[1 : len(line)-1]))
			if out[section] == nil {
				out[section] = map[string]string{}
			}
		default:
			k, v, ok := strings.Cut(line, "=")
			if ok && section != "" {
				out[section][strings.ToUpper(strings.TrimSpace(k))] = strings.TrimSpace(v)
			}
		}
	}
	return out, nil
}

// numbers extracts every number in s ("(1, 577.98, 78.56, 1762.12)").
var numberRE = regexp.MustCompile(`-?\d+(\.\d+)?`)

func numbers(s string) []float64 {
	var out []float64
	for _, m := range numberRE.FindAllString(s, -1) {
		if v, err := strconv.ParseFloat(m, 64); err == nil {
			out = append(out, v)
		}
	}
	return out
}

// MapPos is a map and position from the original configs.
type MapPos struct {
	MapID   uint16
	X, Y, Z float32
}

// parseMapPos reads "map, x, y, z"; map -1 or all-zero coordinates mean
// "not set", as the original Map.ini documents.
func parseMapPos(s string) (MapPos, bool) {
	n := numbers(s)
	if len(n) < 4 || n[0] < 0 || (n[1] == 0 && n[2] == 0 && n[3] == 0) {
		return MapPos{}, false
	}
	return MapPos{uint16(n[0]), float32(n[1]), float32(n[2]), float32(n[3])}, true
}

// ZoneConfig is one Map.ini [SET_ZONE_n] entry.
type ZoneConfig struct {
	Dungeon bool // MapType D (F is a field)
	// Rebirth applies to every faction; RebirthLight/RebirthFury
	// (RebirthMapPos1/2) are the per-faction alternatives.
	Rebirth, RebirthLight, RebirthFury *MapPos
	WarType                            string
	CreateType                         string // instanced maps (R, H, P)
}

// MapConfig is the parsed Map.ini.
type MapConfig struct {
	// Start is the new-character position per race (Chars.Family): human,
	// elf, vail, deatheater.
	Start [4]MapPos
	Zones map[uint16]ZoneConfig
}

var mapConfig = MapConfig{
	Start: [4]MapPos{{1, 577.98, 78.56, 1762.12}, {1, 1678.92, 38.92, 1776.91},
		{2, 165.25, 118.50, 397.89}, {2, 1838.93, 129.33, 443.43}},
	Zones: map[uint16]ZoneConfig{},
}

var enchantConfig = struct {
	WeaponRate, DefenseRate [enchantMaxLevel + 1]uint32 // per million, step N -> N+1
}{}

func loadMapIni(log *logx.Logger) error {
	ini, err := readIni(filepath.Join(gameDataDir(), "Map.ini"))
	if err != nil {
		return err
	}
	for i, key := range []string{"HUGAMESTARTMAPPOS", "ELGAMESTARTMAPPOS", "BAGAMESTARTMAPPOS", "DEGAMESTARTMAPPOS"} {
		if p, ok := parseMapPos(ini["BASE"][key]); ok {
			mapConfig.Start[i] = p
		}
	}
	defaults := ini["DEFAULT_SET_ZONE"]
	get := func(sec map[string]string, k string) string {
		if v, ok := sec[k]; ok {
			return v
		}
		return defaults[k]
	}
	for name, sec := range ini {
		if !strings.HasPrefix(name, "SET_ZONE_") {
			continue
		}
		id, err := strconv.Atoi(strings.TrimPrefix(name, "SET_ZONE_"))
		if err != nil || id < 0 || id > 65535 {
			continue
		}
		z := ZoneConfig{Dungeon: strings.EqualFold(get(sec, "MAPTYPE"), "D"), WarType: get(sec, "WARTYPE"),
			CreateType: get(sec, "CREATETYPE")}
		for key, dst := range map[string]**MapPos{"REBIRTHMAPPOS": &z.Rebirth, "REBIRTHMAPPOS1": &z.RebirthLight, "REBIRTHMAPPOS2": &z.RebirthFury} {
			if p, ok := parseMapPos(get(sec, key)); ok {
				*dst = &p
			}
		}
		mapConfig.Zones[uint16(id)] = z
	}
	log.Printf("Rapture World loaded Map.ini zones=%d start=%v", len(mapConfig.Zones), mapConfig.Start)
	return nil
}

func loadItemEnchantIni(log *logx.Logger) error {
	ini, err := readIni(filepath.Join(gameDataDir(), "ItemEnchant.ini"))
	if err != nil {
		return err
	}
	rates := ini["LAPISIANENCHANTPERCENTRATE"]
	for step := 0; step <= enchantMaxLevel; step++ {
		for prefix, dst := range map[string]*[enchantMaxLevel + 1]uint32{"WEAPON": &enchantConfig.WeaponRate, "DEFENSE": &enchantConfig.DefenseRate} {
			v, ok := rates[fmt.Sprintf("%sSTEP%02d", prefix, step)]
			if !ok {
				return fmt.Errorf("ItemEnchant.ini: missing %sStep%02d", prefix, step)
			}
			n, err := strconv.ParseUint(strings.TrimSpace(v), 10, 32)
			if err != nil {
				return fmt.Errorf("ItemEnchant.ini: %sStep%02d: %w", prefix, step, err)
			}
			dst[step] = uint32(n)
		}
	}
	add := ini["LAPISIANENCHANTADDVALUE"]
	for step := 0; step <= enchantMaxLevel; step++ {
		if v, err := strconv.Atoi(add[fmt.Sprintf("WEAPONSTEP%02d", step)]); err == nil {
			weaponEnchantAttackBonus[step] = uint32(max(v, 0))
		}
		if v, err := strconv.Atoi(add[fmt.Sprintf("DEFENSESTEP%02d", step)]); err == nil {
			armorEnchantAbsorption[step] = uint32(max(v, 0))
		}
	}
	log.Printf("Rapture World loaded ItemEnchant.ini weapon=%v defense=%v", enchantConfig.WeaponRate, enchantConfig.DefenseRate)
	return nil
}

// LoadGameConfig loads the original PS_Game ini files from data/game/.
func LoadGameConfig(log *logx.Logger) error {
	if err := loadMapIni(log); err != nil {
		return fmt.Errorf("Map.ini: %w", err)
	}
	if err := loadItemEnchantIni(log); err != nil {
		return fmt.Errorf("ItemEnchant.ini: %w", err)
	}
	if err := loadSequenceItems(log); err != nil {
		return fmt.Errorf("SequenceItemCreate.ini: %w", err)
	}
	buildDropCatalog()
	if err := loadNpcQuest(log); err != nil {
		return fmt.Errorf("NpcQuest.SData: %w", err)
	}
	return nil
}

// rebirthPosition is where a character of faction revives on mapID, per
// Map.ini; ok is false where the map defines none (fields use the
// faction's nearest svmap spawn area).
func rebirthPosition(mapID uint16, faction uint8) (MapPos, bool) {
	z, found := mapConfig.Zones[mapID]
	if !found {
		return MapPos{}, false
	}
	switch {
	case faction == FactionLight && z.RebirthLight != nil:
		return *z.RebirthLight, true
	case faction == FactionDark && z.RebirthFury != nil:
		return *z.RebirthFury, true
	case z.Rebirth != nil:
		return *z.Rebirth, true
	}
	return MapPos{}, false
}
