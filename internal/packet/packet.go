// Package packet holds the Shaiya 5.4 opcodes and the little-endian frame
// writer/readers shared by the login and world servers. Frame layout:
// length:u16 (whole frame), opcode:u16, payload.
package packet

import (
	"encoding/binary"
	"errors"
	"math"
)

type Opcode uint16

// Opcodes. Comments carry the confirmed wire shapes recorded while the
// server was written against captures of the real 5.4 client.
const (
	CharacterList   Opcode = 0x0101
	CreateCharacter Opcode = 0x0102
	// Request charId:u32; response result:u8 (0=ok), charId:u32. Soft delete,
	// restorable for 2 hours.
	DeleteCharacter Opcode = 0x0103
	SelectCharacter Opcode = 0x0104
	// Same shape as DeleteCharacter.
	RestoreCharacter Opcode = 0x010F
	// Request charId:u32, newName; response result:u8 with 1 = ok, 0 = fail.
	RenameCharacter        Opcode = 0x010E
	CharacterDetails       Opcode = 0x0105
	CharacterItems         Opcode = 0x0106
	Logout                 Opcode = 0x0107
	CharacterSkills        Opcode = 0x0108
	AccountFaction         Opcode = 0x0109
	CharacterActiveBuffs   Opcode = 0x010A
	CharacterSkillBar      Opcode = 0x010B
	CharacterEnteredMap    Opcode = 0x0201
	AutoAttackStop         Opcode = 0x0212
	SetMoney               Opcode = 0x0213
	ExpGain                Opcode = 0x0207
	LevelUpSelf            Opcode = 0x0508
	LevelUpOther           Opcode = 0x051E
	TargetMobGetState      Opcode = 0x0304
	TargetMobHpUpdate      Opcode = 0x0305
	TargetClear            Opcode = 0x030A
	UseVehicle             Opcode = 0x0216 // empty request; response ok:u8, isOnVehicle:u8
	MobDebuffAdd           Opcode = 0x030C // debuffType:u8, mobId:u32, skillId:u16, skillLevel:u8
	MobDebuffRemove        Opcode = 0x030D
	ItemBagToBag           Opcode = 0x0204
	AddItem                Opcode = 0x0205
	RemoveItem             Opcode = 0x0206
	SetStatPoint           Opcode = 0x0208
	StatsReset             Opcode = 0x0214 // success:u8, statPoint:u16, str,rec,int,wis,dex,luc:u16
	LearnNewSkill          Opcode = 0x0209
	CharacterEnteredPortal Opcode = 0x020A
	CharacterMapTeleport   Opcode = 0x020B
	UserKillCountUpdate    Opcode = 0x020E
	CharacterShape         Opcode = 0x0303
	// Request bag,slot,hair,face,height,sex:u8; reply charId:u32,hair,face,height,sex:u8.
	ChangeAppearance          Opcode = 0x0226
	WorldDay                  Opcode = 0x0404
	CharacterMove             Opcode = 0x0501
	CharacterReviveHitpoints  Opcode = 0x0505 // charId:u32, hp,mp,sp:u32
	CharacterUserAutoAttack   Opcode = 0x0502
	CharacterMobAutoAttack    Opcode = 0x0503
	CharacterDeath            Opcode = 0x0504 // charId:u32, deathType:u8, killerId:u32
	CharacterMotion           Opcode = 0x0506
	CharacterEquipment        Opcode = 0x0507
	MapAddItem                Opcode = 0x0401
	MapRemoveItem             Opcode = 0x0402
	CharacterReviveInit       Opcode = 0x0406 // charId:u32
	MobEnter                  Opcode = 0x0601
	MobLeave                  Opcode = 0x0602
	MobMove                   Opcode = 0x0603
	MobAttackCharacter        Opcode = 0x0605 // result:u8, mobId:u32, charId:u32, dmgHp,dmgSp,dmgMp:u16
	MobDeath                  Opcode = 0x0606
	MobPeriodicDamage         Opcode = 0x0607 // mobId:u32, skillId:u16, skillLevel:u8, dmgHp,dmgSp,dmgMp:u16
	MobCurrentHitpoints       Opcode = 0x0610 // mobId:u32, currentHp:u32
	MapNpcEnter               Opcode = 0x0E01
	MapNpcLeave               Opcode = 0x0E02
	MapNpcMove                Opcode = 0x0E03
	UseItem                   Opcode = 0x050A
	CharacterMaxEnergy        Opcode = 0x050B
	SkillUsedSpMp             Opcode = 0x050C
	SkillCastStarted          Opcode = 0x0510
	BuffAdd                   Opcode = 0x050D // id:u32, skillId:u16, skillLevel:u8, countdownSeconds:u32
	BuffRemove                Opcode = 0x050E // objectId:u32
	SkillKeep                 Opcode = 0x050F // objectId:u32, skillId:u16, skillLevel:u8, hp,mp,sp:u16
	CharacterMaxHpMpSp        Opcode = 0x051F
	CharacterCurrentHitpoints Opcode = 0x0521
	CharacterAdditionalStats  Opcode = 0x0526
	TeleportSavePositionList  Opcode = 0x0225
	BlessInit                 Opcode = 0x0211
	QuestList                 Opcode = 0x0901
	QuestFinishedList         Opcode = 0x0906
	ChatNormal                Opcode = 0x1101
	ChatWorld                 Opcode = 0x1103
	ChatError                 Opcode = 0x1106
	ChatMap                   Opcode = 0x1111
	WarehouseItemList         Opcode = 0x0711
	ShopSell                  Opcode = 0x0703
	SkillUseNoTarget          Opcode = 0x0511
	ResurrectRequest          Opcode = 0x0553 // 1-byte body
	// charId:u32, 5-byte blob 04 46 AF 37 01 (verbatim), x,y,z:f32.
	CharacterResurrect   Opcode = 0x0551
	SkillUse             Opcode = 0x0517
	SkillExtraDamage     Opcode = 0x051A
	CharacterMountShape  Opcode = 0x051D // characterId:u32, shape:u8
	CharacterDeathState  Opcode = 0x051C // charId:u32, 0x07, 0x02
	AccountPoints        Opcode = 0x2601
	PointBuy             Opcode = 0x2602
	PointGiftSend        Opcode = 0x2603
	PointStoredItemList  Opcode = 0x2606
	PointStoredItemGet   Opcode = 0x2607
	CashPoint            Opcode = 0x2605
	BankItemList         Opcode = 0xB101
	LoginHandshake       Opcode = 0xA101
	LoginRequest         Opcode = 0xA102
	ServerList           Opcode = 0xA201
	SelectServer         Opcode = 0xA202
	GameHandshake        Opcode = 0xA301
	Ping                 Opcode = 0xA303
	ChatNormalAdmin      Opcode = 0xF101
	ChatWorldAdmin       Opcode = 0xF103
	GmCommandError       Opcode = 0xF501
	GmCommandGetItem     Opcode = 0xF702
	EnchantAdd           Opcode = 0x0805
	ItemCompose          Opcode = 0x0806
	GemAdd               Opcode = 0x0801
	GemRemove            Opcode = 0x0802
	GemAddPossibility    Opcode = 0x0809
	GemRemovePossibility Opcode = 0x080A
	ExchangeRequest      Opcode = 0x0A01
	ExchangeAnswer       Opcode = 0x0A02
	ExchangeStart        Opcode = 0x0A03
	ExchangeEnd          Opcode = 0x0A04
	ExchangeReady        Opcode = 0x0A05
	ExchangeAdd          Opcode = 0x0A06
	ExchangeRemove       Opcode = 0x0A07
	ExchangeAddMoney     Opcode = 0x0A08
	ExchangeAddDest      Opcode = 0x0A09
	ExchangeConfirm      Opcode = 0x0A0A
)

