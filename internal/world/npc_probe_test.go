//go:build probe

package world

import (
	"os"
	"testing"

	"github.com/Spelunkern/Rapture/internal/logx"
)

func TestNpcMoveValues(t *testing.T) {
	os.Chdir("../..")
	l, _ := logx.New("probe")
	if err := loadNpcQuest(l); err != nil {
		t.Fatal(err)
	}
	counts := map[[2]int32]int{}
	for g := range npcDefs {
		for _, n := range npcDefs[g] {
			counts[[2]int32{n.MoveDistance, n.MoveSpeed}]++
		}
	}
	t.Logf("distance/speed -> count: %v", counts)
}
