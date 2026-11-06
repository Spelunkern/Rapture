//go:build integration

package main

import (
	"encoding/binary"
	"math"
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
	"github.com/Spelunkern/Rapture/internal/world"
)

// TestCombatAgainstSQLServer plays the armed test character Valiant: it
// walks onto a low-level mob, uses an attack skill, auto-attacks it to
// death and checks the death, the 0x0503 results and that every drop
// (0x0401) carries Y 0 as CMob::GetDropPosRand sends it.
func TestCombatAgainstSQLServer(t *testing.T) {
	setupWorld(t)
	charID, uid, found, err := db.CharacterByName("Valiant")
	if err != nil || !found {
		t.Skip("Valiant not found")
	}
	saved, err := db.CharacterDetail(uid, charID)
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		pool, _ := db.Pool(db.GameData)
		pool.Exec(`UPDATE dbo.Chars SET Map = @p2, PosX = @p3, PosY = @p4, PosZ = @p5, HP = @p6, MP = @p7, SP = @p8 WHERE CharID = @p1`,
			int32(charID), int32(saved.MapID), saved.PosX, saved.PosY, saved.PosZ, int32(saved.CurrentHP), int32(saved.CurrentMP), int32(saved.CurrentSP))
	}()

	c := connect(t, uid)
	seen := map[packet.Opcode]int{}
	c.until(packet.AccountFaction, seen)
	c.write(msg(packet.SelectCharacter, byte(charID), byte(charID>>8), byte(charID>>16), byte(charID>>24)))
	c.until(packet.CharacterSkillBar, seen)
	c.expanded = true
	c.write(msg(packet.CharacterEnteredMap))
	c.until(packet.CharacterMove, seen)

	var target world.MapMob
	for _, m := range world.EntitiesOnMap(saved.MapID).Mobs {
		if m.CurrentHP > 0 && m.Level <= 20 && m.MaxHP <= 3000 {
			target = m
			break
		}
	}
	if target.ID == 0 {
		t.Skipf("no low-level mob on map %d", saved.MapID)
	}
	move := packet.New(packet.CharacterMove)
	move.U8(1)
	move.U16(0)
	move.F32(target.PosX + 0.5)
	move.F32(saved.PosY)
	move.F32(target.PosZ)
	c.write(move.MustFinish()[2:])
	le := func(v uint32) []byte { return binary.LittleEndian.AppendUint32(nil, v) }

	// An attack skill the character knows, on the selected mob (0x0517).
	skills, _ := db.CharacterSkills(charID)
	for _, k := range skills {
		d := db.MustDefs().Skill(k.SkillID, k.Level)
		if d != nil && d.TypeEffect == 1 && d.TargetType == 3 && d.ReadyTime == 0 && d.TypeDetail == 21 {
			c.write(msg(packet.SkillUse, append([]byte{k.Number}, le(target.ID)...)...))
			var reply []byte
			for end := time.Now().Add(3 * time.Second); reply == nil && time.Now().Before(end); {
				if q := c.read(500 * time.Millisecond); q != nil && op(q) == packet.SkillUse {
					reply = q
				}
			}
			if reply == nil {
				t.Logf("skill %d/%d gave no reply (still cooling down from an earlier run)", k.SkillID, k.Level)
				break
			}
			if got, _ := packet.U32(reply[7:]); got != target.ID || reply[2] > 3 {
				t.Fatalf("skill reply result %d target %d", reply[2], got)
			}
			hp, _ := packet.U16(reply[14:])
			t.Logf("skill %d/%d result %d damage %d", k.SkillID, k.Level, reply[2], hp)
			break
		}
	}

	c.write(msg(packet.CharacterMobAutoAttack, le(target.ID)...))
	deadline := time.Now().Add(60 * time.Second)
	swings, drops := 0, 0
	for time.Now().Before(deadline) {
		p := c.read(5 * time.Second)
		if p == nil {
			continue
		}
		seen[op(p)]++
		switch op(p) {
		case packet.CharacterMobAutoAttack:
			if got, _ := packet.U32(p[7:]); got == target.ID {
				swings++
				if p[2] != 0 && p[2] != 1 && p[2] != 3 {
					t.Fatalf("auto attack result %d", p[2])
				}
			}
		case packet.MapAddItem:
			drops++
			if y := math.Float32frombits(binary.LittleEndian.Uint32(p[14:])); y != 0 {
				t.Fatalf("drop Y %.2f, the original sends 0", y)
			}
		case packet.MobDeath:
			if got, _ := packet.U32(p[2:]); got == target.ID {
				// Drops follow the death notice in the same burst.
				for end := time.Now().Add(2 * time.Second); time.Now().Before(end); {
					q := c.read(500 * time.Millisecond)
					if q == nil {
						continue
					}
					if op(q) == packet.MapAddItem {
						drops++
						if y := math.Float32frombits(binary.LittleEndian.Uint32(q[14:])); y != 0 {
							t.Fatalf("drop Y %.2f, the original sends 0", y)
						}
					}
				}
				t.Logf("mob %d (id %d) died after %d swings, %d drops; opcodes %v", target.MobID, target.ID, swings, drops, seen)
				c.write(msg(packet.Logout))
				return
			}
		}
	}
	t.Fatalf("mob %d did not die (swings %d, seen %v)", target.ID, swings, seen)
}
