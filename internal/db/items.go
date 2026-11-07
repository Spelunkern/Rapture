package db

import (
	"crypto/rand"
	"database/sql"
	"encoding/binary"
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/Spelunkern/Rapture/internal/config"
)

// Item is one CharItems or UserStoredItems row; UID (ItemUID) is the key
// the original procedures use.
type Item struct {
	UID          int64
	Bag, Slot    uint8
	Type, TypeID uint8
	Quality      uint16
	Gems         [6]uint8
	Count        uint8
	CraftName    string
	MakeTime     time.Time
	MakeType     string
}

// ItemID is Type*1000+TypeID, the original encoding.
func (it *Item) ItemID() int32 { return int32(it.Type)*1000 + int32(it.TypeID) }

// NewItemUID returns a random positive 63-bit ItemUID.
func NewItemUID() int64 {
	var b [8]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic(err)
	}
	return int64(binary.LittleEndian.Uint64(b[:]) & 0x7FFF_FFFF_FFFF_FFFF)
}

func scanItems(rows *sql.Rows, stored bool) ([]Item, error) {
	defer rows.Close()
	var items []Item
	for rows.Next() {
		var it Item
		var itemID int32
		var quality int64
		var slot int64
		var craft, makeType sql.NullString
		var mk [6]sql.NullInt32
		dest := []any{&itemID, &it.Type, &it.TypeID, &it.UID}
		if !stored {
			dest = append(dest, &it.Bag)
		}
		dest = append(dest, &slot, &quality, &it.Gems[0], &it.Gems[1], &it.Gems[2], &it.Gems[3],
			&it.Gems[4], &it.Gems[5], &craft, &it.Count, &makeType, &mk[0], &mk[1], &mk[2], &mk[3], &mk[4], &mk[5])
		if err := rows.Scan(dest...); err != nil {
			return nil, err
		}
		it.Slot, it.Quality = uint8(min(max(slot, 0), 255)), clampU16(quality)
		it.CraftName, it.MakeType = strings.TrimRight(craft.String, " "), strings.TrimSpace(makeType.String)
		if mk[0].Valid {
			it.MakeTime = time.Date(int(mk[0].Int32), time.Month(mk[1].Int32), int(mk[2].Int32),
				int(mk[3].Int32), int(mk[4].Int32), int(mk[5].Int32), 0, time.Local)
		}
		if it.Count > 0 {
			items = append(items, it)
		}
	}
	return items, rows.Err()
}

// CharacterItems runs usp_Read_Char_Items_Detail_R.
func CharacterItems(charID uint32) ([]Item, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return nil, err
	}
	c, cancel := ctx()
	defer cancel()
	rows, err := pool.QueryContext(c, "dbo.usp_Read_Char_Items_Detail_R", sql.Named("CharID", int32(charID)))
	if err != nil {
		return nil, fmt.Errorf("usp_Read_Char_Items_Detail_R: %w", err)
	}
	return scanItems(rows, false)
}

func makeTime(it *Item) time.Time {
	if it.MakeTime.IsZero() {
		return time.Now()
	}
	return it.MakeTime
}

func makeType(it *Item) string {
	if it.MakeType == "" {
		return "S"
	}
	return it.MakeType[:1]
}

// AddCharacterItem runs usp_Save_Char_Item_Add_E; a zero UID gets a new one.
func AddCharacterItem(charID uint32, it *Item) error {
	if it.UID == 0 {
		it.UID = NewItemUID()
	}
	if it.MakeTime.IsZero() {
		it.MakeTime = time.Now()
	}
	return execProc(GameData, "dbo.usp_Save_Char_Item_Add_E", sql.Named("CharID", int32(charID)),
		sql.Named("ItemUID", it.UID), sql.Named("Bag", it.Bag), sql.Named("Slot", it.Slot),
		sql.Named("ItemID", it.ItemID()), sql.Named("Type", it.Type), sql.Named("TypeID", it.TypeID),
		sql.Named("Quality", int32(it.Quality)), sql.Named("Gem1", it.Gems[0]), sql.Named("Gem2", it.Gems[1]),
		sql.Named("Gem3", it.Gems[2]), sql.Named("Gem4", it.Gems[3]), sql.Named("Gem5", it.Gems[4]),
		sql.Named("Gem6", it.Gems[5]), sql.Named("Craftname", it.CraftName), sql.Named("Count", it.Count),
		sql.Named("Maketime", makeTime(it)), sql.Named("Maketype", makeType(it)))
}

