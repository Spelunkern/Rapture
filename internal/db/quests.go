package db

import (
	"database/sql"
	"fmt"
)

// CharQuest is an active quest row: Delay is minutes left (timed quests)
// or minutes elapsed (untimed), as the original saves it.
type CharQuest struct {
	QuestID uint16
	Delay   int16
	Counts  [3]uint8
}

// FinishedQuest is a finished quest row (Success 1 completed, 0 failed).
type FinishedQuest struct {
	QuestID uint16
	Success uint8
}

// CharacterQuests runs usp_Read_Char_Quest_R.
func CharacterQuests(charID uint32) ([]CharQuest, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return nil, err
	}
	c, cancel := ctx()
	defer cancel()
	rows, err := pool.QueryContext(c, "dbo.usp_Read_Char_Quest_R", sql.Named("CharID", int32(charID)))
	if err != nil {
		return nil, fmt.Errorf("usp_Read_Char_Quest_R: %w", err)
	}
	defer rows.Close()
	var out []CharQuest
	for rows.Next() {
		var q CharQuest
		var id int64
		if err := rows.Scan(&id, &q.Delay, &q.Counts[0], &q.Counts[1], &q.Counts[2]); err != nil {
			return nil, err
		}
		q.QuestID = clampU16(id)
		out = append(out, q)
	}
	return out, rows.Err()
}

// FinishedQuests runs usp_Read_Char_FinishedQuest_R.
func FinishedQuests(charID uint32) ([]FinishedQuest, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return nil, err
	}
	c, cancel := ctx()
	defer cancel()
	rows, err := pool.QueryContext(c, "dbo.usp_Read_Char_FinishedQuest_R", sql.Named("CharID", int32(charID)))
	if err != nil {
		return nil, fmt.Errorf("usp_Read_Char_FinishedQuest_R: %w", err)
	}
	defer rows.Close()
	var out []FinishedQuest
	for rows.Next() {
		var q FinishedQuest
		var id int64
		var success sql.NullInt64
		if err := rows.Scan(&id, &success); err != nil {
			return nil, err
		}
		q.QuestID, q.Success = clampU16(id), clampU8(success.Int64)
		out = append(out, q)
	}
	return out, rows.Err()
}

// AddCharacterQuest runs usp_Save_Char_Quest_Add_E.
func AddCharacterQuest(charID uint32, q CharQuest) error {
	return execProc(GameData, "dbo.usp_Save_Char_Quest_Add_E", sql.Named("CharID", int32(charID)),
		sql.Named("QuestID", int16(q.QuestID)), sql.Named("Delay", q.Delay), sql.Named("Count1", q.Counts[0]),
		sql.Named("Count2", q.Counts[1]), sql.Named("Count3", q.Counts[2]))
}

// SaveCharacterQuest runs usp_Save_Char_Quest_Mod_E.
func SaveCharacterQuest(charID uint32, q CharQuest) error {
	return execProc(GameData, "dbo.usp_Save_Char_Quest_Mod_E", sql.Named("CharID", int32(charID)),
		sql.Named("QuestID", int16(q.QuestID)), sql.Named("Delay", q.Delay), sql.Named("Count1", q.Counts[0]),
		sql.Named("Count2", q.Counts[1]), sql.Named("Count3", q.Counts[2]))
}

// FinishCharacterQuest runs usp_Save_Char_FinishedQuest_E on the active row.
func FinishCharacterQuest(charID uint32, questID uint16, success uint8) error {
	return execProc(GameData, "dbo.usp_Save_Char_FinishedQuest_E", sql.Named("CharID", int32(charID)),
		sql.Named("QuestID", int16(questID)), sql.Named("Success", success), sql.Named("Insert", false))
}

// DeleteCharacterQuest runs usp_Save_Char_Quest_Del_E (repeatable quests).
func DeleteCharacterQuest(charID uint32, questID uint16) error {
	return execProc(GameData, "dbo.usp_Save_Char_Quest_Del_E", sql.Named("CharID", int32(charID)),
		sql.Named("QuestID", int16(questID)))
}
