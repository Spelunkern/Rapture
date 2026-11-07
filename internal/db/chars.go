package db

import (
	"database/sql"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/Spelunkern/Rapture/internal/config"
	mssql "github.com/microsoft/go-mssqldb"
)

// Character mirrors PS_GameData.dbo.Chars. CurrentHP/MP/SP are base values
// (see world.CharacterStats); Chars.HP/MP/SP store them.
type Character struct {
	ID, AccountID                     uint32
	Slot                              uint8
	Name                              string
	Level                             uint16
	MapID                             uint16
	PosX, PosY, PosZ                  float32
	Dir                               int16
	Race, Mode                        uint8 // Family, Grow
	Hair, Face, Height                uint8
	Class, Gender                     uint8 // Job, Sex
	Str, Rec, Int, Wis, Dex, Luc      uint16
	StatPoint, SkillPoint             uint16
	MaxHP, MaxMP, MaxSP               uint32
	CurrentHP, CurrentMP, CurrentSP   uint32
	Exp, Money                        uint32
	Kills, Deaths, Victories, Defeats uint32
	KillLevel, DeadLevel              uint8
	Hg, Vg                            int16
	Cg, Og, Ig                        uint8
	PendingDelete, RenamePending      bool
}

// NewCharacter is a character to create: the client's request plus the
// starting stats, pools and position the world server derives for it.
type NewCharacter struct {
	Slot, Race, Mode, Hair, Face, Height, Class, Gender uint8
	Name                                                string
	MapID                                               uint16
	PosX, PosY, PosZ                                    float32
	Str, Dex, Rec, Int, Wis, Luc                        uint16
	HP, MP, SP                                          uint16
}

// DeleteWindow is how long a deleted character stays restorable.
const DeleteWindow = 2 * time.Hour

// MoneyCap keeps gold within the signed Money column's bit pattern.
const MoneyCap = 4_000_000_000

func u16(v int64) uint16 { return clampU16(v) }

// CharactersForAccount lists live characters (usp_Read_Chars_R) plus those
// deleted within DeleteWindow, flagged PendingDelete.
func CharactersForAccount(userUID uint32) ([]Character, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return nil, err
	}
	c, cancel := ctx()
	defer cancel()
	rows, err := pool.QueryContext(c, "dbo.usp_Read_Chars_R",
		sql.Named("ServerID", config.ServerID), sql.Named("UserUID", int32(userUID)), sql.Named("Del", 0))
	if err != nil {
		return nil, fmt.Errorf("usp_Read_Chars_R: %w", err)
	}
	var chars []Character
	for rows.Next() {
		var ch Character
		var level, str, dex, rec, intel, luc, wis, hp, mp, sp, mapID int64
		var renameCnt uint8
		var remain int32
		var mk [6]int32
		if err := rows.Scan(&ch.ID, &ch.Name, &ch.Slot, &ch.Race, &ch.Mode, &ch.Hair, &ch.Face, &ch.Height,
			&ch.Class, &ch.Gender, &level, &str, &dex, &rec, &intel, &luc, &wis, &hp, &mp, &sp, &mapID,
			&renameCnt, &remain, &mk[0], &mk[1], &mk[2], &mk[3], &mk[4], &mk[5]); err != nil {
			rows.Close()
			return nil, err
		}
		ch.AccountID = userUID
		ch.Name = strings.TrimRight(ch.Name, " ")
		ch.Level, ch.MapID = max(u16(level), 1), max(u16(mapID), 1)
		ch.Str, ch.Dex, ch.Rec, ch.Int, ch.Luc, ch.Wis = u16(str), u16(dex), u16(rec), u16(intel), u16(luc), u16(wis)
		ch.MaxHP, ch.MaxMP, ch.MaxSP = uint32(max(hp, 0)), uint32(max(mp, 0)), uint32(max(sp, 0))
		ch.CurrentHP, ch.CurrentMP, ch.CurrentSP = ch.MaxHP, ch.MaxMP, ch.MaxSP
		ch.RenamePending = renameCnt > 0
		chars = append(chars, ch)
	}
	rows.Close()
	// No original procedure lists recently deleted characters with their
	// DeleteDate, so the restore window is read directly.
	pending, err := pool.QueryContext(c, `SELECT CharID, CharName, Slot, Family, Grow, Hair, Face, Size, Job, Sex,
		Level, Str, Dex, Rec, [Int], Luc, Wis, HP, MP, SP, Map, RenameCnt FROM dbo.Chars
		WHERE ServerID = @p1 AND UserUID = @p2 AND Del = 1 AND DeleteDate >= DATEADD(hour, -2, GETDATE())`,
		config.ServerID, int32(userUID))
	if err != nil {
		return nil, err
	}
	defer pending.Close()
	for pending.Next() {
		var ch Character
		var level, str, dex, rec, intel, luc, wis, hp, mp, sp, mapID int64
		var renameCnt uint8
		if err := pending.Scan(&ch.ID, &ch.Name, &ch.Slot, &ch.Race, &ch.Mode, &ch.Hair, &ch.Face, &ch.Height,
			&ch.Class, &ch.Gender, &level, &str, &dex, &rec, &intel, &luc, &wis, &hp, &mp, &sp, &mapID, &renameCnt); err != nil {
			return nil, err
		}
		ch.AccountID = userUID
		ch.Name = strings.TrimRight(ch.Name, " ")
		ch.Level, ch.MapID = max(u16(level), 1), max(u16(mapID), 1)
		ch.Str, ch.Dex, ch.Rec, ch.Int, ch.Luc, ch.Wis = u16(str), u16(dex), u16(rec), u16(intel), u16(luc), u16(wis)
		ch.MaxHP, ch.MaxMP, ch.MaxSP = uint32(max(hp, 0)), uint32(max(mp, 0)), uint32(max(sp, 0))
		ch.PendingDelete, ch.RenamePending = true, renameCnt > 0
		chars = append(chars, ch)
	}
	return chars, nil
}

