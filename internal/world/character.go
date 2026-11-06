package world

import (
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

const (
	defaultCharacterID = 1
	basePoolPerLevel   = 100
	baseStat           = 10
	baseMinAttack      = 15
	baseMaxAttack      = 15
	baseMinMagic       = 15
	baseMaxMagic       = 15
	baseDefense        = 10
	baseResistance     = 10
	hpPerReaction      = 5
	spPerDexterity     = 5
	mpPerWisdom        = 5

	specialNone                 = 0
	specialPercentHealingPotion = 17
	specialChangeAppearance     = 44
	specialChangeSex            = 45
	specialChangeName           = 51
	specialResetStats           = 34
	specialRecreationRune       = 62
	statPointsPerLevel          = 5

	InventoryFirstBag  = 1
	InventoryLastBag   = 5
	InventorySlotCount = 24

	FactionLight      = 0
	FactionDark       = 1
	FactionNone       = 2
	MaxModeUltimate   = 3
	MaxCharacterSlots = 5

	MotionSitDown = 10
	MotionStandUp = 11
)

var consumableItemTypes = []uint8{25, 100, 101, 102}

func isConsumableType(t uint8) bool {
	for _, c := range consumableItemTypes {
		if c == t {
			return true
		}
	}
	return false
}

// basePools are the base max HP/MP/SP for a level and job from
// PS_GameDefs.dbo.StatusDefs, falling back to the highest defined level.
func basePools(level uint16, job uint8) (hp, mp, sp uint32) {
	status := db.MustDefs().Status
	for l := max(level, 1); l >= 1; l-- {
		if st, ok := status[db.StatusKey{Level: l, Job: job}]; ok {
			return st.HP, st.MP, st.SP
		}
	}
	return basePoolPerLevel, basePoolPerLevel, basePoolPerLevel
}

func satAdd(a, b uint32) uint32 {
	if s := a + b; s >= a {
		return s
	}
	return ^uint32(0)
}

func satSub(a, b uint32) uint32 {
	if a > b {
		return a - b
	}
	return 0
}

func satMul(a, b uint32) uint32 {
	r := uint64(a) * uint64(b)
	if r > uint64(^uint32(0)) {
		return ^uint32(0)
	}
	return uint32(r)
}

// EquipmentStats aggregates equipped items, gems and craft bonuses.
type EquipmentStats struct {
	Str, Rec, Int, Wis, Dex, Luc uint32
	MaxHP, MaxMP, MaxSP          uint32
	MinAttack, MaxAttack         uint32
	MinMagic, MaxMagic           uint32
	Defense, Resistance          uint32
	Absorption                   uint32
}

const gemItemType = 30

// IsWeaponItemType: Type 1-15 and 45-65.
func IsWeaponItemType(t uint8) bool { return (t >= 1 && t <= 15) || (t >= 45 && t <= 65) }

// IsArmorItemType: Type 16-21, 31-36 and 66-92.
func IsArmorItemType(t uint8) bool {
	return (t >= 16 && t <= 21) || (t >= 31 && t <= 36) || (t >= 66 && t <= 92)
}

var weaponEnchantAttackBonus = [21]uint32{0, 7, 14, 21, 31, 41, 51, 64, 77, 90, 106, 122, 138, 157, 176, 195, 217, 239, 261, 286, 311}
var armorEnchantAbsorption = [21]uint32{0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55, 60, 65, 70, 75, 80, 85, 90, 95, 100}

// CraftStats is the decoded 20-digit Craftname: str dex rec int wis luc
// hp/100 mp/100 sp/100 enchant, two digits each.
type CraftStats struct {
	Str, Dex, Rec, Int, Wis, Luc uint32
	MaxHP, MaxMP, MaxSP          uint32
	EnchantLevel                 uint8
}

func ParseCraftName(name string) (CraftStats, bool) {
	for len(name) > 0 && name[len(name)-1] == ' ' {
		name = name[:len(name)-1]
	}
	if len(name) != 20 {
		return CraftStats{}, false
	}
	for i := 0; i < 20; i++ {
		if name[i] < '0' || name[i] > '9' {
			return CraftStats{}, false
		}
	}
	pair := func(i int) uint32 { return uint32(name[i]-'0')*10 + uint32(name[i+1]-'0') }
	return CraftStats{Str: pair(0), Dex: pair(2), Rec: pair(4), Int: pair(6), Wis: pair(8), Luc: pair(10),
		MaxHP: pair(12) * 100, MaxMP: pair(14) * 100, MaxSP: pair(16) * 100, EnchantLevel: uint8(pair(18))}, true
}

func pos16(v int16) uint32 { return uint32(max(v, 0)) }

func (e *EquipmentStats) addItemDef(itemType uint8, d *db.ItemDef) {
	maxEffect, minEffect := pos16(d.Effect1), pos16(d.Effect2)
	if itemType == 12 { // staff: magic weapon
		e.MinMagic, e.MaxMagic = satAdd(e.MinMagic, minEffect), satAdd(e.MaxMagic, maxEffect)
	} else {
		e.MinAttack, e.MaxAttack = satAdd(e.MinAttack, minEffect), satAdd(e.MaxAttack, maxEffect)
	}
	e.Defense = satAdd(e.Defense, pos16(d.Effect3))
	e.Resistance = satAdd(e.Resistance, pos16(d.Effect4))
	e.MaxHP, e.MaxSP, e.MaxMP = satAdd(e.MaxHP, pos16(d.ConstHP)), satAdd(e.MaxSP, pos16(d.ConstSP)), satAdd(e.MaxMP, pos16(d.ConstMP))
	e.Str, e.Dex, e.Rec = satAdd(e.Str, pos16(d.ConstStr)), satAdd(e.Dex, pos16(d.ConstDex)), satAdd(e.Rec, pos16(d.ConstRec))
	e.Int, e.Wis, e.Luc = satAdd(e.Int, pos16(d.ConstInt)), satAdd(e.Wis, pos16(d.ConstWis)), satAdd(e.Luc, pos16(d.ConstLuc))
}

// GetEquipmentStats sums every bag-0 item, its gems and craft bonuses.
func GetEquipmentStats(items []db.Item) EquipmentStats {
	defs := db.MustDefs()
	var e EquipmentStats
	for i := range items {
		it := &items[i]
		if it.Bag != 0 {
			continue
		}
		if d := defs.Item(it.Type, it.TypeID); d != nil {
			e.addItemDef(it.Type, d)
		}
		for _, gem := range it.Gems {
			if gem == 0 {
				continue
			}
			if d := defs.Item(gemItemType, gem); d != nil {
				e.addItemDef(gemItemType, d)
			}
		}
		if c, ok := ParseCraftName(it.CraftName); ok {
			e.Str, e.Dex, e.Rec = satAdd(e.Str, c.Str), satAdd(e.Dex, c.Dex), satAdd(e.Rec, c.Rec)
			e.Int, e.Wis, e.Luc = satAdd(e.Int, c.Int), satAdd(e.Wis, c.Wis), satAdd(e.Luc, c.Luc)
			e.MaxHP, e.MaxMP, e.MaxSP = satAdd(e.MaxHP, c.MaxHP), satAdd(e.MaxMP, c.MaxMP), satAdd(e.MaxSP, c.MaxSP)
			if c.EnchantLevel > 0 {
				step := min(c.EnchantLevel, 20)
				if IsWeaponItemType(it.Type) {
					b := weaponEnchantAttackBonus[step]
					e.MinAttack, e.MaxAttack = satAdd(e.MinAttack, b), satAdd(e.MaxAttack, b)
					e.MinMagic, e.MaxMagic = satAdd(e.MinMagic, b), satAdd(e.MaxMagic, b)
				} else if IsArmorItemType(it.Type) {
					e.Absorption = satAdd(e.Absorption, armorEnchantAbsorption[step])
				}
			}
		}
	}
	return e
}

// CharacterStats are the effective stats. Invariant: Character.CurrentHP/
// MP/SP are BASE values; this adds the equipment and stat-derived bonus and
// clamps to the effective max. Changes to current HP/MP/SP must be applied
// to the base field as deltas, never by assigning an effective value.
type CharacterStats struct {
	Str, Rec, Int, Wis, Dex, Luc    uint32
	MaxHP, MaxMP, MaxSP             uint32
	CurrentHP, CurrentMP, CurrentSP uint32
	MinAttack, MaxAttack            uint32
	MinMagic, MaxMagic              uint32
	Defense, Resistance, Absorption uint32
}

func StatsFor(ch *db.Character, e EquipmentStats) CharacterStats {
	bStr, bRec, bInt, bWis, bDex, bLuc := uint32(baseStat), uint32(baseStat), uint32(baseStat), uint32(baseStat), uint32(baseStat), uint32(baseStat)
	baseHP, baseMP, baseSP := uint32(100), uint32(100), uint32(100)
	curHP, curMP, curSP := uint32(100), uint32(100), uint32(100)
	if ch != nil {
		bStr, bRec, bInt, bWis, bDex, bLuc = uint32(ch.Str), uint32(ch.Rec), uint32(ch.Int), uint32(ch.Wis), uint32(ch.Dex), uint32(ch.Luc)
		baseHP, baseMP, baseSP = basePools(ch.Level, ch.Class)
		curHP, curMP, curSP = ch.CurrentHP, ch.CurrentMP, ch.CurrentSP
	}
	var s CharacterStats
	s.Str, s.Rec, s.Int = satAdd(bStr, e.Str), satAdd(bRec, e.Rec), satAdd(bInt, e.Int)
	s.Wis, s.Dex, s.Luc = satAdd(bWis, e.Wis), satAdd(bDex, e.Dex), satAdd(bLuc, e.Luc)
	dHP, dMP, dSP := satMul(s.Rec, hpPerReaction), satMul(s.Wis, mpPerWisdom), satMul(s.Dex, spPerDexterity)
	s.MaxHP = satAdd(satAdd(baseHP, e.MaxHP), dHP)
	s.MaxMP = satAdd(satAdd(baseMP, e.MaxMP), dMP)
	s.MaxSP = satAdd(satAdd(baseSP, e.MaxSP), dSP)
	s.CurrentHP = min(satAdd(satAdd(curHP, e.MaxHP), dHP), s.MaxHP)
	s.CurrentMP = min(satAdd(satAdd(curMP, e.MaxMP), dMP), s.MaxMP)
	s.CurrentSP = min(satAdd(satAdd(curSP, e.MaxSP), dSP), s.MaxSP)
	dAtk := satAdd(satMul(s.Str, 13), satMul(s.Dex, 2)) / 10
	dMag := satAdd(satMul(s.Int, 13), satMul(s.Wis, 2)) / 10
	s.MinAttack = satAdd(satAdd(baseMinAttack, e.MinAttack), dAtk)
	s.MaxAttack = max(satAdd(satAdd(baseMaxAttack, e.MaxAttack), dAtk), s.MinAttack)
	s.MinMagic = satAdd(satAdd(baseMinMagic, e.MinMagic), dMag)
	s.MaxMagic = max(satAdd(satAdd(baseMaxMagic, e.MaxMagic), dMag), s.MinMagic)
	s.Defense = satAdd(satAdd(baseDefense, e.Defense), s.Rec)
	s.Resistance = satAdd(satAdd(baseResistance, e.Resistance), s.Wis)
	s.Absorption = e.Absorption
	return s
}

func recoveryAmount(value, maxValue uint32, special uint8) uint32 {
	if special == specialPercentHealingPotion {
		return satMul(maxValue, value) / 100
	}
	return value
}

func charID(ch *db.Character) uint32 {
	if ch == nil {
		return defaultCharacterID
	}
	return ch.ID
}

func charName(ch *db.Character) string {
	if ch == nil || ch.Name == "" {
		return "Rapture"
	}
	return ch.Name
}

func charPos(ch *db.Character) (float32, float32, float32) {
	if ch == nil {
		return 0, 0, 0
	}
	return ch.PosX, ch.PosY, ch.PosZ
}

// buildCharacterDetails (0x0105), per the client's reader (0x0058dbd0):
// stat point, skill point (u16), max HP, MP, SP (u32), direction (u16),
// then eleven u32 and the 25-byte guild name. The three exp fields are
// the cumulative exp at the level's start, the total for the next level
// and the current total; their order is not yet confirmed in game.
func buildCharacterDetails(ch *db.Character, s *CharacterStats) []byte {
	w := packet.New(packet.CharacterDetails)
	var statPoint, skillPoint uint16
	var exp, money, k1, k2, k3, k4 uint32
	if ch != nil {
		statPoint, skillPoint, exp, money = ch.StatPoint, ch.SkillPoint, ch.Exp, ch.Money
		k1, k2, k3, k4 = ch.Kills, ch.Deaths, ch.Victories, ch.Defeats
	}
	x, y, z := charPos(ch)
	var dir uint16
	var prev, next uint32
	if ch != nil {
		dir = uint16(ch.Dir)
		prev, next = expBounds(ch)
	}
	w.U16(statPoint)
	w.U16(skillPoint)
	w.U32(s.MaxHP)
	w.U32(s.MaxMP)
	w.U32(s.MaxSP)
	w.U16(dir)
	w.U32(prev)
	w.U32(next)
	w.U32(exp)
	w.U32(money)
	w.F32(x)
	w.F32(y)
	w.F32(z)
	w.U32(k1)
	w.U32(k2)
	w.U32(k3)
	w.U32(k4)
	w.FixedString("", 25) // guild name
	return w.MustFinish()
}

func buildAdditionalStats(s *CharacterStats) []byte {
	w := packet.New(packet.CharacterAdditionalStats)
	for _, v := range []uint32{s.Str, s.Rec, s.Int, s.Wis, s.Dex, s.Luc, s.MinAttack, s.MaxAttack,
		s.MinMagic, s.MaxMagic, s.Defense, s.Resistance} {
		w.U32(v)
	}
	return w.MustFinish()
}

func buildStatsReset(statPoint, str, rec, intel, wis, dex, luc uint16) []byte {
	w := packet.New(packet.StatsReset)
	w.U8(1)
	for _, v := range []uint16{statPoint, str, rec, intel, wis, dex, luc} {
		w.U16(v)
	}
	return w.MustFinish()
}

func buildCurrentHitpoints(s *CharacterStats) []byte {
	w := packet.New(packet.CharacterCurrentHitpoints)
	w.U32(s.CurrentHP)
	w.U32(s.CurrentMP)
	w.U32(s.CurrentSP)
	return w.MustFinish()
}

func buildMaxHPMPSP(ch *db.Character, s *CharacterStats) []byte {
	w := packet.New(packet.CharacterMaxHpMpSp)
	w.U32(charID(ch))
	w.U32(s.MaxHP)
	w.U32(s.MaxMP)
	w.U32(s.MaxSP)
	return w.MustFinish()
}

// buildMaxEnergy (0x050B): hitpointType 0 hp, 1 sp, 2 mp.
func buildMaxEnergy(ch *db.Character, hitpointType uint8, value uint32) []byte {
	w := packet.New(packet.CharacterMaxEnergy)
	w.U32(charID(ch))
	w.U8(hitpointType)
	w.U32(value)
	return w.MustFinish()
}

// buildCharacterEnteredMap: marker 0 already present, 1 own first load, 2 admin.
func buildCharacterEnteredMap(ch *db.Character, isAdmin, firstLoad bool) []byte {
	marker := uint8(0)
	if isAdmin {
		marker = 2
	} else if firstLoad {
		marker = 1
	}
	x, y, z := charPos(ch)
	w := packet.New(packet.CharacterEnteredMap)
	w.U32(charID(ch))
	w.U8(marker)
	w.U16(0)
	w.F32(x)
	w.F32(y)
	w.F32(z)
	w.U32(0) // guild id
	w.U32(0) // vehicle id
	return w.MustFinish()
}

func buildCharacterMapPosition(ch *db.Character) []byte {
	mapID := uint16(0)
	if ch != nil {
		mapID = ch.MapID
	}
	x, y, z := charPos(ch)
	w := packet.New(packet.CharacterMapTeleport)
	w.U32(charID(ch))
	w.U16(mapID)
	w.F32(x)
	w.F32(y)
	w.F32(z)
	return w.MustFinish()
}

func buildCharacterMove(ch *db.Character, motion uint8, direction uint16) []byte {
	x, y, z := charPos(ch)
	w := packet.New(packet.CharacterMove)
	w.U32(charID(ch))
	w.U8(motion)
	w.U16(direction)
	w.F32(x)
	w.F32(y)
	w.F32(z)
	return w.MustFinish()
}

func buildCharacterMotion(characterID uint32, motion uint8) []byte {
	w := packet.New(packet.CharacterMotion)
	w.U32(characterID)
	w.U8(motion)
	return w.MustFinish()
}

func buildLeaveWorldAck(characterID uint32) []byte {
	w := packet.New(packet.Logout)
	w.U32(characterID)
	return w.MustFinish()
}

func equippedItem(items []db.Item, slot uint8) *db.Item {
	for i := range items {
		if items[i].Bag == 0 && items[i].Slot == slot {
			return &items[i]
		}
	}
	return nil
}

func enchantLevel(it *db.Item) uint8 {
	if c, ok := ParseCraftName(it.CraftName); ok {
		return c.EnchantLevel
	}
	return 0
}

func buildCharacterEquipment(ch *db.Character, items []db.Item, slot uint8) []byte {
	w := packet.New(packet.CharacterEquipment)
	w.U32(ch.ID)
	w.U8(slot)
	it := equippedItem(items, slot)
	if it != nil {
		w.U8(it.Type)
		w.U8(it.TypeID)
		w.U8(enchantLevel(it))
		// The client always reads 6 more bytes (0x0058f980): the cloak
		// badge, zero for other slots.
		if slot == 7 {
			w.Bytes(it.Gems[:])
		} else {
			w.Bytes(make([]byte, 6))
		}
	} else {
		w.Bytes(make([]byte, 9))
	}
	return w.MustFinish()
}

func buildChangeAppearance(characterID uint32, hair, face, height, sex uint8) []byte {
	w := packet.New(packet.ChangeAppearance)
	w.U32(characterID)
	w.Bytes([]byte{hair, face, height, sex})
	return w.MustFinish()
}

// buildCharacterShape (0x0303). The persistent motion is 1 only for sitting.
func buildCharacterShape(ch *db.Character, faction uint8, items []db.Item, motion uint8) []byte {
	w := packet.New(packet.CharacterShape)
	w.U32(charID(ch))
	w.U8(0) // alive
	if motion == 1 {
		w.U8(1)
	} else {
		w.U8(0)
	}
	w.U8(faction)
	if ch != nil {
		w.Bytes([]byte{ch.Race, ch.Hair, ch.Face, ch.Height, ch.Class, ch.Gender, 0, ch.Mode})
	} else {
		w.Bytes(make([]byte, 8))
	}
	w.U32(0) // kills
	for slot := uint8(0); slot < 8; slot++ {
		if it := equippedItem(items, slot); it != nil {
			w.Bytes([]byte{it.Type, it.TypeID, 0})
		} else {
			w.Bytes([]byte{0, 0, 0})
		}
	}
	w.FixedString(charName(ch), 21)
	if cloak := equippedItem(items, 7); cloak != nil {
		w.Bytes(cloak.Gems[:])
	} else {
		w.Bytes(make([]byte, 6))
	}
	w.FixedString("", 25) // guild name
	return w.MustFinish()
}

func statPackets(ch *db.Character, s *CharacterStats) []labeled {
	return []labeled{
		{buildAdditionalStats(s), "0x0526 additional stats"},
		{buildMaxHPMPSP(ch, s), "0x051F max hp/mp/sp"},
		{buildMaxEnergy(ch, 0, s.MaxHP), "0x050B max hp"},
		{buildMaxEnergy(ch, 1, s.MaxSP), "0x050B max sp"},
		{buildMaxEnergy(ch, 2, s.MaxMP), "0x050B max mp"},
		{buildCurrentHitpoints(s), "0x0521 current hp/mp/sp"},
	}
}

func buildEmptyCharacterListSlot(slot uint8) []byte {
	w := packet.New(packet.CharacterList)
	w.U8(slot)
	w.U32(0)
	return w.MustFinish()
}

func buildCharacterListSlot(ch *db.Character, items []db.Item) []byte {
	w := packet.New(packet.CharacterList)
	w.U8(ch.Slot)
	w.U32(ch.ID)
	w.U32(0)
	w.U16(ch.Level)
	w.Bytes([]byte{ch.Race, ch.Mode, ch.Hair, ch.Face, ch.Height, ch.Class, ch.Gender})
	w.U16(ch.MapID)
	for _, v := range []uint16{ch.Str, ch.Rec, ch.Int, ch.Wis, ch.Dex, ch.Luc} {
		w.U16(v)
	}
	w.U16(uint16(ch.MaxHP))
	w.U16(uint16(ch.MaxMP))
	w.U16(uint16(ch.MaxSP))
	for slot := uint8(0); slot < 8; slot++ {
		if it := equippedItem(items, slot); it != nil {
			w.U8(it.Type)
		} else {
			w.U8(0)
		}
	}
	for slot := uint8(0); slot < 8; slot++ {
		if it := equippedItem(items, slot); it != nil {
			w.U8(it.TypeID)
		} else {
			w.U8(0)
		}
	}
	w.FixedString(ch.Name, 19)
	w.Bool(ch.PendingDelete)
	w.Bool(ch.RenamePending)
	w.Bytes(make([]byte, 6))
	return w.MustFinish()
}
