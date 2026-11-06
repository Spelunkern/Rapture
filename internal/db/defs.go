package db

import (
	"fmt"
	"strings"
	"sync"
)

// PS_GameDefs is static game content. Like the original server, Rapture
// loads it once at world startup and serves lookups from memory.

type ItemKey struct{ Type, TypeID uint8 }

// ItemDef is one PS_GameDefs.dbo.Items row.
type ItemDef struct {
	ItemID                                 int32
	Name                                   string
	Type, TypeID                           uint8
	ReqLevel                               uint16
	Country                                uint8
	Classes                                [6]bool // Attackfighter..Defensemage, indexed by Job
	Grow                                   uint8
	ReqStr, ReqDex, ReqRec, ReqInt, ReqWis int16
	ReqLuc                                 int32
	ReqIg                                  uint8
	Range                                  int16
	AttackTime                             uint8
	Special                                uint8
	Attrib                                 uint8
	Slot                                   uint8
	Quality                                uint16
	Effect1, Effect2, Effect3, Effect4     int16
	ConstHP, ConstSP, ConstMP              int16
	ConstStr, ConstDex, ConstRec           int16
	ConstInt, ConstWis, ConstLuc           int16
	Buy, Sell                              int32
	Grade                                  uint16
	Drop                                   uint16 // drop-limit interval selector (0 = unlimited)
	Server                                 uint8
	Count                                  uint8
	ReqVg                                  int16 // lapisia: breaks the item on a failed enchant
	ExpAbsorb                              uint8 // Items.Exp: damage absorbed while equipped
}

// MaxStack is the stack size (Items.Count, at least 1).
func (d *ItemDef) MaxStack() uint8 { return max(d.Count, 1) }

type SkillKey struct {
	SkillID uint16
	Level   uint8
}

// SkillDef is one PS_GameDefs.dbo.Skills row. ClassEligible is indexed by
// Chars.Job: Attackfighter, Defensefighter, Patrolrogue, Shootrogue,
// Attackmage, Defensemage. EP4.5 has three AbilityType/Value pairs.
type SkillDef struct {
	SkillID                                  uint16
	SkillLevel                               uint8
	Name                                     string
	Country                                  uint8
	ReqLevel                                 uint16
	Grow                                     uint8
	SkillPointCost                           uint8
	PrevSkillID                              uint16
	ClassEligible                            [6]bool
	MPCost, SPCost                           uint16
	ResetTimeSeconds                         uint16
	DamageHP, AddDamageHP                    int32
	HealHP, HealSP, HealMP                   int32
	TypeDetail                               uint16
	KeepTime                                 uint16
	TimeHealHP, TimeHealSP, TimeHealMP       int32
	ReadyTime                                uint8
	AbilityType                              [3]uint8
	AbilityValue                             [3]int16
	TimeDamageHP, TimeDamageSP, TimeDamageMP int32
	AttackRange                              uint8
	TypeAttack                               uint8
	TargetType, ApplyRange, MultiAttack      uint8

	// CGameData::LoadConstSkill (0x004bffe0) normalizes these on load.
	Kind           uint8 // clamp(TypeAttack, 1, 3) - 1: 0 melee, 1 ranged, 2 magic
	TypeEffect     uint8 // 0 passive, 1 attack, 2 heal, 3 debuff, 4 buff, ...
	DamageType     uint8 // AttackDamage mode 0..5
	TimeDamageType uint8 // TimeDamage mode 0, 3, 4, 5
	TypeShow       uint8 // 0: not learnable
	DamageSP       int32
	DamageMP       int32
	AddDamageSP    int32
	AddDamageMP    int32
	AttrType       uint8 // element override, 0 when > 8
	StateType      uint8 // 0 when > 15
	SuccessType    uint8
	SuccessValue   uint8
	Weapon1        uint8 // passive weapon types, at most 19
	Weapon2        uint8
	WeaponValue    uint8
	DefenceType    uint8 // clamp(DefenceType, 1, 3) - 1
	DefenceValue   uint8
	Aggro          uint16 // Arrow
	LimitHP        uint8
	KeepSeconds    uint32 // KeepTime, in hours when FixRange is 1
	KeepOnDeath    bool   // FixRange != 0: survives CUser::ClearApplySkillByDeath
	NeedWeapon     [15]bool
	NeedShield     bool
	MobSkill       bool // SkillLevel 100 rows are mob skills
}

