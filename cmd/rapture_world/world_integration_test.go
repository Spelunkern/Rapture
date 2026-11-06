//go:build integration

// Run with: go test -tags integration ./cmd/rapture_world
// Needs the SQL Server from tools/sqlserver.py. Plays a synthetic client on
// test account 10: faction, create, select, map load, move, /town, logout,
// delete. The character it creates is removed at the end.
package main

import (
	"crypto/rand"
	"fmt"
	"io"
	"net"
	"os"
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/config"
	"github.com/Spelunkern/Rapture/internal/crypto"
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
	"github.com/Spelunkern/Rapture/internal/packet"
	"github.com/Spelunkern/Rapture/internal/session"
	"github.com/Spelunkern/Rapture/internal/world"
)

const testAccount = 10

type client struct {
	t        *testing.T
	conn     net.Conn
	send     *crypto.AESCTR
	recv     *crypto.AESCTR
	expanded bool
}

func (c *client) write(payload []byte) {
	c.t.Helper()
	frame := append([]byte{byte(len(payload) + 2), byte((len(payload) + 2) >> 8)}, c.send.Transform(payload)...)
	if _, err := c.conn.Write(frame); err != nil {
		c.t.Fatal(err)
	}
}

// read returns the next decoded payload, or nil after the timeout.
func (c *client) read(timeout time.Duration) []byte {
	c.conn.SetReadDeadline(time.Now().Add(timeout))
	head := make([]byte, 2)
	if _, err := io.ReadFull(c.conn, head); err != nil {
		return nil
	}
	frame := make([]byte, int(head[0])|int(head[1])<<8)
	if _, err := io.ReadFull(c.conn, frame[2:]); err != nil {
		c.t.Fatal(err)
	}
	if c.expanded {
		return crypto.ExpandedXOR(frame[2:])
	}
	return c.recv.Transform(frame[2:])
}

func op(p []byte) packet.Opcode { v, _ := packet.U16(p); return packet.Opcode(v) }

// until reads payloads until one has the wanted opcode.
func (c *client) until(want packet.Opcode, seen map[packet.Opcode]int) []byte {
	c.t.Helper()
	for {
		p := c.read(10 * time.Second)
		if p == nil {
			c.t.Fatalf("timed out waiting for 0x%04X (seen %v)", uint16(want), seen)
		}
		seen[op(p)]++
		if op(p) == want {
			return p
		}
	}
}

func msg(o packet.Opcode, body ...byte) []byte {
	return append([]byte{byte(o), byte(o >> 8)}, body...)
}

func TestWorldSessionAgainstSQLServer(t *testing.T) {
	os.Chdir("../..")
	config.LoadRuntimeEnv()
	logger, err := logx.New("rapture_world_test")
	if err != nil {
		t.Fatal(err)
	}
	if err := db.Open(db.UserData, db.GameData, db.GameDefs); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Definitions(); err != nil {
		t.Fatal(err)
	}
	if err := world.LoadGameConfig(logger); err != nil {
		t.Fatal(err)
	}
	if err := world.LoadSvmaps(logger); err != nil {
		t.Fatal(err)
	}
	world.StartSweeps(logger)
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go func() { world.Serve(conn, logger, logger.Tagged("test")); conn.Close() }()
		}
	}()

	var material [32]byte
	rand.Read(material[:])
	identity := session.NewIdentity()
	if err := session.Save(session.Handoff{Identity: identity, AESMaterial: material, UserUID: testAccount, SessionID: session.NewSessionID()}); err != nil {
		t.Fatal(err)
	}
	conn, err := net.Dial("tcp", listener.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	var key, loginIV [16]byte
	copy(key[:], material[:16])
	copy(loginIV[:], material[16:])
	iv := crypto.DeriveWorldAESIV(loginIV)
	c := &client{t: t, conn: conn, send: crypto.NewAESCTR(key, iv), recv: crypto.NewAESCTR(key, iv)}

	hs := packet.New(packet.GameHandshake)
	hs.U32(testAccount)
	hs.Bytes(identity[:])
	conn.Write(hs.MustFinish())
	seen := map[packet.Opcode]int{}
	c.until(packet.GameHandshake, seen)
	c.until(packet.AccountFaction, seen)

	c.write(msg(packet.AccountFaction, 0))
	c.until(packet.AccountFaction, seen)

	name := fmt.Sprintf("GoTest%04d", time.Now().UnixNano()%10000)
	create := msg(packet.CreateCharacter, 4, 0, 1, 1, 1, 1, 0, 0) // slot 4, human fighter
	create = append(create, make([]byte, 19)...)
	copy(create[10:], name)
	c.write(create)
	if r := c.until(packet.CreateCharacter, seen); r[2] != 0 {
		t.Fatalf("create rejected (%d)", r[2])
	}
	c.until(packet.AccountFaction, seen) // end of the refreshed character list
	charID, _, found, err := db.CharacterByName(name)
	if err != nil || !found {
		t.Fatalf("created character not found: %v", err)
	}
	items, _ := db.CharacterItems(charID)
	detail, _ := db.CharacterDetail(testAccount, charID)
	t.Logf("starter items=%d start map=%d pos=(%.1f, %.1f, %.1f)", len(items), detail.MapID, detail.PosX, detail.PosY, detail.PosZ)
	t.Logf("created %s id=%d", name, charID)
	defer func() {
		if ok, err := db.DeleteCharacter(testAccount, charID); !ok || err != nil {
			t.Errorf("cleanup delete failed: %v", err)
		}
		// Remove the rows too, so test runs do not pile up pending deletions.
		pool, _ := db.Pool(db.GameData)
		if _, err := pool.Exec(`DELETE FROM dbo.CharItems WHERE CharID = @p1; DELETE FROM dbo.CharSkills WHERE CharID = @p1;
			DELETE FROM dbo._DeletedChars WHERE CharID = @p1; DELETE FROM dbo.Chars WHERE CharID = @p1`, int32(charID)); err != nil {
			t.Errorf("cleanup purge failed: %v", err)
		}
	}()

	sel := msg(packet.SelectCharacter, 0, 0, 0, 0)
	sel[2], sel[3], sel[4], sel[5] = byte(charID), byte(charID>>8), byte(charID>>16), byte(charID>>24)
	c.write(sel)
	if r := c.until(packet.SelectCharacter, seen); r[2] != 0 {
		t.Fatalf("select failed")
	}
	c.until(packet.CharacterSkillBar, seen)
	c.expanded = true
	c.until(packet.PointStoredItemList, seen)

	c.write(msg(packet.CharacterEnteredMap))
	c.until(packet.CharacterMapTeleport, seen)
	c.until(packet.CharacterMove, seen)

	move := packet.New(packet.CharacterMove)
	move.U8(1)
	move.U16(90)
	move.F32(510)
	move.F32(50)
	move.F32(510)
	m := move.MustFinish()
	c.write(m[2:])

	town := msg(packet.ChatNormal, 6)
	town = append(town, []byte("/town\x00")...)
	c.write(town)
	chat := c.until(packet.ChatNormal, seen)
	t.Logf("town reply: %q", chat[7:])

	c.write(msg(packet.Ping))
	c.until(packet.Ping, seen)

	c.write(msg(packet.Logout))
	c.until(packet.Logout, seen)
	c.expanded = false
	c.until(packet.AccountFaction, seen)

	ch, err := db.CharacterDetail(testAccount, charID)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("saved position map=%d (%.1f, %.1f, %.1f); opcodes seen: %v", ch.MapID, ch.PosX, ch.PosY, ch.PosZ, seen)
}