// SaveCharacterItem writes bag, slot, quality, gems, count and craft name
// with usp_Save_Char_Item_Mod_E. A zero count deletes the item.
func SaveCharacterItem(charID uint32, it *Item) error {
	if it.Count == 0 {
		return DeleteCharacterItems(charID, it.UID)
	}
	return execProc(GameData, "dbo.usp_Save_Char_Item_Mod_E", sql.Named("CharID", int32(charID)),
		sql.Named("ItemUID", it.UID), sql.Named("Bag", it.Bag), sql.Named("Slot", it.Slot),
		sql.Named("Quality", int16(min(it.Quality, 32767))), sql.Named("Gem1", it.Gems[0]),
		sql.Named("Gem2", it.Gems[1]), sql.Named("Gem3", it.Gems[2]), sql.Named("Gem4", it.Gems[3]),
		sql.Named("Gem5", it.Gems[4]), sql.Named("Gem6", it.Gems[5]), sql.Named("Craftname", it.CraftName),
		sql.Named("Count", it.Count))
}

func idList(uids []int64) string {
	parts := make([]string, len(uids))
	for i, uid := range uids {
		parts[i] = strconv.FormatInt(uid, 10)
	}
	return strings.Join(parts, ",")
}

// DeleteCharacterItems runs usp_Save_Char_Item_Del_E for the given UIDs.
func DeleteCharacterItems(charID uint32, uids ...int64) error {
	if len(uids) == 0 {
		return nil
	}
	return execProc(GameData, "dbo.usp_Save_Char_Item_Del_E", sql.Named("CharID", int32(charID)),
		sql.Named("IDList", idList(uids)))
}

// StoredItems runs usp_Read_User_StoredItems_Detail_R (account warehouse).
func StoredItems(userUID uint32) ([]Item, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return nil, err
	}
	c, cancel := ctx()
	defer cancel()
	rows, err := pool.QueryContext(c, "dbo.usp_Read_User_StoredItems_Detail_R",
		sql.Named("ServerID", config.ServerID), sql.Named("UserUID", int32(userUID)))
	if err != nil {
		return nil, fmt.Errorf("usp_Read_User_StoredItems_Detail_R: %w", err)
	}
	return scanItems(rows, true)
}

// AddStoredItem runs usp_Save_User_StoredItem_Add_E.
func AddStoredItem(userUID uint32, it *Item) error {
	if it.UID == 0 {
		it.UID = NewItemUID()
	}
	return execProc(GameData, "dbo.usp_Save_User_StoredItem_Add_E", sql.Named("ServerID", config.ServerID),
		sql.Named("UserUID", int32(userUID)), sql.Named("ItemUID", it.UID), sql.Named("Slot", it.Slot),
		sql.Named("ItemID", it.ItemID()), sql.Named("Type", it.Type), sql.Named("TypeID", it.TypeID),
		sql.Named("Quality", int16(min(it.Quality, 32767))), sql.Named("Gem1", it.Gems[0]),
		sql.Named("Gem2", it.Gems[1]), sql.Named("Gem3", it.Gems[2]), sql.Named("Gem4", it.Gems[3]),
		sql.Named("Gem5", it.Gems[4]), sql.Named("Gem6", it.Gems[5]), sql.Named("Craftname", it.CraftName),
		sql.Named("Count", it.Count), sql.Named("Maketime", makeTime(it)), sql.Named("Maketype", makeType(it)))
}

