//go:build integration

package main

import (
	"fmt"
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
	"github.com/Spelunkern/Rapture/internal/world"
)

func TestQuestAgainstSQLServer(t *testing.T) {
	setupWorld(t)
	time.Sleep(time.Second)
	c := connect(t, testAccount)
	seen := map[packet.Opcode]int{}
	c.until(packet.AccountFaction, seen)
	c.write(msg(packet.AccountFaction, 0))
	c.until(packet.AccountFaction, seen)

	name := fmt.Sprintf("QTest%04d", time.Now().UnixNano()%10000)
	create := msg(packet.CreateCharacter, 3, 0, 1, 1, 1, 1, 0, 0) // slot 3, human fighter, normal
	create = append(create, make([]byte, 19)...)
	copy(create[10:], name)
	c.write(create)
	if r := c.until(packet.CreateCharacter, seen); r[2] != 0 {
		t.Fatalf("create rejected")
	}
	c.until(packet.AccountFaction, seen)
	charID, _, _, _ := db.CharacterByName(name)
	defer func() {
		db.DeleteCharacter(testAccount, charID)
		pool, _ := db.Pool(db.GameData)
		pool.Exec(`DELETE FROM dbo.CharQuests WHERE CharID = @p1; DELETE FROM dbo.CharItems WHERE CharID = @p1;
			DELETE FROM dbo.CharSkills WHERE CharID = @p1; DELETE FROM dbo._DeletedChars WHERE CharID = @p1;
			DELETE FROM dbo.Chars WHERE CharID = @p1`, int32(charID))
	}()

	c.write(msg(packet.SelectCharacter, byte(charID), byte(charID>>8), byte(charID>>16), byte(charID>>24)))
	c.until(packet.QuestList, seen)
	c.until(packet.CharacterSkillBar, seen)
	c.expanded = true
	c.write(msg(packet.CharacterEnteredMap))
	c.until(packet.CharacterMove, seen)
	time.Sleep(500 * time.Millisecond)

	var startNpc uint32
	for _, n := range world.EntitiesOnMap(1).NPCs {
		if n.Type == 8 && n.TypeID == 83 {
			startNpc = n.ID
		}
	}
	if startNpc == 0 {
		t.Skip("quest NPC 8/83 not on map 1")
	}
	le := func(v uint32) []byte { return []byte{byte(v), byte(v >> 8), byte(v >> 16), byte(v >> 24)} }
	req := func(op packet.Opcode, npc uint32, quest uint16) []byte {
		return msg(op, append(le(npc), byte(quest), byte(quest>>8))...)
	}

	c.write(req(packet.QuestStart, startNpc, 1))
	start := c.until(packet.QuestStart, seen)
	if id, _ := packet.U16(start[6:]); id != 1 {
		t.Fatalf("started quest %d", id)
	}
	// Handing in with no kills must not complete it.
	c.write(req(packet.QuestEnd, startNpc+6, 1))
	c.write(msg(packet.QuestAbandon, 1, 0))
	end := c.until(packet.QuestEnd, seen)
	if id, _ := packet.U16(end[6:]); id != 1 || end[8] != 0 {
		t.Fatalf("abandon reply quest %d success %d", id, end[8])
	}
	time.Sleep(300 * time.Millisecond) // the reply precedes the database write, as in the original
	rows, _ := db.FinishedQuests(charID)
	if len(rows) != 1 || rows[0].QuestID != 1 || rows[0].Success != 0 {
		t.Fatalf("finished rows %+v", rows)
	}
	// A finished (even failed) quest cannot be taken again.
	c.write(req(packet.QuestStart, startNpc, 1))
	c.write(msg(packet.Ping))
	for {
		p := c.read(5 * time.Second)
		if p == nil {
			t.Fatal("no ping reply")
		}
		if op(p) == packet.QuestStart {
			t.Fatal("failed quest restarted")
		}
		if op(p) == packet.Ping {
			break
		}
	}
	t.Logf("quest flow ok; opcodes seen: %v", seen)
}