var ErrTooLarge = errors.New("packet is too large")
var ErrStringTooLarge = errors.New("fixed string value is too large")

// Writer builds one frame; the length prefix is filled in by Bytes.
type Writer struct {
	data []byte
	err  error
}

func New(opcode Opcode) *Writer {
	w := &Writer{data: make([]byte, 0, 64)}
	w.U16(0)
	w.U16(uint16(opcode))
	return w
}

func (w *Writer) U8(v uint8)     { w.data = append(w.data, v) }
func (w *Writer) U16(v uint16)   { w.data = binary.LittleEndian.AppendUint16(w.data, v) }
func (w *Writer) U32(v uint32)   { w.data = binary.LittleEndian.AppendUint32(w.data, v) }
func (w *Writer) I32(v int32)    { w.U32(uint32(v)) }
func (w *Writer) U64(v uint64)   { w.data = binary.LittleEndian.AppendUint64(w.data, v) }
func (w *Writer) F32(v float32)  { w.U32(math.Float32bits(v)) }
func (w *Writer) Bytes(v []byte) { w.data = append(w.data, v...) }
func (w *Writer) Bool(v bool) {
	if v {
		w.U8(1)
	} else {
		w.U8(0)
	}
}

// FixedString writes value null-padded to size bytes; an oversized value
// makes Finish fail instead of truncating silently.
func (w *Writer) FixedString(value string, size int) {
	if len(value) > size {
		if w.err == nil {
			w.err = ErrStringTooLarge
		}
		value = value[:size]
	}
	w.data = append(w.data, value...)
	w.data = append(w.data, make([]byte, size-len(value))...)
}

func (w *Writer) Len() int { return len(w.data) }

// Finish returns the frame with its length prefix set.
func (w *Writer) Finish() ([]byte, error) {
	if w.err != nil {
		return nil, w.err
	}
	if len(w.data) > math.MaxUint16 {
		return nil, ErrTooLarge
	}
	binary.LittleEndian.PutUint16(w.data[0:2], uint16(len(w.data)))
	return w.data, nil
}

// MustFinish is for frames whose shape is fixed and cannot overflow.
func (w *Writer) MustFinish() []byte {
	frame, err := w.Finish()
	if err != nil {
		panic(err)
	}
	return frame
}

var ErrShort = errors.New("not enough bytes")

func U16(b []byte) (uint16, error) {
	if len(b) < 2 {
		return 0, ErrShort
	}
	return binary.LittleEndian.Uint16(b), nil
}

func U32(b []byte) (uint32, error) {
	if len(b) < 4 {
		return 0, ErrShort
	}
	return binary.LittleEndian.Uint32(b), nil
}

func F32(b []byte) (float32, error) {
	v, err := U32(b)
	return math.Float32frombits(v), err
}

// FixedASCII decodes a null-terminated, fixed-width string field.
func FixedASCII(b []byte) string {
	for i, c := range b {
		if c == 0 {
			return string(b[:i])
		}
	}
	return string(b)
}
