//go:build integration

package main

import (
	"encoding/binary"
	"testing"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// TestItemMallAgainstSQLServer buys a product of the client's Cash.SData
// catalog (AP_1: 10 points, item 100/251) as Valiant and checks the 0x0205
// grant, the 0x2602 result and the balance, then undoes the purchase.
func TestItemMallAgainstSQLServer(t *testing.T) {
	setupWorld(t)
	const code = "AP_1"
	prod := db.MustDefs().Products[code]
	if prod == nil {
		t.Skip("AP_1 not in ProductList (load the client catalog first)")
	}
	charID, uid, found, err := db.CharacterByName("Valiant")
	if err != nil || !found {
		t.Skip("Valiant not found")
	}
	before, err := db.Points(uid)
	if err != nil {
		t.Fatal(err)
	}
	var bag, slot byte
	defer func() {
		user, _ := db.Pool(db.UserData)
		user.Exec(`UPDATE dbo.Users_Master SET Point = @p2 WHERE UserUID = @p1`, int32(uid), int32(before))
		if bag != 0 {
			game, _ := db.Pool(db.GameData)
			game.Exec(`DELETE FROM dbo.CharItems WHERE CharID = @p1 AND Bag = @p2 AND Slot = @p3`, int32(charID), bag, slot)
		}
	}()

	c := connect(t, uid)
	seen := map[packet.Opcode]int{}
	c.until(packet.AccountFaction, seen)
	c.write(msg(packet.SelectCharacter, byte(charID), byte(charID>>8), byte(charID>>16), byte(charID>>24)))
	c.until(packet.CharacterSkillBar, seen)
	c.expanded = true
	c.write(msg(packet.CharacterEnteredMap))
	c.until(packet.CharacterMove, seen)

	buy := make([]byte, 21)
	copy(buy, code)
	c.write(msg(packet.PointBuy, buy...))
	add := c.until(packet.AddItem, seen)
	bag, slot = add[2], add[3]
	if add[4] != prod.Items[0].Type || add[5] != prod.Items[0].TypeID {
		t.Fatalf("granted %d/%d, want %d/%d", add[4], add[5], prod.Items[0].Type, prod.Items[0].TypeID)
	}
	res := c.until(packet.PointBuy, seen)
	if res[2] != 0 {
		t.Fatalf("point buy result %d", res[2])
	}
	if got := binary.LittleEndian.Uint32(res[3:]); got != before-prod.BuyCost {
		t.Fatalf("points %d, want %d", got, before-prod.BuyCost)
	}
	if res[36] != 1 || res[37] != bag || res[38] != slot {
		t.Fatalf("result items % x", res[36:])
	}
	if after, _ := db.Points(uid); after != before-prod.BuyCost {
		t.Fatalf("stored points %d, want %d", after, before-prod.BuyCost)
	}

	missing := make([]byte, 21)
	copy(missing, "NoSuchCode")
	c.write(msg(packet.PointBuy, missing...))
	if res := c.until(packet.PointBuy, seen); res[2] != 1 {
		t.Fatalf("unknown product result %d, want 1", res[2])
	}
}
