package db

import (
	"database/sql"
	"fmt"
)

// CharacterSkill is one learned skill. Delay is the remaining cooldown in
// seconds as last saved (CharSkills.Delay).
type CharacterSkill struct {
	SkillID uint16
	Level   uint8
	Number  uint8
	Delay   int16
}

// CharacterSkills runs usp_Read_Char_Skills_R.
func CharacterSkills(charID uint32) ([]CharacterSkill, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return nil, err
	}
	c, cancel := ctx()
	defer cancel()
	rows, err := pool.QueryContext(c, "dbo.usp_Read_Char_Skills_R", sql.Named("CharID", int32(charID)))
	if err != nil {
		return nil, fmt.Errorf("usp_Read_Char_Skills_R: %w", err)
	}
	defer rows.Close()
	var skills []CharacterSkill
	for rows.Next() {
		var s CharacterSkill
		var id int64
		if err := rows.Scan(&id, &s.Level, &s.Number, &s.Delay); err != nil {
			return nil, err
		}
		s.SkillID = clampU16(id)
		skills = append(skills, s)
	}
	return skills, rows.Err()
}

// AddCharacterSkill runs usp_Save_Char_Skill_Add_E.
func AddCharacterSkill(charID uint32, s CharacterSkill) error {
	return execProc(GameData, "dbo.usp_Save_Char_Skill_Add_E", sql.Named("CharID", int32(charID)),
		sql.Named("SkillID", int16(s.SkillID)), sql.Named("SkillLevel", s.Level),
		sql.Named("Number", s.Number), sql.Named("Delay", s.Delay))
}

// SaveCharacterSkill runs usp_Save_Char_Skill_Mod_E (level and remaining
// cooldown).
func SaveCharacterSkill(charID uint32, s CharacterSkill) error {
	return execProc(GameData, "dbo.usp_Save_Char_Skill_Mod_E", sql.Named("CharID", int32(charID)),
		sql.Named("SkillID", int16(s.SkillID)), sql.Named("SkillLevel", s.Level), sql.Named("Delay", s.Delay))
}

// QuickSlot is one skill-bar entry.
type QuickSlot struct {
	QuickBar, QuickSlot, Bag uint8
	Number                   uint16
}

// QuickSlots runs usp_Read_Char_QuickSlots_R.
func QuickSlots(charID uint32) ([]QuickSlot, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return nil, err
	}
	c, cancel := ctx()
	defer cancel()
	rows, err := pool.QueryContext(c, "dbo.usp_Read_Char_QuickSlots_R", sql.Named("CharID", int32(charID)))
	if err != nil {
		return nil, fmt.Errorf("usp_Read_Char_QuickSlots_R: %w", err)
	}
	defer rows.Close()
	var slots []QuickSlot
	for rows.Next() {
		var q QuickSlot
		var number int64
		if err := rows.Scan(&q.QuickBar, &q.QuickSlot, &q.Bag, &number); err != nil {
			return nil, err
		}
		q.Number = clampU16(number)
		slots = append(slots, q)
	}
	return slots, rows.Err()
}

// SaveQuickSlots replaces the skill bar (usp_Save_Char_QuickSlot_Del_E then
// usp_Save_Char_QuickSlot_Add_E per entry, at most 128).
func SaveQuickSlots(charID uint32, slots []QuickSlot) error {
	if err := execProc(GameData, "dbo.usp_Save_Char_QuickSlot_Del_E", sql.Named("CharID", int32(charID))); err != nil {
		return err
	}
	for i, q := range slots {
		if i >= 128 {
			break
		}
		if err := execProc(GameData, "dbo.usp_Save_Char_QuickSlot_Add_E", sql.Named("CharID", int32(charID)),
			sql.Named("QuickBar", q.QuickBar), sql.Named("QuickSlot", q.QuickSlot), sql.Named("Bag", q.Bag),
			sql.Named("Number", int16(q.Number))); err != nil {
			return err
		}
	}
	return nil
}

// AppliedBuff is a persisted active buff with its remaining seconds.
type AppliedBuff struct {
	SkillID     uint16
	Level       uint8
	LeftSeconds uint32
}

// AppliedBuffs runs usp_Read_Char_ApplySkills_R.
func AppliedBuffs(charID uint32) ([]AppliedBuff, error) {
	pool, err := Pool(GameData)
	if err != nil {
		return nil, err
	}
	c, cancel := ctx()
	defer cancel()
	rows, err := pool.QueryContext(c, "dbo.usp_Read_Char_ApplySkills_R", sql.Named("CharID", int32(charID)))
	if err != nil {
		return nil, fmt.Errorf("usp_Read_Char_ApplySkills_R: %w", err)
	}
	defer rows.Close()
	var buffs []AppliedBuff
	for rows.Next() {
		var b AppliedBuff
		var id int64
		var left int32
		if err := rows.Scan(&id, &b.Level, &left); err != nil {
			return nil, err
		}
		b.SkillID, b.LeftSeconds = clampU16(id), uint32(max(left, 0))
		buffs = append(buffs, b)
	}
	return buffs, rows.Err()
}

// SaveAppliedBuffs replaces the persisted buffs
// (usp_Save_Char_ApplySkill_Del_E with DeleteAll, then
// usp_Save_Char_ApplySkill_Add_E2 per buff).
func SaveAppliedBuffs(charID uint32, buffs []AppliedBuff) error {
	if err := execProc(GameData, "dbo.usp_Save_Char_ApplySkill_Del_E", sql.Named("CharID", int32(charID)),
		sql.Named("SkillID", int16(0)), sql.Named("SkillLevel", uint8(0)), sql.Named("DeleteAll", true)); err != nil {
		return err
	}
	for _, b := range buffs {
		if err := execProc(GameData, "dbo.usp_Save_Char_ApplySkill_Add_E2", sql.Named("CharID", int32(charID)),
			sql.Named("SkillID", int16(b.SkillID)), sql.Named("SkillLevel", b.Level),
			sql.Named("LeftResetTime", int32(b.LeftSeconds))); err != nil {
			return err
		}
	}
	return nil
}
