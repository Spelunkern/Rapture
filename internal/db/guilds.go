package db

import (
	"database/sql"
	"fmt"
	"strings"

	mssql "github.com/microsoft/go-mssqldb"
)

// Guilds live in PS_GameData: Guilds, GuildDetails and GuildChars
// (GuildLevel 0 is a pending applicant, 1 the master, 2 sub-masters, up to
// 9). The original game server reached them through the DB agent; Rapture
// calls the same procedures. usp_Read_Guild2_R (orders by a Guilds.Rank
// column the EP4.5 schema lacks) and usp_Save_GuildRemark_E (reads a
// missing GuildHouses table) fail on this database, so their statements
// are run as plain SQL.

// mirrorGuild is what Trig_Guilds_Insert/Update/Delete did through the
// linked server PS_USERDB01 (disabled, see tools/sqlserver.py prepare):
// PS_UserData.dbo.CreatedGuilds holds a copy of the Guilds row for server 1.
// The EP4.5 CreatedGuilds also has a NOT NULL UpdateDate the triggers never
// set; it gets the time of the change.
func mirrorGuild(guildID uint32) error {
	pool, err := Pool(GameData)
	if err != nil {
		return err
	}
	c, cancel := ctx()
	defer cancel()
	_, err = pool.ExecContext(c, `DELETE FROM PS_UserData.dbo.CreatedGuilds WHERE ServerID = 1 AND GuildID = @p1;
		INSERT INTO PS_UserData.dbo.CreatedGuilds (ServerID, GuildID, GuildName, MasterUserID, MasterCharID, MasterName,
			Country, TotalCount, GuildPoint, Del, CreateDate, UpdateDate, DeleteDate)
		SELECT 1, GuildID, GuildName, MasterUserID, MasterCharID, MasterName, Country, TotalCount, GuildPoint, Del,
			CreateDate, GETDATE(), DeleteDate FROM dbo.Guilds WHERE GuildID = @p1`, int32(guildID))
	if err != nil {
		return fmt.Errorf("mirror guild %d to CreatedGuilds: %w", guildID, err)
	}
	return nil
}

// guildOfMember is the live guild of a character, for the mirror after
// procedures keyed by CharID.
func guildOfMember(charID uint32) uint32 {
	pool, err := Pool(GameData)
	if err != nil {
		return 0
	}
	c, cancel := ctx()
	defer cancel()
	var id int64
	_ = pool.QueryRowContext(c, `SELECT TOP 1 GuildID FROM dbo.GuildChars WHERE CharID = @p1 AND Del = 0`, int32(charID)).Scan(&id)
	return uint32(id)
}

// GuildRow is one usp_Read_Guild2_R row.
type GuildRow struct {
	ID         uint32
	Name       string
	MasterName string
	Country    uint8
	Points     uint32
	Rank       uint8
	Etin       uint32
	Remark     string
	BuyHouse   uint8
	UseHouse   uint8
	EtinReturn uint32
	KeepEtin   uint32
}

// GuildCharRow is one usp_Read_GuildChar_R row.
type GuildCharRow struct {
	CharID  uint32
	Name    string
	Level   uint16
	Job     uint8
	GuildID uint32
	Rank    uint8 // GuildLevel
}

// LoadGuilds reads every live guild and its members.
func LoadGuilds() ([]GuildRow, []GuildCharRow, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return nil, nil, err
	}
	c, cancel := ctx()
	defer cancel()
	rows, err := pool.QueryContext(c, `SELECT G.GuildID, G.GuildName, G.MasterName, G.Country, G.GuildPoint,
		ISNULL(H.Rank,31), ISNULL(H.Etin,0), ISNULL(H.Remark,''), ISNULL(H.BuyHouse,0), ISNULL(H.UseHouse,0),
		ISNULL(H.EtinReturnCnt,0), ISNULL(H.KeepEtin,0)
		FROM dbo.Guilds G LEFT OUTER JOIN dbo.GuildDetails H ON G.GuildID = H.GuildID WHERE G.Del = 0`)
	if err != nil {
		return nil, nil, fmt.Errorf("read guilds: %w", err)
	}
	var guilds []GuildRow
	for rows.Next() {
		var g GuildRow
		var id, points, etin, ret, keep int64
		if err := rows.Scan(&id, &g.Name, &g.MasterName, &g.Country, &points, &g.Rank, &etin, &g.Remark, &g.BuyHouse,
			&g.UseHouse, &ret, &keep); err != nil {
			rows.Close()
			return nil, nil, err
		}
		g.ID, g.Points, g.Etin = uint32(id), uint32(max(points, 0)), uint32(max(etin, 0))
		g.EtinReturn, g.KeepEtin = uint32(max(ret, 0)), uint32(max(keep, 0))
		g.Name, g.MasterName, g.Remark = strings.TrimSpace(g.Name), strings.TrimSpace(g.MasterName), strings.TrimSpace(g.Remark)
		guilds = append(guilds, g)
	}
	rows.Close()
	rows, err = pool.QueryContext(c, "dbo.usp_Read_GuildChar_R", sql.Named("GuildID", int32(0)))
	if err != nil {
		return nil, nil, fmt.Errorf("usp_Read_GuildChar_R: %w", err)
	}
	defer rows.Close()
	var members []GuildCharRow
	for rows.Next() {
		var m GuildCharRow
		var charID, guildID int64
		var level int64
		if err := rows.Scan(&charID, &m.Name, &level, &m.Job, &guildID, &m.Rank); err != nil {
			return nil, nil, err
		}
		m.CharID, m.GuildID, m.Level, m.Name = uint32(charID), uint32(guildID), uint16(max(level, 0)), strings.TrimSpace(m.Name)
		members = append(members, m)
	}
	return guilds, members, rows.Err()
}

