//go:build integration

package db

import (
	"os"
	"testing"

	"github.com/Spelunkern/Rapture/internal/config"
)

func TestLoadDefinitions(t *testing.T) {
	os.Chdir("../..")
	config.LoadRuntimeEnv()
	d, err := Definitions()
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("items=%d grades=%d skills=%d mobs=%d drops=%d exp=%d products=%d",
		len(d.Items), len(d.ItemsByGrade), len(d.Skills), len(d.Mobs), len(d.MobDrops), len(d.Exp), len(d.Products))
	if len(d.Items) == 0 || len(d.Skills) == 0 || len(d.Mobs) == 0 || len(d.Exp) == 0 {
		t.Fatal("empty definitions")
	}
}
