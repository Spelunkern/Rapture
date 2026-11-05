//go:build integration

// Run with: go test -tags integration ./cmd/rapture_login
// Needs the SQL Server from tools/sqlserver.py and the admin1 test account.
package main

import (
	"crypto/rand"
	"io"
	"math/big"
	"net"
	"os"
	"slices"
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/config"
	"github.com/Spelunkern/Rapture/internal/crypto"
	"github.com/Spelunkern/Rapture/internal/logx"
	"github.com/Spelunkern/Rapture/internal/packet"
)

func readFrame(t *testing.T, conn net.Conn) []byte {
	t.Helper()
	conn.SetReadDeadline(time.Now().Add(10 * time.Second))
	head := make([]byte, 2)
	if _, err := io.ReadFull(conn, head); err != nil {
		t.Fatal(err)
	}
	frame := make([]byte, int(head[0])|int(head[1])<<8)
	copy(frame, head)
	if _, err := io.ReadFull(conn, frame[2:]); err != nil {
		t.Fatal(err)
	}
	return frame
}

func le(b []byte) *big.Int { r := slices.Clone(b); slices.Reverse(r); return new(big.Int).SetBytes(r) }

func TestLoginAgainstSQLServer(t *testing.T) {
	os.Chdir("../..")
	config.LoadRuntimeEnv()
	logger, err := logx.New("rapture_login_test")
	if err != nil {
		t.Fatal(err)
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	go func() {
		conn, err := listener.Accept()
		if err == nil {
			handle(conn, logger.Tagged("test"))
			conn.Close()
		}
	}()
	conn, err := net.Dial("tcp", listener.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()

	hello := readFrame(t, conn)
	if op, _ := packet.U16(hello[2:]); packet.Opcode(op) != packet.LoginHandshake {
		t.Fatalf("first opcode 0x%04x", op)
	}
	e, n := le(hello[7:71]), le(hello[71:199])
	secret := make([]byte, 64)
	rand.Read(secret)
	cipher := new(big.Int).Exp(le(secret), e, n).Bytes()
	slices.Reverse(cipher)
	w := packet.New(packet.LoginHandshake)
	w.U8(uint8(len(cipher)))
	w.Bytes(cipher)
	conn.Write(w.MustFinish())

	material := crypto.DeriveLoginAESMaterial(crypto.DecryptLoginRSA(cipher))
	var key, iv [16]byte
	copy(key[:], material[:16])
	copy(iv[:], material[16:])
	send, recv := crypto.NewAESCTR(key, iv), crypto.NewAESCTR(key, iv)

	login := make([]byte, 50)
	copy(login[0:], []byte{0x02, 0xA1})
	copy(login[2:], "admin1")
	copy(login[34:], os.Getenv("RAPTURE_TEST_PASSWORD"))
	frame := append([]byte{byte(len(login) + 2), 0}, send.Transform(login)...)
	conn.Write(frame)

	reply := readFrame(t, conn)
	body := recv.Transform(reply[2:])
	if op, _ := packet.U16(body); packet.Opcode(op) != packet.LoginRequest {
		t.Fatalf("reply opcode 0x%04x", op)
	}
	if body[2] != 0 {
		t.Fatalf("login result %d", body[2])
	}
	uid, _ := packet.U32(body[3:])
	t.Logf("login ok user_uid=%d permission=%d", uid, body[7])
	list := recv.Transform(readFrame(t, conn)[2:])
	if op, _ := packet.U16(list); packet.Opcode(op) != packet.ServerList {
		t.Fatalf("expected server list, got 0x%04x", op)
	}
	if _, err := os.Stat("runtime/sessions/1.bin"); err != nil {
		t.Fatalf("handoff missing: %v", err)
	}
}
