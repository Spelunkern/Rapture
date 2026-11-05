// Package session hands an authenticated login over to the world server.
// Login writes runtime/sessions/<userUID>.bin; the client then connects to
// world and presents the same identity token in its 0xA301 handshake.
package session

import (
	"crypto/rand"
	"crypto/subtle"
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

const dir = "runtime/sessions"

// MaxAge bounds how long a handoff stays valid after login.
const MaxAge = 10 * time.Minute

type Handoff struct {
	Identity    [16]byte
	AESMaterial [32]byte // login AES key (16) + IV (16)
	UserUID     uint32
	SessionID   int64
}

// NewIdentity returns a fresh random token for one login.
func NewIdentity() [16]byte {
	var id [16]byte
	if _, err := rand.Read(id[:]); err != nil {
		panic(err)
	}
	return id
}

// NewSessionID returns the bigint session id passed to the login procedures.
func NewSessionID() int64 {
	var b [8]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic(err)
	}
	return int64(binary.LittleEndian.Uint64(b[:]) & 0x7FFF_FFFF_FFFF_FFFF)
}

func path(userUID uint32) string { return filepath.Join(dir, fmt.Sprintf("%d.bin", userUID)) }

// Save writes the handoff atomically (owner-only permissions).
func Save(h Handoff) error {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	buf := make([]byte, 0, 16+32+4+8)
	buf = append(buf, h.Identity[:]...)
	buf = append(buf, h.AESMaterial[:]...)
	buf = binary.LittleEndian.AppendUint32(buf, h.UserUID)
	buf = binary.LittleEndian.AppendUint64(buf, uint64(h.SessionID))
	tmp := path(h.UserUID) + ".tmp"
	if err := os.WriteFile(tmp, buf, 0o600); err != nil {
		return err
	}
	return os.Rename(tmp, path(h.UserUID))
}

var ErrInvalid = errors.New("no valid login handoff")

// Consume loads and deletes the handoff for userUID if identity matches and
// it is younger than MaxAge.
func Consume(userUID uint32, identity [16]byte) (Handoff, error) {
	p := path(userUID)
	info, err := os.Stat(p)
	if err != nil || time.Since(info.ModTime()) > MaxAge {
		return Handoff{}, ErrInvalid
	}
	buf, err := os.ReadFile(p)
	if err != nil || len(buf) != 16+32+4+8 {
		return Handoff{}, ErrInvalid
	}
	var h Handoff
	copy(h.Identity[:], buf[0:16])
	copy(h.AESMaterial[:], buf[16:48])
	h.UserUID = binary.LittleEndian.Uint32(buf[48:52])
	h.SessionID = int64(binary.LittleEndian.Uint64(buf[52:60]))
	if h.UserUID != userUID || subtle.ConstantTimeCompare(h.Identity[:], identity[:]) != 1 {
		return Handoff{}, ErrInvalid
	}
	os.Remove(p)
	return h, nil
}