// CharacterDetail loads a playable character with
// usp_Read_Chars_Detail2_R (which also stamps JoinDate). It returns
// ErrNotFound unless the character belongs to userUID and is not deleted.
func CharacterDetail(userUID, charID uint32) (Character, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return Character{}, err
	}
	c, cancel := ctx()
	defer cancel()
	var ch Character
	var owner int32
	var level, statPoint, skillPoint, str, dex, rec, intel, luc, wis, hp, mp, sp, mapID, dir int64
	var exp, money, k1, k2, k3, k4 int32
	var guildID sql.NullInt64
	var storedMoney sql.NullInt64
	var hg, vg int16
	var cg, og, ig, del uint8
	var guildLevel, killLevel, deadLevel sql.NullInt64
	var event1, event2 sql.NullInt64
	var leave [6]sql.NullInt32
	var posX, posY, posZ float32
	err = pool.QueryRowContext(c, "dbo.usp_Read_Chars_Detail2_R", sql.Named("CharID", int32(charID))).Scan(
		&owner, &ch.ID, &ch.Name, &ch.Slot, &ch.Race, &ch.Mode, &ch.Hair, &ch.Face, &ch.Height, &ch.Class,
		&ch.Gender, &level, &statPoint, &skillPoint, &str, &dex, &rec, &intel, &luc, &wis, &hp, &mp, &sp,
		&mapID, &dir, &exp, &money, &posX, &posY, &posZ, &hg, &vg, &cg, &og, &ig, &del, &k1, &k2, &k3, &k4,
		&storedMoney, &guildID, &guildLevel, &killLevel, &deadLevel, &event1, &event2,
		&leave[0], &leave[1], &leave[2], &leave[3], &leave[4], &leave[5])
	if errors.Is(err, sql.ErrNoRows) || (err == nil && (uint32(owner) != userUID || del != 0)) {
		return Character{}, ErrNotFound
	}
	if err != nil {
		return Character{}, fmt.Errorf("usp_Read_Chars_Detail2_R: %w", err)
	}
	ch.AccountID = uint32(owner)
	ch.Name = strings.TrimRight(ch.Name, " ")
	ch.Level, ch.MapID, ch.Dir = max(u16(level), 1), max(u16(mapID), 1), int16(dir)
	ch.StatPoint, ch.SkillPoint = u16(statPoint), u16(skillPoint)
	ch.Str, ch.Dex, ch.Rec, ch.Int, ch.Luc, ch.Wis = u16(str), u16(dex), u16(rec), u16(intel), u16(luc), u16(wis)
	ch.MaxHP, ch.MaxMP, ch.MaxSP = uint32(max(hp, 0)), uint32(max(mp, 0)), uint32(max(sp, 0))
	ch.CurrentHP, ch.CurrentMP, ch.CurrentSP = ch.MaxHP, ch.MaxMP, ch.MaxSP
	ch.Exp, ch.Money = uint32(max(exp, 0)), uint32(money) // Money is an unsigned bit pattern
	ch.PosX, ch.PosY, ch.PosZ = posX, posY, posZ
	ch.Hg, ch.Vg, ch.Cg, ch.Og, ch.Ig = hg, vg, cg, og, ig
	ch.Kills, ch.Deaths, ch.Victories, ch.Defeats = uint32(max(k1, 0)), uint32(max(k2, 0)), uint32(max(k3, 0)), uint32(max(k4, 0))
	ch.KillLevel, ch.DeadLevel = uint8(killLevel.Int64), uint8(deadLevel.Int64)
	var renameCnt uint8
	if err := pool.QueryRowContext(c, `SELECT RenameCnt FROM dbo.Chars WHERE CharID = @p1`, int32(charID)).Scan(&renameCnt); err == nil {
		ch.RenamePending = renameCnt > 0
	}
	return ch, nil
}

