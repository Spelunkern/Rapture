//go:build integration

package db

import (
	"os"
	"testing"

	"github.com/Spelunkern/Rapture/internal/config"
)

// Exercises the character, item and skill procedures on account normal5
// and removes everything it created.
func TestCharacterLifecycle(t *testing.T) {
	os.Chdir("../..")
	config.LoadRuntimeEnv()
	account, err := AccountByUID(10)
	if err != nil {
		t.Fatal(err)
	}
	pool, _ := Pool(GameData)
	defer pool.Exec(`DELETE FROM dbo.CharItems WHERE CharID IN (SELECT CharID FROM dbo.Chars WHERE CharName='ItTestChar');
		DELETE FROM dbo.CharSkills WHERE CharID IN (SELECT CharID FROM dbo.Chars WHERE CharName='ItTestChar');
		DELETE FROM dbo._DeletedChars WHERE CharID IN (SELECT CharID FROM dbo.Chars WHERE CharName='ItTestChar');
		DELETE FROM dbo.Chars WHERE CharName='ItTestChar'`)

	id, err := CreateCharacter(account, NewCharacter{Slot: 0, Race: 0, Class: 0, Name: "ItTestChar",
		MapID: 1, PosX: 500, PosY: 50, PosZ: 500}, 100, 100, 100)
	if err != nil {
		t.Fatal(err)
	}
	if taken, _ := NameTaken("ItTestChar"); !taken {
		t.Fatal("name not taken after create")
	}
	list, err := CharactersForAccount(10)
	if err != nil || len(list) != 1 || list[0].ID != id {
		t.Fatalf("list = %+v, %v", list, err)
	}
	ch, err := CharacterDetail(10, id)
	if err != nil || ch.Name != "ItTestChar" || ch.SkillPoint != 5 || ch.PosX != 500 {
		t.Fatalf("detail = %+v, %v", ch, err)
	}
	if _, err := CharacterDetail(9, id); err != ErrNotFound {
		t.Fatalf("other account read: %v", err)
	}
	ch.Level, ch.Exp, ch.Money, ch.PosX = 7, 1234, 3_500_000_000, 42.5
	if err := SaveCharacter(&ch); err != nil {
		t.Fatal(err)
	}
	if ch2, _ := CharacterDetail(10, id); ch2.Level != 7 || ch2.Money != 3_500_000_000 || ch2.PosX != 42.5 {
		t.Fatalf("saved = %+v", ch2)
	}

	item := Item{Bag: 1, Slot: 3, Type: 25, TypeID: 1, Quality: 0, Count: 5}
	if err := AddCharacterItem(id, &item); err != nil {
		t.Fatal(err)
	}
	item.Count, item.Slot = 2, 4
	if err := SaveCharacterItem(id, &item); err != nil {
		t.Fatal(err)
	}
	items, _ := CharacterItems(id)
	if len(items) != 1 || items[0].Count != 2 || items[0].Slot != 4 || items[0].UID != item.UID {
		t.Fatalf("items = %+v", items)
	}
	if err := DeleteCharacterItems(id, item.UID); err != nil {
		t.Fatal(err)
	}
	if items, _ := CharacterItems(id); len(items) != 0 {
		t.Fatalf("items after delete = %+v", items)
	}

	if err := AddCharacterSkill(id, CharacterSkill{SkillID: 1, Level: 1, Number: 0}); err != nil {
		t.Fatal(err)
	}
	if err := SaveCharacterSkill(id, CharacterSkill{SkillID: 1, Level: 2, Delay: 30}); err != nil {
		t.Fatal(err)
	}
	if skills, _ := CharacterSkills(id); len(skills) != 1 || skills[0].Level != 2 || skills[0].Delay != 30 {
		t.Fatalf("skills = %+v", skills)
	}

	if ok, err := DeleteCharacter(10, id); !ok || err != nil {
		t.Fatalf("delete = %v %v", ok, err)
	}
	if list, _ := CharactersForAccount(10); len(list) != 1 || !list[0].PendingDelete {
		t.Fatalf("pending list = %+v", list)
	}
	if ok, err := RestoreCharacter(10, id); !ok || err != nil {
		t.Fatalf("restore = %v %v", ok, err)
	}
	if country, maxGrow, err := ReadMaxGrow(10); err != nil || maxGrow != 3 {
		t.Fatalf("maxgrow = %d %d %v", country, maxGrow, err)
	}
	if defs, err := Definitions(); err != nil || len(defs.Items) == 0 {
		t.Fatal(err)
	}
}