// MobAttack is one of a mob's three attack slots (AttackTypeN..AttackokN).
type MobAttack struct {
	Type         uint16 // 0 melee, 1 ranged, 2 magic
	TimeMS       uint32
	Range        uint16
	Attack, Plus uint16 // base damage and random variance
	Attrib       uint8
	Special      uint8
	OK           uint8 // 0 unused, 1/2 always, 3 HP 20-40%, 4 HP <= 5%, 5 HP < 20%, 6 target the weakest attacker
}

// MobDef is the subset of PS_GameDefs.dbo.Mobs the world uses.
type MobDef struct {
	MobID              uint16
	Name               string
	Level              uint16
	MaxHP              uint32
	MaxMP, MaxSP       uint32
	MoneyMin, MoneyMax uint32 // Money1 (base) and Money2 (random range)
	RespawnSeconds     uint64 // 0: does not respawn
	Exp                uint32
	Attack             uint32
	AI                 uint8
	ChaseRange         float32
	Dex, Wis, Luc      uint16
	Defense, Magic     uint16
	ShieldDefense      uint32 // Attacktime3, as the original reads it
	Attrib             uint8
	Size               uint8
	QuestItemID        int32 // Type*1000+TypeID of the quest item it may drop
	NormalTimeMS       uint32
	NormalStep         uint16
	ChaseTimeMS        uint32
	ChaseStep          uint16
	Attacks            [3]MobAttack
	// CGameData::LoadConstMob reuses attack slot 3: Attack3 is the 5 s
	// recovery (nRecoverValue), Attackplus3 != 0 an obelisk, Attackattrib3
	// the country (1 light -> 0, 2 fury -> 1, otherwise 2: none).
	RecoverValue int32
	Obelisk      bool
	Country      uint8
	ResistState  [15]uint8 // immune to skills of StateType i+1 when non-zero
	ResistSkill  [6]uint16 // immune to these skill IDs
}

type MobDrop struct {
	MobID     uint16
	ItemOrder uint8
	Grade     uint16
	DropRate  uint32
}

type ProductItem struct{ Type, TypeID, Count uint8 }

type Product struct {
	Code    string
	Name    string
	BuyCost uint32
	Items   []ProductItem
}

type ExpKey struct {
	Level uint16
	Grow  uint8 // ExpDefs.Grow, 1-4 (Chars.Grow + 1)
}

// StatusKey indexes StatusDefs by level and job (Chars.Job).
type StatusKey struct {
	Level uint16
	Job   uint8
}

// Status is a StatusDefs row: base max HP/SP/MP for a level and job.
type Status struct{ HP, SP, MP uint32 }

// BaseItem is a BaseItemsDefs row: an item a new character of that
// family and job starts with, in inventory bag 1 at Slot.
type BaseItem struct {
	Type, TypeID, Count, Slot uint8
	Quality                   uint16
}

// BaseItemKey indexes BaseItemsDefs by family (race) and job.
type BaseItemKey struct{ Family, Job uint8 }

// Defs holds every PS_GameDefs table the world needs.
type Defs struct {
	Items        map[ItemKey]*ItemDef
	ItemsByGrade map[uint16][]*ItemDef
	Skills       map[SkillKey]*SkillDef
	Mobs         map[uint16]*MobDef
	MobDrops     map[uint16][]MobDrop
	Exp          map[ExpKey]uint32
	Products     map[string]*Product
	Status       map[StatusKey]Status
	BaseItems    map[BaseItemKey][]BaseItem
}

var (
	defsOnce sync.Once
	defs     *Defs
	defsErr  error
)