// SaveStoredItem runs usp_Save_User_StoredItem_Mod_E; zero count deletes.
func SaveStoredItem(userUID uint32, it *Item) error {
	if it.Count == 0 {
		return DeleteStoredItems(userUID, it.UID)
	}
	return execProc(GameData, "dbo.usp_Save_User_StoredItem_Mod_E", sql.Named("ServerID", config.ServerID),
		sql.Named("UserUID", int32(userUID)), sql.Named("ItemUID", it.UID), sql.Named("Slot", it.Slot),
		sql.Named("Quality", int16(min(it.Quality, 32767))), sql.Named("Gem1", it.Gems[0]),
		sql.Named("Gem2", it.Gems[1]), sql.Named("Gem3", it.Gems[2]), sql.Named("Gem4", it.Gems[3]),
		sql.Named("Gem5", it.Gems[4]), sql.Named("Gem6", it.Gems[5]), sql.Named("Count", it.Count),
		sql.Named("Craftname", it.CraftName))
}

// DeleteStoredItems runs usp_Save_User_StoredItem_Del_E.
func DeleteStoredItems(userUID uint32, uids ...int64) error {
	if len(uids) == 0 {
		return nil
	}
	return execProc(GameData, "dbo.usp_Save_User_StoredItem_Del_E", sql.Named("ServerID", config.ServerID),
		sql.Named("UserUID", int32(userUID)), sql.Named("IDList", idList(uids)))
}

// GuildStoredItems runs usp_Read_Guild_StoredItems_Detail_R.
func GuildStoredItems(guildID uint32) ([]Item, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return nil, err
	}
	c, cancel := ctx()
	defer cancel()
	rows, err := pool.QueryContext(c, "dbo.usp_Read_Guild_StoredItems_Detail_R", sql.Named("GuildID", int32(guildID)))
	if err != nil {
		return nil, fmt.Errorf("usp_Read_Guild_StoredItems_Detail_R: %w", err)
	}
	return scanItems(rows, true)
}

// AddGuildStoredItem runs usp_Save_Guild_StoredItem_Add_E.
func AddGuildStoredItem(guildID uint32, it *Item) error {
	if it.UID == 0 {
		it.UID = NewItemUID()
	}
	return execProc(GameData, "dbo.usp_Save_Guild_StoredItem_Add_E", sql.Named("GuildID", int32(guildID)),
		sql.Named("ItemUID", it.UID), sql.Named("Slot", it.Slot), sql.Named("ItemID", it.ItemID()),
		sql.Named("Type", it.Type), sql.Named("TypeID", it.TypeID), sql.Named("Quality", int16(min(it.Quality, 32767))),
		sql.Named("Gem1", it.Gems[0]), sql.Named("Gem2", it.Gems[1]), sql.Named("Gem3", it.Gems[2]),
		sql.Named("Gem4", it.Gems[3]), sql.Named("Gem5", it.Gems[4]), sql.Named("Gem6", it.Gems[5]),
		sql.Named("Craftname", it.CraftName), sql.Named("Count", it.Count), sql.Named("Maketime", makeTime(it)),
		sql.Named("Maketype", makeType(it)))
}

// SaveGuildStoredItem runs usp_Save_Guild_StoredItem_Mod_E.
func SaveGuildStoredItem(guildID uint32, it *Item) error {
	return execProc(GameData, "dbo.usp_Save_Guild_StoredItem_Mod_E", sql.Named("GuildID", int32(guildID)),
		sql.Named("ItemUID", it.UID), sql.Named("Slot", it.Slot), sql.Named("Quality", int16(min(it.Quality, 32767))),
		sql.Named("Gem1", it.Gems[0]), sql.Named("Gem2", it.Gems[1]), sql.Named("Gem3", it.Gems[2]),
		sql.Named("Gem4", it.Gems[3]), sql.Named("Gem5", it.Gems[4]), sql.Named("Gem6", it.Gems[5]),
		sql.Named("Count", it.Count), sql.Named("Craftname", it.CraftName))
}

// DeleteGuildStoredItems runs usp_Save_Guild_StoredItem_Del_E.
func DeleteGuildStoredItems(guildID uint32, uids ...int64) error {
	if len(uids) == 0 {
		return nil
	}
	return execProc(GameData, "dbo.usp_Save_Guild_StoredItem_Del_E", sql.Named("GuildID", int32(guildID)),
		sql.Named("IDList", idList(uids)))
}
