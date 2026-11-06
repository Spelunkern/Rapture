package db

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"time"

	mssql "github.com/microsoft/go-mssqldb"
)

// The item mall uses Users_Master.Point, PS_GameData.dbo.PointLog and the
// gift box PS_GameData.dbo.UserStoredPointItems (Slot 0-239). The original
// purchase, gift and claim procedures need a linked server and MSDTC
// (PS_DEFINEDB, BEGIN DISTRIBUTED TRANSACTION), unavailable to an isolated
// instance, so their effects are reproduced here in local transactions;
// point deduction still goes through PS_UserData.dbo.usp_Update_UserPoint.

const GiftBoxSlots = 240

// Points reads the account's item-mall balance.
func Points(userUID uint32) (uint32, error) {
	pool, err := Pool(UserData)
	if err != nil {
		return 0, err
	}
	c, cancel := ctx()
	defer cancel()
	var point sql.NullInt32
	err = pool.QueryRowContext(c, `SELECT Point FROM dbo.Users_Master WHERE UserUID = @p1`, int32(userUID)).Scan(&point)
	if errors.Is(err, sql.ErrNoRows) {
		return 0, ErrNotFound
	}
	return uint32(max(point.Int32, 0)), err
}

// ErrNotEnoughPoints means the balance does not cover the cost.
var ErrNotEnoughPoints = errors.New("not enough points")

// SpendPoints deducts cost with usp_Update_UserPoint (which zeroes the
// balance on overdraft, so the balance is checked first) and writes the
// PointLog row the original NCash procedures write. useType: 1 buy, 2 gift.
func SpendPoints(userUID, charID, cost uint32, productCode, targetName string, useType uint8) (uint32, error) {
	current, err := Points(userUID)
	if err != nil {
		return 0, err
	}
	if current < cost {
		return current, ErrNotEnoughPoints
	}
	pool, err := Pool(UserData)
	if err != nil {
		return 0, err
	}
	c, cancel := ctx()
	defer cancel()
	var status mssql.ReturnStatus
	if _, err := pool.ExecContext(c, "dbo.usp_Update_UserPoint", sql.Named("UserUID", int32(userUID)),
		sql.Named("UsePoint", int32(cost)), &status); err != nil {
		return 0, fmt.Errorf("usp_Update_UserPoint: %w", err)
	}
	if status != 1 {
		return 0, ErrNotEnoughPoints
	}
	remaining := current - cost
	game, err := Pool(GameData)
	if err != nil {
		return remaining, err
	}
	_, err = game.ExecContext(c, `INSERT INTO dbo.PointLog (UseType, UserUID, CharID, UsePoint, ProductCode,
		TargetName, UseDate, RemainPoint) VALUES (@p1, @p2, @p3, @p4, @p5, @p6, GETDATE(), @p7)`,
		useType, int32(userUID), int32(charID), int32(cost), productCode, targetName, int32(remaining))
	return remaining, err
}

// GiftEntry is one gift-box slot.
type GiftEntry struct {
	Slot                uint8
	Type, TypeID, Count uint8
}

// GiftBox runs usp_Read_User_StoredPointItems.
func GiftBox(userUID uint32) ([]GiftEntry, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return nil, err
	}
	c, cancel := ctx()
	defer cancel()
	rows, err := pool.QueryContext(c, "dbo.usp_Read_User_StoredPointItems", sql.Named("UserUID", int32(userUID)))
	if err != nil {
		return nil, fmt.Errorf("usp_Read_User_StoredPointItems: %w", err)
	}
	defer rows.Close()
	var gifts []GiftEntry
	for rows.Next() {
		var slot, count uint8
		var itemID int32
		if err := rows.Scan(&slot, &itemID, &count); err != nil {
			return nil, err
		}
		if slot < GiftBoxSlots && itemID > 0 && count > 0 {
			gifts = append(gifts, GiftEntry{Slot: slot, Type: uint8(min(itemID/1000, 255)),
				TypeID: uint8(min(itemID%1000, 255)), Count: count})
		}
	}
	return gifts, rows.Err()
}

// GiftResult codes match the original usp_Save_User_GiftPointItems_NCash
// and the client's gift result byte.
const (
	GiftOK             = 0
	GiftNoSuchChar     = 1
	GiftSameAccount    = 2
	GiftBoxFull        = 3
	GiftNotEnoughSlots = 4
	GiftInsertError    = 5
)

