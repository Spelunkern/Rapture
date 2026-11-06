package world

import (
	"errors"

	"github.com/Spelunkern/Rapture/internal/packet"
)

// labeled pairs a frame with its log label.
type labeled struct {
	Packet []byte
	Label  string
}

func buildWorldHandshake() []byte {
	w := packet.New(packet.GameHandshake)
	w.U8(0)
	w.U8(2)
	w.Bytes([]byte{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16})
	return w.MustFinish()
}

func buildAccountFaction(faction, maxMode uint8) []byte {
	w := packet.New(packet.AccountFaction)
	w.U8(faction)
	w.U8(maxMode)
	return w.MustFinish()
}

func isValidFaction(f uint8) bool { return f == FactionLight || f == FactionDark || f == FactionNone }

func resultByte(ok bool) uint8 {
	if ok {
		return 0
	}
	return 1
}

// buildCreateCharacterResult: one byte, 0 ok, 1 failure ("name in use").
func buildCreateCharacterResult(ok bool) []byte {
	w := packet.New(packet.CreateCharacter)
	w.U8(resultByte(ok))
	return w.MustFinish()
}

func buildCharResult(op packet.Opcode, ok bool, characterID uint32) []byte {
	w := packet.New(op)
	w.U8(resultByte(ok))
	w.U32(characterID)
	return w.MustFinish()
}

// buildRenameResult uses the inverted convention: 1 ok, 0 failure.
func buildRenameResult(ok bool) []byte {
	w := packet.New(packet.RenameCharacter)
	w.Bool(ok)
	return w.MustFinish()
}

// shaiyaTime encodes 2020-01-01 12:30:00 in the client's packed format.
func shaiyaTime() int32 {
	year, month, day, hour, minute, second := int32(2020), int32(1), int32(1), int32(12), int32(30), int32(0)
	n1 := 16 * (year - 16)
	n2 := 32 * (month + n1)
	return second + ((minute + ((hour + 32*(day+n2)) << 6)) << 6)
}

func buildWorldDay() []byte {
	w := packet.New(packet.WorldDay)
	w.I32(shaiyaTime())
	return w.MustFinish()
}

func buildKillCountUpdate(index uint8, value uint32) []byte {
	w := packet.New(packet.UserKillCountUpdate)
	w.U8(index)
	w.U32(value)
	return w.MustFinish()
}

func buildZeroCount(op packet.Opcode) []byte {
	w := packet.New(op)
	w.U8(0)
	return w.MustFinish()
}

func buildBlessInit(faction uint8) []byte {
	w := packet.New(packet.BlessInit)
	w.U8(faction)
	w.I32(0)
	w.U32(0)
	return w.MustFinish()
}

func buildAccountPoints(points uint32) []byte {
	w := packet.New(packet.AccountPoints)
	w.U32(points)
	w.U8(0)
	return w.MustFinish()
}

func buildCashPoints(points uint32) []byte {
	w := packet.New(packet.CashPoint)
	w.U32(points)
	return w.MustFinish()
}

func buildPing() []byte {
	w := packet.New(packet.Ping)
	w.U16(0)
	return w.MustFinish()
}

// Chat text is the client's single-byte ANSI codepage, mapped 1:1 to
// Latin-1 code points.
func decodeLatin1(b []byte) string {
	r := make([]rune, len(b))
	for i, c := range b {
		r[i] = rune(c)
	}
	return string(r)
}

func encodeLatin1(s string) []byte {
	out := make([]byte, 0, len(s))
	for _, r := range s {
		if r <= 0xFF {
			out = append(out, byte(r))
		} else {
			out = append(out, '?')
		}
	}
	return out
}

const maxChatMessageLen = 127

var errChat = errors.New("malformed chat packet")

// readChatMessage parses length:u8 (including the terminator) + text.
func readChatMessage(payload []byte) (string, error) {
	if len(payload) < 3 {
		return "", errChat
	}
	n := int(payload[2])
	if len(payload) < 3+n {
		return "", errChat
	}
	b := payload[3 : 3+n]
	for i, c := range b {
		if c == 0 {
			b = b[:i]
			break
		}
	}
	return decodeLatin1(b), nil
}

func writeChatText(w *packet.Writer, message string) {
	b := encodeLatin1(message)
	if len(b) > maxChatMessageLen {
		b = b[:maxChatMessageLen]
	}
	b = append(b, 0)
	w.U8(uint8(len(b)))
	w.Bytes(b)
}

func buildChatByID(op packet.Opcode, senderID uint32, message string) []byte {
	w := packet.New(op)
	w.U32(senderID)
	writeChatText(w, message)
	return w.MustFinish()
}

func buildChatNormal(characterID uint32, message string) []byte {
	return buildChatByID(packet.ChatNormal, characterID, message)
}

func buildChatByName(op packet.Opcode, sender, message string) []byte {
	w := packet.New(op)
	name := sender
	if len(name) > 21 {
		name = name[:21]
	}
	w.FixedString(name, 21)
	writeChatText(w, message)
	return w.MustFinish()
}

func buildChatError(result uint8) []byte {
	w := packet.New(packet.ChatError)
	w.U8(result)
	return w.MustFinish()
}

func isChatOpcode(op packet.Opcode) bool {
	switch op {
	case packet.ChatNormal, packet.ChatWorld, packet.ChatMap, packet.ChatNormalAdmin, packet.ChatWorldAdmin:
		return true
	}
	return false
}