// MaxGuildID is usp_Read_MaxGuildID_R: the next free guild id.
func NextGuildID() (uint32, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return 0, err
	}
	c, cancel := ctx()
	defer cancel()
	var next int64
	err = pool.QueryRowContext(c, "dbo.usp_Read_MaxGuildID_R").Scan(&next)
	return uint32(max(next, 1)), err
}

func execStatus(name string, args ...any) (int, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return 0, err
	}
	c, cancel := ctx()
	defer cancel()
	var status mssql.ReturnStatus
	if _, err := pool.ExecContext(c, name, append(args, &status)...); err != nil {
		return 0, fmt.Errorf("%s: %w", name, err)
	}
	return int(status), nil
}

// CreateGuild is usp_Create_Guild2_E (0 on success).
func CreateGuild(id uint32, name, masterUserID string, masterCharID uint32, masterName string, country uint8, remark string) error {
	status, err := execStatus("dbo.usp_Create_Guild2_E", sql.Named("GuildID", int32(id)), sql.Named("GuildName", name),
		sql.Named("MasterUserID", masterUserID), sql.Named("MasterCharID", int32(masterCharID)),
		sql.Named("MasterName", masterName), sql.Named("Country", country), sql.Named("Remark", remark))
	if err == nil && status != 0 {
		err = fmt.Errorf("usp_Create_Guild2_E returned %d", status)
	}
	if err != nil {
		return err
	}
	return mirrorGuild(id)
}

// AddGuildChar is usp_Create_GuildChar_E (rank 0 for an applicant).
func AddGuildChar(guildID, charID uint32, rank uint8) error {
	status, err := execStatus("dbo.usp_Create_GuildChar_E", sql.Named("GuildID", int32(guildID)),
		sql.Named("CharID", int32(charID)), sql.Named("GuildLevel", rank))
	if err == nil && status != 0 {
		err = fmt.Errorf("usp_Create_GuildChar_E returned %d", status)
	}
	if err != nil {
		return err
	}
	return mirrorGuild(guildID)
}

// SaveGuildCharRank is usp_Save_GuildChar_E (rank 1 also moves the master).
func SaveGuildCharRank(charID uint32, rank uint8) error {
	if _, err := execStatus("dbo.usp_Save_GuildChar_E", sql.Named("CharID", int32(charID)), sql.Named("GuildLevel", rank)); err != nil {
		return err
	}
	if rank != 1 {
		return nil // only a new master changes the Guilds row
	}
	return mirrorGuild(guildOfMember(charID))
}

// DeleteGuildChar is usp_Delete_GuildChar_E.
func DeleteGuildChar(guildID, charID uint32) error {
	if _, err := execStatus("dbo.usp_Delete_GuildChar_E", sql.Named("GuildID", int32(guildID)), sql.Named("CharID", int32(charID))); err != nil {
		return err
	}
	return mirrorGuild(guildID)
}

// DeleteGuild is usp_Delete_Guild_E.
func DeleteGuild(guildID uint32) error {
	if _, err := execStatus("dbo.usp_Delete_Guild_E", sql.Named("GuildID", int32(guildID))); err != nil {
		return err
	}
	return mirrorGuild(guildID)
}

// SaveGuildRemark stores the notice in GuildDetails (usp_Save_GuildRemark_E's
// update or insert).
func SaveGuildRemark(guildID uint32, remark string) error {
	pool, err := Pool(GameData)
	if err != nil {
		return err
	}
	c, cancel := ctx()
	defer cancel()
	_, err = pool.ExecContext(c, `UPDATE dbo.GuildDetails SET Remark = @p2 WHERE GuildID = @p1;
		IF @@ROWCOUNT = 0 INSERT INTO dbo.GuildDetails (GuildID, Remark) VALUES (@p1, @p2)`, int32(guildID), remark)
	return err
}

// GuildJoinRemainHours is usp_Read_Char_GuildJoinDate_R: hours left of the
// 72 hours after the character's last guild join.
func GuildJoinRemainHours(charID uint32) (int, error) {
	return execStatus("dbo.usp_Read_Char_GuildJoinDate_R", sql.Named("CharID", int32(charID)))
}

// SaveGuildRanking is usp_Save_GuildInfo_E2 (the DB agent's 0x0B16 batch):
// guild points, rank and the house fields, then the CreatedGuilds copy.
func SaveGuildRanking(g GuildRow) error {
	if _, err := execStatus("dbo.usp_Save_GuildInfo_E2", sql.Named("GuildID", int32(g.ID)), sql.Named("GuildPoint", int32(g.Points)),
		sql.Named("Etin", int32(g.Etin)), sql.Named("Remark", g.Remark), sql.Named("BuyHouse", g.BuyHouse),
		sql.Named("EtinReturnCnt", int32(g.EtinReturn)), sql.Named("KeepEtin", int32(g.KeepEtin)),
		sql.Named("Rank", g.Rank), sql.Named("UseHouse", g.UseHouse)); err != nil {
		return err
	}
	return mirrorGuild(g.ID)
}
