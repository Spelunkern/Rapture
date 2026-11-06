//go:build integration

package main

import (
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// enterWorld selects a migrated test character and loads its map.
func enterWorld(t *testing.T, account uint32, name string) (*client, uint32) {
	t.Helper()
	c := connect(t, account)
	seen := map[packet.Opcode]int{}
	c.until(packet.AccountFaction, seen)
	id, _, found, err := db.CharacterByName(name)
	if err != nil || !found {
		t.Skipf("test character %s not found", name)
	}
	sel := msg(packet.SelectCharacter, byte(id), byte(id>>8), byte(id>>16), byte(id>>24))
	c.write(sel)
	c.until(packet.CharacterSkillBar, seen)
	c.expanded = true
	c.write(msg(packet.CharacterEnteredMap))
	c.until(packet.CharacterMove, seen)
	// The server registers the player after streaming the map's entities.
	time.Sleep(500 * time.Millisecond)
	return c, id
}

func TestPartyAgainstSQLServer(t *testing.T) {
	setupWorld(t)
	leader, leaderID := enterWorld(t, 1, "Interloper")
	member, memberID := enterWorld(t, 3, "Arcanist")
	seen := map[packet.Opcode]int{}

	leader.write(msg(packet.PartyInvite, byte(memberID), byte(memberID>>8), byte(memberID>>16), byte(memberID>>24)))
	if inv := member.until(packet.PartyInvite, seen); len(inv) < 6 {
		t.Fatal("short invite")
	}
	member.write(msg(packet.PartyAnswer, 0, byte(leaderID), byte(leaderID>>8), byte(leaderID>>16), byte(leaderID>>24)))

	if r := leader.until(packet.PartyRoster, seen); r[3] != 0 {
		t.Fatalf("leader roster count %d, want 0", r[3])
	}
	joined := leader.until(packet.PartyJoined, seen)
	if id, _ := packet.U32(joined[2:]); id != memberID {
		t.Fatalf("joined id %d, want %d", id, memberID)
	}
	roster := member.until(packet.PartyRoster, seen)
	if roster[2] != 0 || roster[3] != 1 {
		t.Fatalf("member roster leader %d count %d", roster[2], roster[3])
	}
	if id, _ := packet.U32(roster[4:]); id != leaderID {
		t.Fatalf("roster member %d, want %d", id, leaderID)
	}
	t.Logf("party formed: %d leads %d (roster %d bytes)", leaderID, memberID, len(roster))

	chat := msg(packet.ChatParty, 5)
	chat = append(chat, []byte("hola\x00")...)
	member.write(chat)
	if c := leader.until(packet.ChatParty, seen); string(c[7:11]) != "hola" {
		t.Fatalf("party chat %q", c[7:])
	}

	member.write(msg(packet.PartyLeft))
	left := leader.until(packet.PartyLeft, seen)
	if id, _ := packet.U32(left[2:]); id != memberID {
		t.Fatalf("left id %d", id)
	}
	// A party of one dissolves: the leader leaves too.
	dissolved := leader.until(packet.PartyLeft, seen)
	if id, _ := packet.U32(dissolved[2:]); id != leaderID {
		t.Fatalf("dissolve id %d", id)
	}
	t.Logf("opcodes seen: %v", seen)
}

func TestRaidAgainstSQLServer(t *testing.T) {
	setupWorld(t)
	// Let the server release accounts from a previous test's sessions.
	time.Sleep(time.Second)
	leader, leaderID := enterWorld(t, 1, "Interloper")
	member, memberID := enterWorld(t, 3, "Arcanist")
	seen := map[packet.Opcode]int{}
	le := func(v uint32) []byte { return []byte{byte(v), byte(v >> 8), byte(v >> 16), byte(v >> 24)} }

	leader.write(msg(packet.RaidInvite, le(memberID)...))
	member.until(packet.RaidInvite, seen)
	member.write(msg(packet.RaidAnswer, append([]byte{0}, le(leaderID)...)...))
	member.until(packet.PartyRoster, seen)

	// Convert to a raid: kind 1, auto-join on, item division 2.
	leader.write(msg(packet.RaidConvert, append([]byte{1, 1}, le(2)...)...))
	conv := member.until(packet.RaidConvert, seen)
	if conv[2] != 1 || conv[3] != 1 {
		t.Fatalf("convert reply kind %d autojoin %d", conv[2], conv[3])
	}
	if div, _ := packet.U32(conv[4:]); div != 2 {
		t.Fatalf("division %d", div)
	}
	leader.write(msg(packet.RaidDivision, le(3)...))
	if d := member.until(packet.RaidDivision, seen); d[2] != 3 {
		t.Fatalf("division update %d", d[2])
	}
	leader.write(msg(packet.RaidSwap, append(le(0), le(5)...)...))
	swap := member.until(packet.RaidSwap, seen)
	if lead, _ := packet.U32(swap[10:]); lead != 5 {
		t.Fatalf("leader wire slot after swap %d, want 5", lead)
	}
	member.write(msg(packet.PartyLeaveAlt))
	if l := leader.until(packet.PartyLeaveAlt, seen); len(l) < 6 {
		t.Fatal("short raid leave")
	}
	t.Logf("raid ok; opcodes seen: %v", seen)
}
