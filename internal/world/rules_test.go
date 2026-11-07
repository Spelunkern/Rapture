package world

import (
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
)

func TestMobExperience(t *testing.T) {
	cases := []struct {
		mob, base, user uint16
		want            uint32
	}{
		{10, 100, 10, 120}, // same level: 1 + 0.2
		{12, 100, 10, 160}, // two above: 1 + 0.6
		{20, 100, 10, 0},   // more than 9 above: nothing
		{19, 100, 10, 300}, // nine above: 1 + 2.0
		{4, 100, 10, 0},    // six below: factor 0
		{5, 100, 10, 19},   // five below: 100 * (1 - 0.8) truncates to 19, as the original
	}
	for _, c := range cases {
		if got := mobExperience(c.mob, c.base, c.user); got != c.want {
			t.Errorf("mobExperience(%d, %d, %d) = %d, want %d", c.mob, c.base, c.user, got, c.want)
		}
	}
}

func TestStartingStatsAndJobs(t *testing.T) {
	str, dex, rec, intel, wis, luc := startingStats(0, 0) // human fighter
	if [6]uint16{str, dex, rec, intel, wis, luc} != [6]uint16{14, 12, 9, 8, 7, 15} {
		t.Errorf("human fighter stats = %v", []uint16{str, dex, rec, intel, wis, luc})
	}
	if !allowedJob(0, 5) || allowedJob(0, 2) || !allowedJob(3, 3) || allowedJob(4, 0) {
		t.Error("family/job mask mismatch")
	}
}

func TestLevelUpAndReset(t *testing.T) {
	ch := &db.Character{Class: 4, Mode: growHard, Level: 1, Int: 15}
	for range 9 {
		levelUp(ch)
	}
	if ch.Level != 10 || ch.Int != 24 || ch.StatPoint != 63 || ch.SkillPoint != 36 {
		t.Errorf("after 9 hard level-ups: level %d int %d stat %d skill %d", ch.Level, ch.Int, ch.StatPoint, ch.SkillPoint)
	}
	ch.Race, ch.Wis, ch.StatPoint = 2, 40, 0
	resetStats(ch)
	if ch.Int != 15+2+9 || ch.Wis != 12+2 || ch.StatPoint != 63 {
		t.Errorf("reset: int %d wis %d stat %d", ch.Int, ch.Wis, ch.StatPoint)
	}
}

// TestBindPosition follows CMap::GetBindPos: the nearest group by its first
// box's lower corner among the faction's (type 1 light, 2 fury, 0/3 both),
// a random whole-unit point inside the box 0.9 above its floor, and group 0
// when no group admits the faction.
func TestBindPosition(t *testing.T) {
	const id = 60001
	RegisterMapGrid(id, 2048, nil)
	AddEntities(id, MapEntities{SpawnAreas: []MapSpawnArea{
		{Faction: 2, Boxes: []SpawnBox{{100, 10, 100, 110, 12, 104}}},
		{Faction: 1, Boxes: []SpawnBox{{500, 20, 500, 520, 22, 530}}},
		{Faction: 1, Boxes: []SpawnBox{{900, 30, 900, 905, 32, 905}}},
		{Faction: 3, Boxes: []SpawnBox{{1500, 40, 1500, 1501, 42, 1501}}},
	}})
	for range 50 {
		m, x, y, z := bindPosition(id, FactionLight, 880, 0, 880)
		if m != id || x < 900 || x >= 905 || z < 900 || z >= 905 || y != 30.9 || x != float32(int(x)) {
			t.Fatalf("light near 900: map %d (%v, %v, %v)", m, x, y, z)
		}
	}
	if _, x, _, _ := bindPosition(id, FactionLight, 600, 0, 600); x < 500 || x >= 520 {
		t.Fatalf("light near 600 picked x %v", x)
	}
	if _, x, _, _ := bindPosition(id, FactionDark, 1400, 0, 1400); x != 1500 {
		t.Fatalf("fury near 1400 should take the shared group, x %v", x)
	}
	const furyOnly = 60002
	RegisterMapGrid(furyOnly, 2048, nil)
	AddEntities(furyOnly, MapEntities{SpawnAreas: []MapSpawnArea{
		{Faction: 2, Boxes: []SpawnBox{{100, 10, 100, 110, 12, 104}}},
		{Faction: 2, Boxes: []SpawnBox{{900, 10, 900, 910, 12, 904}}},
	}})
	if _, x, _, _ := bindPosition(furyOnly, FactionLight, 900, 0, 900); x < 100 || x >= 110 {
		t.Fatalf("no admitted group should fall back to group 0, x %v", x)
	}
}

// TestReturnToTown follows 0x0556: the bind-point move fires 10 s later
// and a move in between cancels it.
func TestReturnToTown(t *testing.T) {
	s := &Session{char: &db.Character{ID: 1, MapID: 1}}
	s.handleReturnToTown()
	if s.returnAt.IsZero() || time.Until(s.returnAt) < 9*time.Second {
		t.Fatalf("return not scheduled 10 s ahead: %v", s.returnAt)
	}
	first := s.returnAt
	s.handleReturnToTown() // a second request does not restart it
	if s.returnAt != first {
		t.Fatal("pending return restarted")
	}
	s.cancelMove()
	if !s.returnAt.IsZero() {
		t.Fatal("cancel kept the return")
	}
	s.dead = true
	s.handleReturnToTown()
	if !s.returnAt.IsZero() {
		t.Fatal("scheduled while dead")
	}
}