// SetDefsForTest installs definitions without a database (unit tests).
func SetDefsForTest(d *Defs) {
	defsOnce.Do(func() {})
	defs, defsErr = d, nil
}

// Definitions returns the cached definitions, loading them on first use.
func Definitions() (*Defs, error) {
	defsOnce.Do(func() { defs, defsErr = loadDefs() })
	return defs, defsErr
}

// MustDefs is for code paths that run only after a successful startup load.
func MustDefs() *Defs {
	d, err := Definitions()
	if err != nil {
		panic(err)
	}
	return d
}

func clampU8(v int64) uint8 {
	return uint8(min(max(v, 0), 255))
}

func clampU16(v int64) uint16 {
	return uint16(min(max(v, 0), 65535))
}

// normalizeSkill applies CGameData::LoadConstSkill's load-time rules.
func normalizeSkill(s *SkillDef, defenceType, fixRange uint8, keep uint32, aggro uint16, need [15]uint8, shield uint8) {
	s.MobSkill = s.SkillLevel == 100
	s.Weapon1, s.Weapon2 = min(s.Weapon1, 19), min(s.Weapon2, 19)
	kind := s.TypeAttack
	if kind == 0 {
		kind = 1
	}
	s.Kind = min(kind, 3) - 1
	if defenceType == 0 {
		defenceType = 1
	}
	s.DefenceType = min(defenceType, 3) - 1
	if s.AttrType > 8 {
		s.AttrType = 0
	}
	if s.StateType > 15 {
		s.StateType = 0
	}
	for i := range s.AbilityType {
		if s.AbilityType[i] != 0 && s.AbilityValue[i] == 0 {
			s.AbilityValue[i] = 1
		}
	}
	switch s.TypeDetail {
	case 93:
		s.TypeEffect = 4
	case 122:
		s.TypeEffect = 6
	}
	s.MultiAttack = min(s.MultiAttack, 60)
	s.KeepSeconds, s.KeepOnDeath = keep, fixRange != 0
	if fixRange == 1 {
		s.KeepSeconds = keep * 3600
	}
	s.Aggro = aggro
	for i, v := range need {
		s.NeedWeapon[i] = v != 0
	}
	s.NeedShield = shield != 0
}

// MobRespawnSeconds maps Mobs.Attackspecial3 to the respawn delay.
func MobRespawnSeconds(special uint8) uint64 {
	// CGameData::LoadConstMob 0x004c1e81..0x004c1f62 (ms): selector 14 is
	// 0 (immediate), anything above 15 reads 0 too.
	table := []uint64{5, 30, 60, 3 * 60, 5 * 60, 10 * 60, 15 * 60, 30 * 60, 3600, 4 * 3600,
		12 * 3600, 24 * 3600, 3 * 86400, 2 * 3600, 0, 7 * 86400}
	if int(special) < len(table) {
		return table[special]
	}
	return table[0]
}