// CharacterIDInSlot runs usp_Read_CharSlots_R.
func CharacterIDInSlot(userUID uint32, slot uint8) (uint32, bool, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return 0, false, err
	}
	c, cancel := ctx()
	defer cancel()
	var id int32
	err = pool.QueryRowContext(c, "dbo.usp_Read_CharSlots_R", sql.Named("ServerID", config.ServerID),
		sql.Named("UserUID", int32(userUID)), sql.Named("Slot", slot)).Scan(&id)
	if errors.Is(err, sql.ErrNoRows) {
		return 0, false, nil
	}
	return uint32(id), err == nil, err
}

// NameTaken reports whether a live character, or one deleted within the
// restore window, already uses name.
func NameTaken(name string) (bool, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return false, err
	}
	c, cancel := ctx()
	defer cancel()
	var n int
	err = pool.QueryRowContext(c, `SELECT COUNT(*) FROM dbo.Chars WHERE CharName = @p1
		AND (Del = 0 OR (Del = 1 AND DeleteDate >= DATEADD(hour, -2, GETDATE())))`, name).Scan(&n)
	return n > 0, err
}

// CreateCharacter runs usp_Create_Char_R. The procedure itself rejects
// duplicate names (return -2) and fixes SkillPoint at 5.
// The parameters mirror the original CUser::ProcessCharacterCreate: level
// 1, no stat or skill points, Vg 200.
func CreateCharacter(account Account, nc NewCharacter) (uint32, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return 0, err
	}
	c, cancel := ctx()
	defer cancel()
	var status mssql.ReturnStatus
	_, err = pool.ExecContext(c, "dbo.usp_Create_Char_R",
		sql.Named("ServerID", config.ServerID), sql.Named("UserID", account.UserID),
		sql.Named("UserUID", account.UserUID), sql.Named("CharName", nc.Name), sql.Named("Slot", nc.Slot),
		sql.Named("Family", nc.Race), sql.Named("Grow", nc.Mode), sql.Named("Hair", nc.Hair),
		sql.Named("Face", nc.Face), sql.Named("Size", nc.Height), sql.Named("Job", nc.Class),
		sql.Named("Sex", nc.Gender), sql.Named("Level", int16(1)), sql.Named("Statpoint", int16(0)),
		sql.Named("Skillpoint", int16(0)), sql.Named("Str", int16(nc.Str)), sql.Named("Dex", int16(nc.Dex)),
		sql.Named("Rec", int16(nc.Rec)), sql.Named("Int", int16(nc.Int)), sql.Named("Luc", int16(nc.Luc)),
		sql.Named("Wis", int16(nc.Wis)), sql.Named("Hp", int16(nc.HP)), sql.Named("Mp", int16(nc.MP)), sql.Named("Sp", int16(nc.SP)),
		sql.Named("Map", int16(nc.MapID)), sql.Named("Dir", int16(0)), sql.Named("Exp", int32(0)),
		sql.Named("Money", int32(0)), sql.Named("Posx", nc.PosX), sql.Named("Posy", nc.PosY),
		sql.Named("Posz", nc.PosZ), sql.Named("Hg", int16(0)), sql.Named("Vg", int16(200)),
		sql.Named("Cg", uint8(0)), sql.Named("Og", uint8(0)), sql.Named("Ig", uint8(0)), &status)
	if err != nil {
		return 0, fmt.Errorf("usp_Create_Char_R: %w", err)
	}
	if status <= 0 {
		return 0, fmt.Errorf("usp_Create_Char_R returned %d", status)
	}
	return uint32(status), nil
}

