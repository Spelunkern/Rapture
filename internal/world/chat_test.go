package world

import (
	"io"
	"net"
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/crypto"
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
)

// TestWhisper follows PacketChatE's 0x1102: the target, found by name in
// the same country, gets direction 0, the sender's name and the text; a
// user of the other country gets nothing.
func TestWhisper(t *testing.T) {
	logger, err := logx.New("rapture_world_test")
	if err != nil {
		t.Fatal(err)
	}
	player := func(id uint32, name string, faction uint8) (*ConnectedPlayer, net.Conn) {
		a, b := net.Pipe()
		p := RegisterPlayer(db.Character{ID: id, Name: name, MapID: 60040}, nil, false, faction, &connWriter{conn: a}, logger)
		t.Cleanup(p.Unregister)
		return p, b
	}
	sender, _ := player(991001, "WhisperFrom", FactionLight)
	_, inbox := player(991002, "WhisperTo", FactionLight)
	_, other := player(991003, "WhisperFury", FactionDark)
	go io.Copy(io.Discard, other)
	s := &Session{char: &db.Character{ID: sender.CharacterID, Name: sender.CharacterName}, player: sender,
		faction: FactionLight, log: logger.Tagged("unit"), logger: logger}

	req := make([]byte, 0x18)
	req[0], req[1] = 0x02, 0x11
	copy(req[2:], "whisperto")
	req[0x17] = 3
	req = append(req, 'h', 'i', 0)
	got := make(chan []byte, 1)
	go func() {
		buf := make([]byte, 64)
		n, _ := io.ReadAtLeast(inbox, buf, 2)
		got <- append([]byte(nil), buf[:n]...)
	}()
	s.handleWhisper(req)
	var frame []byte
	select {
	case frame = <-got:
	case <-time.After(2 * time.Second):
		t.Fatal("whisper not delivered")
	}
	body := crypto.ExpandedXOR(frame[2:])
	if body[0] != 0x02 || body[1] != 0x11 || body[2] != 0 || string(body[3:14]) != "WhisperFrom" || body[24] != 3 || string(body[25:27]) != "hi" {
		t.Fatalf("whisper frame % x", body)
	}

	copy(req[2:], "WhisperFury\x00")
	s.handleWhisper(req) // other country: only the sender's 0x1106 (dropped without a client)
}
