//go:build integration

package main

import (
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// TestViewAgainstSQLServer: two users in the same 5x5 view see each other
// (0x0201) and the one left behind gets 0x0202 when the other logs out.
func TestViewAgainstSQLServer(t *testing.T) {
	setupWorld(t)
	type who struct {
		id, uid uint32
		c       *client
	}
	var users []who
	for _, name := range []string{"Valiant", "Arcanist"} {
		id, uid, found, _ := db.CharacterByName(name)
		if !found {
			t.Skipf("%s not found", name)
		}
		users = append(users, who{id: id, uid: uid})
	}
	a, _ := db.CharacterDetail(users[0].uid, users[0].id)
	b, _ := db.CharacterDetail(users[1].uid, users[1].id)
	if a.MapID != b.MapID {
		t.Skipf("Valiant and Arcanist are on maps %d and %d", a.MapID, b.MapID)
	}
	defer func() {
		pool, _ := db.Pool(db.GameData)
		for _, s := range []db.Character{a, b} {
			pool.Exec(`UPDATE dbo.Chars SET PosX = @p2, PosY = @p3, PosZ = @p4 WHERE CharID = @p1`, int32(s.ID), s.PosX, s.PosY, s.PosZ)
		}
	}()
	seen := map[packet.Opcode]int{}
	enter := func(u *who) {
		u.c = connect(t, u.uid)
		u.c.until(packet.AccountFaction, seen)
		u.c.write(msg(packet.SelectCharacter, byte(u.id), byte(u.id>>8), byte(u.id>>16), byte(u.id>>24)))
		u.c.until(packet.CharacterSkillBar, seen)
		u.c.expanded = true
		u.c.write(msg(packet.CharacterEnteredMap))
		u.c.until(packet.CharacterMove, seen)
		m := packet.New(packet.CharacterMove)
		m.U8(1)
		m.U16(0)
		m.F32(a.PosX + 1)
		m.F32(a.PosY)
		m.F32(a.PosZ)
		u.c.write(m.MustFinish()[2:])
	}
	enter(&users[0])
	enter(&users[1])
	waitFor := func(c *client, want packet.Opcode, id uint32) bool {
		for end := time.Now().Add(5 * time.Second); time.Now().Before(end); {
			p := c.read(time.Second)
			if p != nil && op(p) == want {
				if got, _ := packet.U32(p[2:]); got == id {
					return true
				}
			}
		}
		return false
	}
	if !waitFor(users[0].c, 0x0201, users[1].id) {
		t.Fatal("Valiant did not see Arcanist enter")
	}
	users[1].c.write(msg(packet.Logout))
	if !waitFor(users[0].c, 0x0202, users[1].id) {
		t.Fatal("Valiant did not get Arcanist's 0x0202 on logout")
	}
	users[0].c.write(msg(packet.Logout))
}