// DeleteCharacter runs usp_Delete_Char_E for a live character owned by
// userUID (Del=1, DeleteDate, guild membership removed).
func DeleteCharacter(userUID, charID uint32) (bool, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return false, err
	}
	c, cancel := ctx()
	defer cancel()
	var n int
	if err := pool.QueryRowContext(c, `SELECT COUNT(*) FROM dbo.Chars WHERE CharID=@p1 AND UserUID=@p2 AND Del=0`,
		int32(charID), int32(userUID)).Scan(&n); err != nil || n == 0 {
		return false, err
	}
	var status mssql.ReturnStatus
	_, err = pool.ExecContext(c, "dbo.usp_Delete_Char_E", sql.Named("ServerID", config.ServerID),
		sql.Named("CharID", int32(charID)), &status)
	return err == nil && status == 1, err
}

// RestoreCharacter undoes a deletion within DeleteWindow. The EP4.5
// procedures have no restore; this is the inverse of usp_Delete_Char_E's
// Chars update.
func RestoreCharacter(userUID, charID uint32) (bool, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return false, err
	}
	c, cancel := ctx()
	defer cancel()
	res, err := pool.ExecContext(c, `UPDATE dbo.Chars SET Del = 0, DeleteDate = NULL
		WHERE CharID = @p1 AND UserUID = @p2 AND Del = 1 AND DeleteDate >= DATEADD(hour, -2, GETDATE())`,
		int32(charID), int32(userUID))
	if err != nil {
		return false, err
	}
	n, _ := res.RowsAffected()
	return n > 0, nil
}

// EnableRename runs Usp_Save_Char_EnableRename_E (name-change stone).
func EnableRename(charID uint32) error {
	return execProc(GameData, "dbo.Usp_Save_Char_EnableRename_E", sql.Named("CharID", int32(charID)))
}

// RenameCharacter commits a pending rename with usp_Update_Char_Rename and
// consumes one RenameCnt.
func RenameCharacter(userUID, charID uint32, oldName, newName string) (bool, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return false, err
	}
	c, cancel := ctx()
	defer cancel()
	var n int
	if err := pool.QueryRowContext(c, `SELECT COUNT(*) FROM dbo.Chars WHERE CharID=@p1 AND UserUID=@p2
		AND Del=0 AND RenameCnt >= 1`, int32(charID), int32(userUID)).Scan(&n); err != nil || n == 0 {
		return false, err
	}
	var ret int32
	if _, err := pool.ExecContext(c, "dbo.usp_Update_Char_Rename", sql.Named("ServerID", config.ServerID),
		sql.Named("CharID", int32(charID)), sql.Named("OldCharName", oldName),
		sql.Named("NewCharName", newName), sql.Named("Ret", sql.Out{Dest: &ret})); err != nil {
		return false, err
	}
	if ret != 1 {
		return false, nil
	}
	_, err = pool.ExecContext(c, `UPDATE dbo.Chars SET RenameCnt = RenameCnt - 1 WHERE CharID = @p1 AND RenameCnt > 0`, int32(charID))
	return err == nil, err
}

