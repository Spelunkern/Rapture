package world

import (
	"testing"

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
