//go:build integration

package world

import (
	"os"
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/config"
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
)

// TestQuestKillProgress counts mob kills on quest 1 (5x mob 2008, handed in
// at an NPC) without a client: counters advance and stop at the hand-in.
func TestQuestKillProgress(t *testing.T) {
	os.Chdir("../..")
	config.LoadRuntimeEnv()
	if err := db.Open(db.UserData, db.GameData, db.GameDefs); err != nil {
		t.Fatal(err)
	}
	db.Definitions()
	l, _ := logx.New("quest_test")
	if err := loadNpcQuest(l); err != nil {
		t.Fatal(err)
	}
	def := findQuestDef(1)
	s := &Session{log: l.Tagged("test"), logger: l, char: &db.Character{ID: 0}}
	s.quests = []*activeQuest{{ID: 1, def: def, startAt: time.Now()}}
	for range 7 {
		s.questKill(2008)
	}
	s.questKill(9999)
	q := s.findQuest(1)
	if q == nil || q.counters[0] != 7 || q.counters[1] != 0 {
		t.Fatalf("counters %+v", q)
	}
	if q.counters[0] < def.MobCounts[0] {
		t.Fatal("quest should be ready to hand in")
	}
	t.Logf("quest 1 counters %v (needs %d of mob %d, hand-in at npc %d/%d)", q.counters, def.MobCounts[0], def.MobIDs[0], def.EndNpcType, def.EndNpcID)
}
