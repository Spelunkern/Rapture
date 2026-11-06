//go:build probe

package main

import (
	"os"
	"sort"
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// TestTrafficProbe logs what a player standing still receives for 30 s.
func TestTrafficProbe(t *testing.T) {
	setupWorld(t)
	charID, uid, found, _ := db.CharacterByName("Valiant")
	if !found {
		t.Skip()
	}
	c := connect(t, uid)
	seen := map[packet.Opcode]int{}
	c.until(packet.AccountFaction, seen)
	c.write(msg(packet.SelectCharacter, byte(charID), byte(charID>>8), byte(charID>>16), byte(charID>>24)))
	c.until(packet.CharacterSkillBar, seen)
	c.expanded = true
	c.write(msg(packet.CharacterEnteredMap))
	c.until(packet.CharacterMove, seen)
	counts, bytes := map[packet.Opcode]int{}, map[packet.Opcode]int{}
	start := time.Now()
	detail, _ := db.CharacterDetail(uid, charID)
	x, z := detail.PosX, detail.PosZ
	if os.Getenv("PROBE_BORDER") != "" {
		// Stand on the cell border and step across it and back.
		x = float32(int(x/40)*40) + 40
	}
	go func() {
		// Walk a square at running speed, one move packet every 250 ms.
		for i := 0; time.Since(start) < 29*time.Second; i++ {
			step := float32(1.5)
			if os.Getenv("PROBE_BORDER") != "" {
				if (i/2)%2 == 0 {
					x += step
				} else {
					x -= step
				}
				i += 0
			} else {
				switch (i / 40) % 4 {
				case 0:
					x += step
				case 1:
					z += step
				case 2:
					x -= step
				case 3:
					z -= step
				}
			}
			m := packet.New(packet.CharacterMove)
			m.U8(1)
			m.U16(0)
			m.F32(x)
			m.F32(detail.PosY)
			m.F32(z)
			c.write(m.MustFinish()[2:])
			time.Sleep(250 * time.Millisecond)
		}
	}()
	for time.Since(start) < 30*time.Second {
		p := c.read(5 * time.Second)
		if p == nil {
			continue
		}
		counts[op(p)]++

		bytes[op(p)] += len(p) + 2
	}
	var ops []packet.Opcode
	for o := range counts {
		ops = append(ops, o)
	}
	sort.Slice(ops, func(i, j int) bool { return counts[ops[i]] > counts[ops[j]] })
	total := 0
	for _, o := range ops {
		total += counts[o]
		t.Logf("0x%04X: %d packets (%.1f/s), %d bytes", uint16(o), counts[o], float64(counts[o])/30, bytes[o])
	}
	t.Logf("total %d packets, %.1f/s", total, float64(total)/30)
	c.write(msg(packet.Logout))
}
