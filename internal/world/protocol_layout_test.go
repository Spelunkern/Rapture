package world

import (
	"testing"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// TestPacketLayoutsMatchClient checks every fixed-size server->client
// packet against the body size the client's reader consumes, taken from
// the shaiya-client-decomp dispatcher (fun_0059cf70) and its consumers.
func TestPacketLayoutsMatchClient(t *testing.T) {
	db.SetDefsForTest(&db.Defs{Exp: map[db.ExpKey]uint32{}})
	ch := &db.Character{ID: 1, Name: "A", Level: 1}
	st := &CharacterStats{}
	m, n, mi, it := &MapMob{ID: 1}, &MapNpc{ID: 1}, &MapItem{ID: 1}, &db.Item{}
	sk := &db.SkillDef{SkillID: 1, SkillLevel: 100}
	cases := []struct {
		name  string
		frame []byte
		body  int // bytes after the opcode, per the client reader
	}{
		{"0x0101 0058d860", buildCharacterListSlot(ch, nil), 81},
		{"0x0105 0058dbd0", buildCharacterDetails(ch, st), 87},
		{"0x0109 0058e0e0", buildAccountFaction(0, 3), 2},
		{"0x0201 0058e280", buildCharacterEnteredMap(ch, false, true), 27},
		{"0x0204 0058e510", buildBagToBag(1, 1, nil, 1, 1, nil), 68},
		{"0x0205 0058eb30", buildAddItem(it), 34},
		{"0x0206 0058eca0", buildRemoveItem(it, false), 5},
		{"0x0209 0058ee30", buildLearnSkillResult(true, 1, 1, 1), 5},
		{"0x020b 0058ef60", buildCharacterMapPosition(ch), 18},
		{"0x020e 0058f030", buildKillCountUpdate(0, 0), 5},
		{"0x0211 0058eed0", buildBlessInit(0), 9},
		{"0x0207 0058ed20", buildExpGain(1), 8},
		{"0x0213 u32", buildSetMoney(1), 4},
		{"0x0b05 u32", partyIDPacket(packet.PartyLeft, 1), 4},
		{"0x0b07 u32", partyIDPacket(packet.PartyLeader, 1), 4},
		{"0x0b13 u32", partyIDPacket(packet.RaidSubLeader, 1), 4},
		{"0x0b17 u32", partyIDPacket(packet.RaidLeader, 1), 4},
		{"0x0214 00595680", buildStatsReset(1, 1, 1, 1, 1, 1, 1), 15},
		{"0x0226 0058fc50", buildChangeAppearance(1, 1, 1, 1, 1), 8},
		{"0x0303 0058e360", buildCharacterShape(ch, 0, nil, 0), 95},
		{"0x0304 0058f260", buildMobState(m), 10},
		{"0x0305 0058f2d0", buildMobTargetHP(m), 8},
		{"0x0401 0058f320", buildMapAddItem(mi), 20},
		{"0x0402 u32", buildMapRemoveItem(1), 4},
		{"0x0404 u32", buildWorldDay(), 4},
		{"0x0406 u32", buildCharacterReviveInit(1), 4},
		{"0x0501 0058f4f0", buildCharacterMove(ch, 0, 0), 19},
		{"0x0503 0058f6d0", buildCharacterMobAttack(1, 1, 0, 1), 15},
		{"0x0504 0058f760", buildCharacterDeath(1, 2, 1), 9},
		{"0x0505 0058f7c0", buildCharacterReviveHitpoints(1, 1, 1, 1), 16},
		{"0x0506 0058f830", buildCharacterMotion(1, 1), 5},
		{"0x0507 0058f980", buildCharacterEquipment(ch, nil, 0), 14},
		{"0x050a 0058fbc0", buildUseItemResult(1, nil), 9},
		{"0x050b 0058fcd0", buildMaxEnergy(ch, 0, 1), 9},
		{"0x050c 0058fd30", buildSkillUsedSpMp(1, 1), 8},
		{"0x0508 0058fa30", buildLevelUp(packet.LevelUpSelf, ch), 18},
		{"0x051e 0058fac0", buildLevelUp(packet.LevelUpOther, ch), 18},
		{"0x0517 00590370", buildSkillUseResult(packet.SkillUse, 0, 1, 1, 1, 1, 1, 1, 1), 18},
		{"0x050d 0058fd80", buildBuffAdd(1, 1, 1), 7},
		{"0x0609 00591a90", buildMobSpeed(m), 6},
		{"0x0518 slot_168", buildSkillDrainResult(0, 1, 1, 1, 1, [3]uint16{}, [3]uint16{}), 24},
		{"0x0519 00590520", buildSkillUseResult(opSkillOther, 0, 1, 1, 1, 1, 1, 1, 1), 18},
		{"0x060b slot_17c", buildMobSkillPacket(0x060B, 0, 1, 1, sk, [3]uint16{}, nil), 19},
		{"0x060c slot_180", buildMobSkillPacket(0x060C, 0, 1, 1, sk, [3]uint16{}, &[3]uint16{}), 25},
		{"0x060d 00590970", buildMobSkillPacket(0x060D, 0, 1, 1, sk, [3]uint16{}, nil), 18},
		{"0x051c 00591a30", buildCharacterDeathState(1), 6},
		{"0x051f 0058fb50", buildMaxHPMPSP(ch, st), 16},
		{"0x0521 00595090", buildCurrentHitpoints(st), 12},
		{"0x0526 12 dwords", buildAdditionalStats(st), 48},
		{"0x0551 00592090", buildCharacterResurrect(1, 1, 1, 1), 21},
		{"0x0601 00590b50", buildMobEnter(m, false), 15},
		{"0x0602 u32", buildMobLeave(1), 4},
		{"0x0603 00590c00", buildMobMove(m, 0), 13},
		{"0x0605 00590ca0", buildMobAttackCharacter(0, 1, 1, 1), 15},
		{"0x0606 00590d30", buildMobDeath(1, 1), 9},
		{"0x0607 00590df0", buildMobPeriodicDamage(1, 1, 1, 1, 1, 1), 13},
		{"0x0610 00590750", buildMobCurrentHP(1, 1), 8},
		{"0x0702 00590ef0", buildShopBuyResult(0, 1, 1, 1, 1, 1, 1), 10},
		{"0x0703 00590f90", buildShopSellResult(0, 1, 1, 1, 1, 1, 1), 10},
		{"0x020c 00595630", buildGatekeeperResult(true, 1), 5},
		{"0x0801 13 fields", buildGemAddResult(0, 0, 0, 0, 0, 0, 0, 0, 0), 16},
		{"0x0805 005917b0", buildEnchantResult(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, ""), 34},
		{"0x0806 00591170", buildComposeResult(0, 0, 0, ""), 24},
		{"0x0a03 00592cc0", buildTradeStart(1), 5},
		{"0x0a05 00592d40", buildTradeReady(1, true), 2},
		{"0x0a08 00592e30", buildTradeMoney(1, 1), 5},
		{"0x0a09 00597040", buildTradeAddDest(0, nil), 33},
		{"0x0a0a 00592d90", buildTradeConfirm(1, true), 2},
		{"0x0e01 00592f50", buildNpcEnter(n), 21},
		{"0x0e02 u32", buildNpcLeave(1), 4},
		{"0x0e03 00593020", buildNpcMove(n, 0), 17},
		{"0x1106 u8", buildChatError(1), 1},
		{"0x2602 00597160", buildPointBuyResult(0, 0, "", 0, nil), 35},
		{"0x2603 00596b90", buildGiftSendResult(0, "", 0, "", 0), 55},
		{"0x2605 u32", buildCashPoints(1), 4},
		{"0xa301 00592420", buildWorldHandshake(), 18},
	}
	for _, c := range cases {
		if got := len(c.frame) - 4; got != c.body {
			t.Errorf("%s: body %d bytes, client reads %d", c.name, got, c.body)
		}
	}
}