func loadDefs() (*Defs, error) {
	pool, err := Pool(GameDefs)
	if err != nil {
		return nil, err
	}
	d := &Defs{Items: map[ItemKey]*ItemDef{}, ItemsByGrade: map[uint16][]*ItemDef{},
		Skills: map[SkillKey]*SkillDef{}, Mobs: map[uint16]*MobDef{}, MobDrops: map[uint16][]MobDrop{},
		Exp: map[ExpKey]uint32{}, Products: map[string]*Product{}, Status: map[StatusKey]Status{},
		BaseItems: map[BaseItemKey][]BaseItem{}}
	c, cancel := longCtx()
	defer cancel()

	rows, err := pool.QueryContext(c, `SELECT ItemID, ISNULL(ItemName,''), Type, TypeID, Reqlevel, Country,
		Attackfighter, Defensefighter, Patrolrogue, Shootrogue, Attackmage, Defensemage, Grow,
		ReqStr, ReqDex, ReqRec, ReqInt, ReqWis, Reqluc, ReqIg, Range, AttackTime, Special, ISNULL(Attrib,0), Slot, Quality,
		Effect1, Effect2, Effect3, Effect4, ConstHP, ConstSP, ConstMP, ConstStr, ConstDex, ConstRec,
		ConstInt, ConstWis, ConstLuc, Buy, Sell, Grade, ISNULL([Drop],0), Server, Count, ReqVg, ISNULL(Exp,0) FROM dbo.Items`)
	if err != nil {
		return nil, fmt.Errorf("load Items: %w", err)
	}
	for rows.Next() {
		var it ItemDef
		var reqLevel, quality, grade, drop int64
		var classes [6]uint8
		if err := rows.Scan(&it.ItemID, &it.Name, &it.Type, &it.TypeID, &reqLevel, &it.Country,
			&classes[0], &classes[1], &classes[2], &classes[3], &classes[4], &classes[5], &it.Grow,
			&it.ReqStr, &it.ReqDex, &it.ReqRec, &it.ReqInt, &it.ReqWis, &it.ReqLuc, &it.ReqIg, &it.Range,
			&it.AttackTime, &it.Special, &it.Attrib, &it.Slot, &quality, &it.Effect1, &it.Effect2, &it.Effect3, &it.Effect4,
			&it.ConstHP, &it.ConstSP, &it.ConstMP, &it.ConstStr, &it.ConstDex, &it.ConstRec, &it.ConstInt,
			&it.ConstWis, &it.ConstLuc, &it.Buy, &it.Sell, &grade, &drop, &it.Server, &it.Count, &it.ReqVg, &it.ExpAbsorb); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scan Items: %w", err)
		}
		it.Name = strings.TrimRight(it.Name, " ")
		it.ReqLevel, it.Quality, it.Grade, it.Drop = clampU16(reqLevel), clampU16(quality), clampU16(grade), clampU16(drop)
		for i, v := range classes {
			it.Classes[i] = v != 0
		}
		item := it
		d.Items[ItemKey{it.Type, it.TypeID}] = &item
		if item.Grade > 0 && item.Type > 0 && item.TypeID > 0 {
			d.ItemsByGrade[item.Grade] = append(d.ItemsByGrade[item.Grade], &item)
		}
	}
	rows.Close()

	rows, err = pool.QueryContext(c, `SELECT SkillID, SkillLevel, ISNULL(SkillName,''), Country, ReqLevel, Grow,
		SkillPoint, PrevSkillID, Attackfighter, Defensefighter, Patrolrogue, Shootrogue, Attackmage, Defensemage,
		SP, MP, ResetTime, ReadyTime, DamageHP, AddDamageHP, HealHP, HealSP, HealMP, TypeDetail, KeepTime,
		TimeHealHP, TimeHealSP, TimeHealMP, AbilityType1, AbilityValue1, AbilityType2, AbilityValue2,
		AbilityType3, AbilityValue3, TimeDamageHP, TimeDamageSP, TimeDamageMP, AttackRange, TypeAttack,
		TargetType, ApplyRange, MultiAttack, TypeEffect, DamageType, DamageSP, DamageMP, AddDamageSP, AddDamageMP,
		AttrType, StateType, SuccessType, SuccessValue, Weapon1, Weapon2, Weaponvalue, DefenceType, DefenceValue,
		Arrow, LimitHP, FixRange, NeedWeapon1, NeedWeapon2, NeedWeapon3, NeedWeapon4, NeedWeapon5, NeedWeapon6,
		NeedWeapon7, NeedWeapon8, NeedWeapon9, NeedWeapon10, NeedWeapon11, NeedWeapon12, NeedWeapon13, NeedWeapon14,
		NeedWeapon15, Shield, TimeDamageType, TypeShow FROM dbo.Skills`)
	if err != nil {
		return nil, fmt.Errorf("load Skills: %w", err)
	}
	for rows.Next() {
		var s SkillDef
		var skillID, reqLevel, prev, sp, mp, reset, typeDetail, keep, aggro int64
		var classes [6]uint8
		var defenceType, fixRange, shield uint8
		var need [15]uint8
		extra := []any{&s.TypeEffect, &s.DamageType, &s.DamageSP, &s.DamageMP, &s.AddDamageSP, &s.AddDamageMP,
			&s.AttrType, &s.StateType, &s.SuccessType, &s.SuccessValue, &s.Weapon1, &s.Weapon2, &s.WeaponValue,
			&defenceType, &s.DefenceValue, &aggro, &s.LimitHP, &fixRange}
		for i := range need {
			extra = append(extra, &need[i])
		}
		extra = append(extra, &shield, &s.TimeDamageType, &s.TypeShow)
		if err := rows.Scan(append([]any{&skillID, &s.SkillLevel, &s.Name, &s.Country, &reqLevel, &s.Grow, &s.SkillPointCost,
			&prev, &classes[0], &classes[1], &classes[2], &classes[3], &classes[4], &classes[5], &sp, &mp,
			&reset, &s.ReadyTime, &s.DamageHP, &s.AddDamageHP, &s.HealHP, &s.HealSP, &s.HealMP, &typeDetail,
			&keep, &s.TimeHealHP, &s.TimeHealSP, &s.TimeHealMP, &s.AbilityType[0], &s.AbilityValue[0],
			&s.AbilityType[1], &s.AbilityValue[1], &s.AbilityType[2], &s.AbilityValue[2], &s.TimeDamageHP,
			&s.TimeDamageSP, &s.TimeDamageMP, &s.AttackRange, &s.TypeAttack, &s.TargetType, &s.ApplyRange,
			&s.MultiAttack}, extra...)...); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scan Skills: %w", err)
		}
		s.SkillID, s.ReqLevel, s.PrevSkillID = clampU16(skillID), clampU16(reqLevel), clampU16(prev)
		s.SPCost, s.MPCost, s.ResetTimeSeconds = clampU16(sp), clampU16(mp), clampU16(reset)
		// KeepTime is a smallint holding an unsigned word (43200 reads -22336).
		keep = int64(uint16(keep))
		s.TypeDetail, s.KeepTime = clampU16(typeDetail), clampU16(keep)
		for i, v := range classes {
			s.ClassEligible[i] = v != 0
		}
		normalizeSkill(&s, defenceType, fixRange, uint32(max(keep, 0)), clampU16(aggro), need, shield)
		skill := s
		d.Skills[SkillKey{s.SkillID, s.SkillLevel}] = &skill
	}
	rows.Close()

	rows, err = pool.QueryContext(c, `SELECT MobID, ISNULL(MobName,''), Level, HP, Money1, Money2, Attackspecial3,
		Exp, Attack1, AI, ChaseRange, Dex, Wis, Luc, Defense, Magic, Attacktime3, Attrib, ISNULL(Size,0), ISNULL(QuestItemID,0), NormalTime, NormalStep,
		ChaseTime, ChaseStep, AttackType1, AttackTime1, Attackrange1, Attack1, Attackplus1, Attackattrib1, Attackok1,
		Attacktype2, Attacktime2, Attackrange2, Attack2, Attackplus2, Attackattrib2, Attackok2,
		Attacktype3, Attacktime3, Attackrange3, Attack3, Attackplus3, Attackattrib3, Attackok3,
		Attackspecial1, Attackspecial2, Attackspecial3,
		ResistState1, ResistState2, ResistState3, ResistState4, ResistState5, ResistState6, ResistState7, ResistState8,
		ResistState9, ResistState10, ResistState11, ResistState12, ResistState13, ResistState14, ResistState15,
		ResistSkill1, ResistSkill2, ResistSkill3, ResistSkill4, ResistSkill5, ResistSkill6, MP, SP FROM dbo.Mobs`)
	if err != nil {
		return nil, fmt.Errorf("load Mobs: %w", err)
	}
	for rows.Next() {
		var mobID, level, hp, money1, money2, exp, attack, chase int64
		var dex, wis, luc, def, magic, shield, attrib, size, questItem, ntime, nstep, ctime, cstep int64
		var a [3][7]int64
		var special, ai uint8
		var name string
		dest := []any{&mobID, &name, &level, &hp, &money1, &money2, &special, &exp, &attack, &ai, &chase,
			&dex, &wis, &luc, &def, &magic, &shield, &attrib, &size, &questItem, &ntime, &nstep, &ctime, &cstep}
		for i := range a {
			for j := range a[i] {
				dest = append(dest, &a[i][j])
			}
		}
		var specials [3]int64
		var resistState [15]int64
		var resistSkill [6]int64
		for i := range specials {
			dest = append(dest, &specials[i])
		}
		for i := range resistState {
			dest = append(dest, &resistState[i])
		}
		for i := range resistSkill {
			dest = append(dest, &resistSkill[i])
		}
		var mobMP, mobSP int64
		dest = append(dest, &mobMP, &mobSP)
		if err := rows.Scan(dest...); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scan Mobs: %w", err)
		}
		if mobID <= 0 {
			continue
		}
		d.Mobs[uint16(mobID)] = &MobDef{MobID: uint16(mobID), Name: strings.TrimRight(name, " "),
			Level: max(clampU16(level), 1), MaxHP: uint32(max(hp, 1)), MoneyMin: uint32(max(money1, 0)),
			MoneyMax: uint32(max(money2, 0)), RespawnSeconds: MobRespawnSeconds(special),
			Exp: uint32(max(exp, 0)), Attack: uint32(max(attack, 0)), AI: ai, ChaseRange: float32(max(chase, 0)),
			Dex: clampU16(dex), Wis: clampU16(wis), Luc: clampU16(luc), Defense: clampU16(def), Magic: clampU16(magic),
			ShieldDefense: uint32(max(shield, 0)), Attrib: clampU8(attrib), Size: clampU8(size), QuestItemID: int32(questItem), NormalTimeMS: uint32(max(ntime, 0)),
			NormalStep: clampU16(nstep), ChaseTimeMS: uint32(max(ctime, 0)), ChaseStep: clampU16(cstep)}
		for i := range a {
			d.Mobs[uint16(mobID)].Attacks[i] = MobAttack{Type: clampU16(a[i][0]), TimeMS: uint32(max(a[i][1], 0)),
				Range: clampU16(a[i][2]), Attack: clampU16(a[i][3]), Plus: clampU16(a[i][4]), Attrib: clampU8(a[i][5]),
				Special: clampU8(specials[i]), OK: clampU8(a[i][6])}
		}
		m := d.Mobs[uint16(mobID)]
		m.MaxMP, m.MaxSP = uint32(max(mobMP, 0)), uint32(max(mobSP, 0))
		m.RecoverValue, m.Obelisk = int32(m.Attacks[2].Attack), m.Attacks[2].Plus != 0
		switch m.Attacks[2].Attrib {
		case 1:
			m.Country = 0
		case 2:
			m.Country = 1
		default:
			m.Country = 2
		}
		for i := 0; i < 2; i++ {
			if m.Attacks[i].Attrib > 4 {
				m.Attacks[i].Attrib = 0
			}
		}
		for i, v := range resistState {
			m.ResistState[i] = clampU8(v)
		}
		for i, v := range resistSkill {
			m.ResistSkill[i] = clampU16(v)
		}
	}
	rows.Close()

	rows, err = pool.QueryContext(c, `SELECT MobID, ItemOrder, Grade, DropRate FROM dbo.MobItems
		WHERE Grade > 0 AND DropRate > 0 ORDER BY MobID, ItemOrder`)
	if err != nil {
		return nil, fmt.Errorf("load MobItems: %w", err)
	}
	for rows.Next() {
		var mobID, grade, rate int64
		var order uint8
		if err := rows.Scan(&mobID, &order, &grade, &rate); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scan MobItems: %w", err)
		}
		if mobID > 0 {
			d.MobDrops[uint16(mobID)] = append(d.MobDrops[uint16(mobID)],
				MobDrop{MobID: uint16(mobID), ItemOrder: order, Grade: clampU16(grade), DropRate: uint32(rate)})
		}
	}
	rows.Close()

	rows, err = pool.QueryContext(c, `SELECT Level, Grow, Exp FROM dbo.ExpDefs`)
	if err != nil {
		return nil, fmt.Errorf("load ExpDefs: %w", err)
	}
	for rows.Next() {
		var level, exp int64
		var grow uint8
		if err := rows.Scan(&level, &grow, &exp); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scan ExpDefs: %w", err)
		}
		d.Exp[ExpKey{clampU16(level), grow}] = uint32(max(exp, 0))
	}
	rows.Close()

	rows, err = pool.QueryContext(c, `SELECT Level, Job, HP, SP, MP FROM dbo.StatusDefs`)
	if err != nil {
		return nil, fmt.Errorf("load StatusDefs: %w", err)
	}
	for rows.Next() {
		var level, hp, sp, mp int64
		var job uint8
		if err := rows.Scan(&level, &job, &hp, &sp, &mp); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scan StatusDefs: %w", err)
		}
		d.Status[StatusKey{clampU16(level), job}] = Status{uint32(max(hp, 0)), uint32(max(sp, 0)), uint32(max(mp, 0))}
	}
	rows.Close()

	rows, err = pool.QueryContext(c, `SELECT Family, Job, Type, TypeID, Count, Slot, ISNULL(Quality,0)
		FROM dbo.BaseItemsDefs ORDER BY RowID`)
	if err != nil {
		return nil, fmt.Errorf("load BaseItemsDefs: %w", err)
	}
	for rows.Next() {
		var family, job uint8
		var b BaseItem
		var quality int64
		if err := rows.Scan(&family, &job, &b.Type, &b.TypeID, &b.Count, &b.Slot, &quality); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scan BaseItemsDefs: %w", err)
		}
		b.Quality = clampU16(quality)
		k := BaseItemKey{family, job}
		d.BaseItems[k] = append(d.BaseItems[k], b)
	}
	rows.Close()

	var columns strings.Builder
	for i := 1; i <= 24; i++ {
		fmt.Fprintf(&columns, ", ISNULL(ItemID%d,0), ISNULL(ItemCount%d,0)", i, i)
	}
	rows, err = pool.QueryContext(c, `SELECT ISNULL(ProductCode,''), ISNULL(ProductName,''), ISNULL(BuyCost,0)`+
		columns.String()+` FROM dbo.ProductList`)
	if err != nil {
		return nil, fmt.Errorf("load ProductList: %w", err)
	}
	for rows.Next() {
		var p Product
		var cost int64
		ids := make([]int64, 24)
		counts := make([]uint8, 24)
		dest := []any{&p.Code, &p.Name, &cost}
		for i := range 24 {
			dest = append(dest, &ids[i], &counts[i])
		}
		if err := rows.Scan(dest...); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scan ProductList: %w", err)
		}
		p.Code, p.Name, p.BuyCost = strings.TrimRight(p.Code, " "), strings.TrimRight(p.Name, " "), uint32(max(cost, 0))
		for i := range 24 {
			if ids[i] > 0 && counts[i] > 0 {
				p.Items = append(p.Items, ProductItem{clampU8(ids[i] / 1000), clampU8(ids[i] % 1000), counts[i]})
			}
		}
		product := p
		if product.Code != "" {
			d.Products[product.Code] = &product
		}
	}
	rows.Close()
	return d, nil
}

// Item returns the definition for (type, typeID), or nil.
func (d *Defs) Item(itemType, typeID uint8) *ItemDef { return d.Items[ItemKey{itemType, typeID}] }

// Skill returns the definition for (skillID, level), or nil.
func (d *Defs) Skill(skillID uint16, level uint8) *SkillDef {
	return d.Skills[SkillKey{skillID, level}]
}