// StoreGift puts every item of product into the gift box of the account
// owning targetName, in free slots, atomically.
func StoreGift(targetName string, senderUID uint32, product *Product) (result uint8, targetUID uint32, err error) {
	pool, err := Pool(GameData)
	if err != nil {
		return GiftInsertError, 0, err
	}
	c, cancel := ctx()
	defer cancel()
	var uid sql.NullInt32
	if err := pool.QueryRowContext(c, `SELECT TOP 1 UserUID FROM dbo.Chars WHERE CharName = @p1 AND Del = 0`,
		targetName).Scan(&uid); err != nil && !errors.Is(err, sql.ErrNoRows) {
		return GiftInsertError, 0, err
	}
	if !uid.Valid || uid.Int32 == 0 {
		return GiftNoSuchChar, 0, nil
	}
	targetUID = uint32(uid.Int32)
	if targetUID == senderUID {
		return GiftSameAccount, targetUID, nil
	}
	err = withTx(c, pool, func(tx *sql.Tx) error {
		used := map[uint8]bool{}
		rows, err := tx.QueryContext(c, `SELECT Slot FROM dbo.UserStoredPointItems WITH (UPDLOCK, HOLDLOCK) WHERE UserUID = @p1`, int32(targetUID))
		if err != nil {
			return err
		}
		for rows.Next() {
			var slot uint8
			rows.Scan(&slot)
			used[slot] = true
		}
		rows.Close()
		if len(used) >= GiftBoxSlots {
			result = GiftBoxFull
			return errRollback
		}
		next := uint8(0)
		for _, item := range product.Items {
			for int(next) < GiftBoxSlots && used[next] {
				next++
			}
			if int(next) >= GiftBoxSlots {
				result = GiftNotEnoughSlots
				return errRollback
			}
			if _, err := tx.ExecContext(c, `INSERT INTO dbo.UserStoredPointItems (UserUID, Slot, ItemID, ItemCount, BuyDate)
				VALUES (@p1, @p2, @p3, @p4, GETDATE())`, int32(targetUID), next,
				int32(item.Type)*1000+int32(item.TypeID), item.Count); err != nil {
				result = GiftInsertError
				return err
			}
			used[next] = true
		}
		return nil
	})
	if errors.Is(err, errRollback) {
		return result, targetUID, nil
	}
	if err != nil {
		return GiftInsertError, targetUID, err
	}
	return GiftOK, targetUID, nil
}

// ClaimGift moves gift-box slot into the character's inventory as it, in
// one transaction (the effect of usp_Save_User_StoredPointItems). It
// returns false if the slot was already claimed.
func ClaimGift(userUID, charID uint32, giftSlot uint8, it *Item) (bool, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return false, err
	}
	c, cancel := ctx()
	defer cancel()
	if it.UID == 0 {
		it.UID = NewItemUID()
	}
	if it.MakeTime.IsZero() {
		it.MakeTime = time.Now()
	}
	claimed := false
	err = withTx(c, pool, func(tx *sql.Tx) error {
		res, err := tx.ExecContext(c, `DELETE FROM dbo.UserStoredPointItems WHERE UserUID = @p1 AND Slot = @p2`,
			int32(userUID), giftSlot)
		if err != nil {
			return err
		}
		if n, _ := res.RowsAffected(); n != 1 {
			return errRollback
		}
		if _, err := tx.ExecContext(c, `INSERT INTO dbo.CharItems (CharID, Bag, Slot, ItemID, Type, TypeID, ItemUID,
			Quality, Gem1, Gem2, Gem3, Gem4, Gem5, Gem6, Craftname, Count, Maketime, Maketype)
			VALUES (@p1, @p2, @p3, @p4, @p5, @p6, @p7, @p8, 0, 0, 0, 0, 0, 0, '', @p9, @p10, 'B')`,
			int32(charID), it.Bag, it.Slot, it.ItemID(), it.Type, it.TypeID, it.UID,
			int16(min(it.Quality, 4999)), it.Count, it.MakeTime); err != nil {
			return err
		}
		claimed = true
		return nil
	})
	if errors.Is(err, errRollback) {
		return false, nil
	}
	return claimed, err
}

var errRollback = errors.New("rollback")

func withTx(c context.Context, pool *sql.DB, fn func(*sql.Tx) error) error {
	tx, err := pool.BeginTx(c, nil)
	if err != nil {
		return err
	}
	if err := fn(tx); err != nil {
		tx.Rollback()
		return err
	}
	return tx.Commit()
}