// MapForSave maps a world zone to the map stored in Chars.Map (the world
// replaces it so instanced zones save their real map).
var MapForSave = func(zone uint16) uint16 { return zone }

// SaveCharacter persists level, points, stats, HP/MP/SP, position, exp,
// money and kill counters with usp_Save_Char_Info_E.
func SaveCharacter(ch *Character) error {
	clamp16 := func(v uint32) int16 { return int16(min(v, 32767)) }
	return execProc(GameData, "dbo.usp_Save_Char_Info_E",
		sql.Named("CharID", int32(ch.ID)), sql.Named("Level", int32(ch.Level)),
		sql.Named("StatPoint", int16(ch.StatPoint)), sql.Named("SkillPoint", int16(ch.SkillPoint)),
		sql.Named("Str", int16(ch.Str)), sql.Named("Dex", int16(ch.Dex)), sql.Named("Rec", int16(ch.Rec)),
		sql.Named("Int", int16(ch.Int)), sql.Named("Wis", int16(ch.Wis)), sql.Named("Luc", int16(ch.Luc)),
		sql.Named("Hp", clamp16(ch.CurrentHP)), sql.Named("Mp", clamp16(ch.CurrentMP)), sql.Named("Sp", clamp16(ch.CurrentSP)),
		sql.Named("Map", int16(MapForSave(ch.MapID))), sql.Named("Dir", ch.Dir), sql.Named("Exp", int32(ch.Exp)),
		sql.Named("Money", int32(ch.Money)), sql.Named("Posx", ch.PosX), sql.Named("Posy", ch.PosY),
		sql.Named("Posz", ch.PosZ), sql.Named("Hg", int32(ch.Hg)), sql.Named("Vg", int32(ch.Vg)),
		sql.Named("Cg", int32(ch.Cg)), sql.Named("Og", int32(ch.Og)), sql.Named("Ig", int32(ch.Ig)),
		sql.Named("K1", int32(ch.Kills)), sql.Named("K2", int32(ch.Deaths)), sql.Named("K3", int32(ch.Victories)),
		sql.Named("K4", int32(ch.Defeats)), sql.Named("KillLevel", ch.KillLevel), sql.Named("DeadLevel", ch.DeadLevel))
}

// SaveAppearance runs usp_Save_Char_BodyChg_E.
func SaveAppearance(charID uint32, hair, face, size, sex uint8) error {
	return execProc(GameData, "dbo.usp_Save_Char_BodyChg_E", sql.Named("CharID", int32(charID)),
		sql.Named("Hair", hair), sql.Named("Face", face), sql.Named("Size", size), sql.Named("Sex", sex))
}

// CharacterByName finds a live character's account and id by name.
func CharacterByName(name string) (charID, userUID uint32, found bool, err error) {
	pool, err := Pool(GameData)
	if err != nil {
		return 0, 0, false, err
	}
	c, cancel := ctx()
	defer cancel()
	var id, uid int32
	err = pool.QueryRowContext(c, `SELECT TOP 1 CharID, UserUID FROM dbo.Chars WHERE CharName = @p1 AND Del = 0`, name).Scan(&id, &uid)
	if errors.Is(err, sql.ErrNoRows) {
		return 0, 0, false, nil
	}
	return uint32(id), uint32(uid), err == nil, err
}

// CharacterByID loads any live character regardless of account (used to
// credit an offline caster).
func CharacterByID(charID uint32) (Character, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return Character{}, err
	}
	c, cancel := ctx()
	defer cancel()
	var uid int32
	err = pool.QueryRowContext(c, `SELECT UserUID FROM dbo.Chars WHERE CharID = @p1 AND Del = 0`, int32(charID)).Scan(&uid)
	if errors.Is(err, sql.ErrNoRows) {
		return Character{}, ErrNotFound
	}
	if err != nil {
		return Character{}, err
	}
	return CharacterDetail(uint32(uid), charID)
}

func execProc(database, name string, args ...any) error {
	pool, err := Pool(database)
	if err != nil {
		return err
	}
	c, cancel := ctx()
	defer cancel()
	if _, err := pool.ExecContext(c, name, args...); err != nil {
		return fmt.Errorf("%s: %w", name, err)
	}
	return nil
}
