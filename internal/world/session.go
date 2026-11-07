package world

import (
	"errors"
	"fmt"
	"net"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/Spelunkern/Rapture/internal/crypto"
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
	"github.com/Spelunkern/Rapture/internal/netx"
	"github.com/Spelunkern/Rapture/internal/packet"
	"github.com/Spelunkern/Rapture/internal/session"
)

const (
	loopTick          = 100 * time.Millisecond
	recoveryInterval  = 5 * time.Second
	characterListSize = MaxCharacterSlots
)

// Session is one client connection. Only its own goroutine touches its
// fields; other sessions reach it through its ConnectedPlayer.
type Session struct {
	conn   net.Conn
	out    *connWriter
	log    *logx.Tag
	logger *logx.Logger

	aesRecv, aesSend *crypto.AESCTR
	expanded         bool

	accountID uint32
	sessionID int64
	account   db.Account
	faction   uint8
	maxMode   uint8
	isAdmin   bool

	characters []db.Character
	char       *db.Character
	items      []db.Item
	stored     []db.Item
	skills     []db.CharacterSkill
	quickSlots []db.QuickSlot
	points     uint32
	player     *ConnectedPlayer

	itemCooldowns  map[uint8]time.Time
	skillCooldowns map[uint16]time.Time
	buffs          []ActiveBuff
	cast           *pendingCast
	mounted        bool
	dead           bool
	deathLoss      int       // CUser::Death exp-loss percent, kept until revive
	rebirthAt      time.Time // UpdateRebirth deadline while dead
	rebirthParty   bool      // 0x0553 mode 3 waiting for the party leader

	autoAttack       bool
	autoAttackTarget uint32
	lastAutoAttackAt time.Time

	positionDirty    bool
	lastPositionSave time.Time
	lastRegenAt      time.Time
	lastQuestCheck   time.Time
	usingNpc         uint32
	quests           []*activeQuest
	finishedQuests   []db.FinishedQuest
	regenSitting     bool
	lastCombatAt     time.Time
	lastChatAt       time.Time
}

var (
	activeAccountsMu sync.Mutex
	activeAccounts   = map[uint32]bool{}
)

func acquireAccount(uid uint32) bool {
	activeAccountsMu.Lock()
	defer activeAccountsMu.Unlock()
	if activeAccounts[uid] {
		return false
	}
	activeAccounts[uid] = true
	return true
}

func releaseAccount(uid uint32) {
	activeAccountsMu.Lock()
	delete(activeAccounts, uid)
	activeAccountsMu.Unlock()
}

// Serve runs one client connection until it closes.
func Serve(conn net.Conn, logger *logx.Logger, log *logx.Tag) error {
	s := &Session{conn: conn, out: &connWriter{conn: conn}, log: log, logger: logger,
		faction: FactionNone, maxMode: MaxModeUltimate,
		itemCooldowns: map[uint8]time.Time{}, skillCooldowns: map[uint16]time.Time{},
		lastPositionSave: time.Now(), lastRegenAt: time.Now()}
	defer s.close()
	log.Printf("Rapture World client connected")
	var frames netx.Frames
	buf := make([]byte, 4096)
	for {
		conn.SetReadDeadline(time.Now().Add(loopTick))
		n, err := conn.Read(buf)
		var nerr net.Error
		timeout := err != nil && errors.As(err, &nerr) && nerr.Timeout()
		if err != nil && !timeout && !errors.Is(err, os.ErrDeadlineExceeded) {
			log.Printf("Rapture World client disconnected")
			return nil
		}
		s.tick(n == 0)
		if n == 0 {
			continue
		}
		list, ok := frames.Push(buf[:n])
		for _, f := range list {
			if err := s.frame(f); err != nil {
				return err
			}
		}
		if !ok {
			return fmt.Errorf("malformed world frame")
		}
	}
}

// close persists the active character and releases the account.
func (s *Session) close() {
	s.leaveWorld()
	if s.accountID != 0 {
		if err := db.TryGameLogout(int32(s.accountID), s.sessionID); err != nil {
			s.log.Printf("Rapture World logout procedure failed: %v", err)
		}
		releaseAccount(s.accountID)
	}
}

// leaveWorld saves and unregisters the active character.
func (s *Session) leaveWorld() {
	if s.char == nil {
		return
	}
	s.savePosition()
	s.persistBuffs()
	s.persistSkillDelays()
	s.saveQuests()
	s.useNpc(0)
	if s.player != nil {
		s.player.Unregister()
		s.player = nil
	}
	s.char = nil
}

// send writes a frame to this client: AES-CTR until the initial character
// state, the stateless expanded XOR afterwards.
func (s *Session) send(frame []byte, label string) {
	if s.aesSend == nil || len(frame) <= 2 {
		return
	}
	var body []byte
	if s.expanded {
		body = crypto.ExpandedXOR(frame[2:])
	} else {
		body = s.aesSend.Transform(frame[2:])
	}
	if err := s.out.write(append(append([]byte{}, frame[:2]...), body...)); err != nil {
		s.log.Printf("Rapture World failed to send %s: %v", label, err)
	}
}

func (s *Session) equipment() EquipmentStats {
	e := GetEquipmentStats(s.items)
	applyPassives(&e, s.skills)
	return addBuffs(e, s.buffs)
}
func (s *Session) stats() CharacterStats { return StatsFor(s.char, s.equipment()) }

func (s *Session) sendStatPackets() {
	if s.char == nil {
		return
	}
	st := s.stats()
	for _, p := range statPackets(s.char, &st) {
		s.send(p.Packet, p.Label)
	}
}

// normalize derives the base pools from StatusDefs (Chars only stores the
// current HP/MP/SP) and clamps the current values.
func normalize(ch *db.Character) {
	ch.MaxHP, ch.MaxMP, ch.MaxSP = basePools(ch)
	ch.CurrentHP, ch.CurrentMP, ch.CurrentSP = min(ch.CurrentHP, ch.MaxHP), min(ch.CurrentMP, ch.MaxMP), min(ch.CurrentSP, ch.MaxSP)
}

// tick runs the per-iteration work: mailboxes, buffs, regen, casts and
// the auto-attack swing.
func (s *Session) tick(idle bool) {
	if s.char == nil {
		return
	}
	if p := s.player; p != nil {
		if t := p.TakeTeleport(); t != nil {
			s.applyTeleport(t.MapID, t.X, t.Y, t.Z, "queued teleport")
		}
		if r := p.TakeTradeRefresh(); r != nil {
			s.applyTradeRefresh(r)
		}
		for _, mob := range p.TakeQuestKills() {
			s.questKill(mob)
		}
		exp, gold := p.TakeRewards()
		for _, e := range exp {
			s.awardExp(e, true)
		}
		if len(gold) > 0 {
			for _, g := range gold {
				s.char.Money = uint32(min(uint64(s.char.Money)+uint64(g), db.MoneyCap))
			}
			s.persist()
			s.send(buildSetMoney(s.char.Money), "0x0213 party gold share")
		}
		for _, d := range p.TakeDamage() {
			if s.dead {
				break
			}
			if s.applyIncomingMobDamage(d) {
				s.dead, s.autoAttack, s.cast = true, false, nil
				s.lastAutoAttackAt = time.Time{}
				s.clearBuffsOnDeath()
			}
		}
	}
	s.processBuffs()
	s.tickSkillOnce()
	s.tickRebirth(time.Now())
	if s.char == nil {
		return
	}
	s.recoveryTick()
	if time.Since(s.lastQuestCheck) >= 5*time.Second {
		s.lastQuestCheck = time.Now()
		s.questTimers()
		s.syncQuestIDs()
	}
	s.processPendingCast()
	if idle && s.autoAttack && !s.autoAttackSwing(s.autoAttackTarget) {
		s.autoAttack = false
	}
}

func (s *Session) applyTradeRefresh(r *tradeRefresh) {
	items, err := db.CharacterItems(s.char.ID)
	if err != nil {
		s.log.Printf("Rapture World trade refresh failed: %v", err)
		return
	}
	s.items = items
	s.char.Money = uint32(min(max(int64(s.char.Money)+r.MoneyDelta, 0), db.MoneyCap))
	s.persist()
	if s.player != nil {
		s.player.UpdateItems(s.items)
	}
	for _, p := range r.Packets {
		s.send(p, "0x0206/0x0205 trade refresh item")
	}
	s.send(buildSetMoney(s.char.Money), "0x0213 trade refresh money")
}

// recoveryTick is the original 5-second recovery: nothing in combat,
// 5% of each max while sitting, and while standing only flat equipment
// recovery (not modeled yet, so none). Like the original, the posture
// seen at one tick decides the next tick's amount. Viewers get 0x0505
// (deltas) and the character 0x0521 when a pool tops out or it sits.
func (s *Session) recoveryTick() {
	if time.Since(s.lastRegenAt) < recoveryInterval {
		return
	}
	s.lastRegenAt = time.Now()
	sitting := s.regenSitting
	if s.player != nil {
		s.regenSitting = s.player.IsSitting()
	}
	if s.dead || s.inCombat() || !sitting {
		return
	}
	st := s.stats()
	if st.CurrentHP == 0 {
		return
	}
	hp, mp, sp := s.healDelta(sittingRecovery(st.MaxHP, st.SitRecover[0]), sittingRecovery(st.MaxMP, st.SitRecover[2]),
		sittingRecovery(st.MaxSP, st.SitRecover[1]))
	if hp+mp+sp == 0 {
		return
	}
	s.persist()
	after := s.stats()
	sendNear(s.char.MapID, s.char.PosX, s.char.PosZ, BroadcastRadius,
		buildCharacterReviveHitpoints(s.char.ID, hp, mp, sp), "0x0505 recover")
	s.send(buildCurrentHitpoints(&after), "0x0521 recover set")
}

// inCombat approximates the original attack mode: swinging, or a hit
// given or taken within the last recovery interval.
func (s *Session) inCombat() bool {
	return s.autoAttack || time.Since(s.lastCombatAt) < recoveryInterval
}

// deadGated lists the inputs ignored while dead (revive is 0x0553).
func deadGated(op packet.Opcode) bool {
	switch op {
	case packet.CharacterMove, packet.CharacterMobAutoAttack, packet.SkillUse, packet.SkillUseNoTarget,
		packet.UseItem, packet.ChangeAppearance, packet.UseVehicle:
		return true
	}
	return false
}

func (s *Session) frame(f []byte) error {
	if s.aesRecv == nil {
		op, _ := packet.U16(f[2:4])
		if packet.Opcode(op) == packet.GameHandshake && len(f) >= 24 {
			return s.handshake(f)
		}
		return nil
	}
	p := s.aesRecv.Transform(f[2:])
	if len(p) < 2 {
		return nil
	}
	o, _ := packet.U16(p)
	op := packet.Opcode(o)
	if op != packet.CharacterMove {
		s.log.Printf("Rapture World opcode=0x%04X", o)
	}
	if s.dead && deadGated(op) {
		return nil
	}
	s.dispatch(op, p)
	return nil
}

func (s *Session) handshake(f []byte) error {
	uid, _ := packet.U32(f[4:8])
	var identity [16]byte
	copy(identity[:], f[8:24])
	h, err := session.Consume(uid, identity)
	if err != nil {
		s.log.Printf("Rapture World rejected handshake for user_uid=%d: %v", uid, err)
		return nil
	}
	if !acquireAccount(uid) {
		s.log.Printf("Rapture World rejected user_uid=%d: already connected", uid)
		return nil
	}
	s.accountID, s.sessionID = uid, h.SessionID
	if a, err := db.AccountByUID(int32(uid)); err == nil {
		s.account = a
		s.faction, s.maxMode, s.isAdmin = a.Country, a.MaxGrow, a.Admin || a.AdminLevel > 0
		if !isValidFaction(s.faction) {
			s.faction = FactionNone
		}
		s.log.SetTag(s.log.Tag() + "/" + a.UserID)
	} else {
		s.log.Printf("Rapture World account user_uid=%d not readable: %v", uid, err)
	}
	var key, loginIV [16]byte
	copy(key[:], h.AESMaterial[:16])
	copy(loginIV[:], h.AESMaterial[16:])
	iv := crypto.DeriveWorldAESIV(loginIV)
	s.aesRecv, s.aesSend = crypto.NewAESCTR(key, iv), crypto.NewAESCTR(key, iv)
	s.log.Printf("Rapture World session ready user_uid=%d faction=%d max_mode=%d admin=%t", uid, s.faction, s.maxMode, s.isAdmin)
	s.send(buildWorldHandshake(), "0xA301 handshake ok")
	s.sendCharacterSelect()
	return nil
}

func (s *Session) sendCharacterSelect() {
	chars, err := db.CharactersForAccount(s.accountID)
	if err != nil {
		s.log.Printf("Rapture World failed to list characters: %v", err)
	}
	s.characters = chars
	if s.faction != FactionNone {
		for slot := uint8(0); slot < characterListSize; slot++ {
			frame := buildEmptyCharacterListSlot(slot)
			for i := range chars {
				if chars[i].Slot == slot {
					normalize(&chars[i])
					items, _ := db.CharacterItems(chars[i].ID)
					frame = buildCharacterListSlot(&chars[i], items)
				}
			}
			s.send(frame, "0x0101 character slot")
		}
	}
	s.send(buildAccountFaction(s.faction, s.maxMode), "0x0109 account faction")
}

func (s *Session) dispatch(op packet.Opcode, p []byte) {
	switch op {
	case packet.AccountFaction:
		if len(p) >= 3 && isValidFaction(p[2]) {
			s.faction = p[2]
			if err := db.SaveCountry(int32(s.accountID), s.faction); err != nil {
				s.log.Printf("Rapture World failed to save faction: %v", err)
			}
			s.send(buildAccountFaction(s.faction, s.maxMode), "0x0109 account faction selected")
		}
	case packet.CreateCharacter:
		s.handleCreateCharacter(p)
	case packet.DeleteCharacter:
		if len(p) >= 6 {
			id, _ := packet.U32(p[2:])
			ok, err := db.DeleteCharacter(s.accountID, id)
			s.logErr("delete character", err)
			s.send(buildCharResult(packet.DeleteCharacter, ok, id), "0x0103 delete character")
		}
	case packet.RestoreCharacter:
		if len(p) >= 6 {
			id, _ := packet.U32(p[2:])
			ok, err := db.RestoreCharacter(s.accountID, id)
			s.logErr("restore character", err)
			s.send(buildCharResult(packet.RestoreCharacter, ok, id), "0x010F restore character")
		}
	case packet.RenameCharacter:
		s.handleRename(p)
	case packet.SelectCharacter:
		if len(p) >= 6 {
			id, _ := packet.U32(p[2:])
			s.selectCharacter(id)
		}
	case packet.ChatNormal, packet.ChatWorld, packet.ChatMap, packet.ChatNormalAdmin, packet.ChatWorldAdmin:
		s.handleChat(op, p)
	case packet.UseItem:
		s.handleUseItem(p)
	case packet.ChangeAppearance:
		s.handleChangeAppearance(p)
	case packet.EnchantAdd:
		s.handleEnchantAdd(p)
	case packet.GemAdd:
		s.handleGemAdd(p)
	case packet.GemRemove:
		s.handleGemRemove(p)
	case packet.ItemCompose:
		s.handleItemCompose(p)
	case packet.RemoveItem:
		s.handleDropItem(p)
	case packet.AddItem:
		s.handlePickupItem(p)
	case packet.ItemBagToBag:
		s.handleItemBagToBag(p)
	case packet.ShopSell:
		s.handleShopSell(p)
	case packet.ShopBuy:
		s.handleShopBuy(p)
	case packet.QuestStart, packet.QuestStartAlt:
		s.handleQuestStart(p)
	case packet.QuestEnd:
		s.handleQuestEnd(p)
	case packet.QuestChoose:
		s.handleQuestChoose(p)
	case packet.QuestAbandon:
		s.handleQuestAbandon(p)
	case packet.NpcTalk:
		if len(p) >= 6 {
			id, _ := packet.U32(p[2:])
			s.useNpc(id)
		}
	case packet.PartyInvite, packet.PartyInviteName:
		s.handlePartyInvite(op, p)
	case packet.PartyAnswer:
		s.handlePartyAnswer(p)
	case packet.PartyLeft, packet.PartyLeaveAlt:
		s.handlePartyLeave()
	case packet.PartyExpelled:
		s.handlePartyExpel(p)
	case packet.PartyLeader:
		s.handlePartyLeader(p)
	case packet.RaidConvert, packet.RaidDivision, packet.RaidAutoJoin, packet.RaidDisband:
		s.handleRaidSettings(op, p)
	case packet.RaidInvite, packet.RaidInviteName:
		s.handleRaidInvite(op, p)
	case packet.RaidAnswer:
		s.handleRaidAnswer(p)
	case packet.RaidAutoJoinName:
		s.handleRaidAutoJoin(p)
	case packet.RaidSwap, packet.RaidSubLeader, packet.RaidLeader:
		s.handleRaidRoles(op, p)
	case packet.RaidExpel:
		s.handleRaidExpel(p)
	case packet.ChatParty:
		if s.char != nil {
			if msg, err := readChatMessage(p); err == nil {
				s.partyChat(msg)
			}
		}
	case packet.Gatekeeper:
		s.handleGatekeeper(p)
	case packet.ExchangeRequest:
		if len(p) >= 6 {
			id, _ := packet.U32(p[2:])
			s.handleTradeRequest(id)
		}
	case packet.ExchangeAnswer:
		if len(p) >= 3 {
			s.handleTradeAnswer(p[2] == 0)
		}
	case packet.ExchangeReady:
		if len(p) >= 3 {
			s.handleTradeReady(p[2])
		}
	case packet.ExchangeAdd:
		if len(p) >= 6 {
			s.handleTradeAdd(p[2], p[3], p[4], p[5])
		}
	case packet.ExchangeRemove:
		if len(p) >= 3 {
			s.handleTradeRemove(p[2])
		}
	case packet.ExchangeAddMoney:
		if len(p) >= 6 {
			v, _ := packet.U32(p[2:])
			s.handleTradeMoney(v)
		}
	case packet.ExchangeConfirm:
		if len(p) >= 3 {
			s.handleTradeConfirm(p[2] != 0)
		}
	case packet.CharacterMove:
		// Any movement interrupts a cast and the auto-attack.
		s.cast, s.autoAttack = nil, false
		s.handleCharacterMove(p)
	case packet.CharacterMotion:
		s.handleMotion(p)
	case packet.CharacterEnteredPortal:
		if len(p) >= 3 {
			s.handleEnteredPortal(p[2])
		}
	case packet.CharacterEnteredMap:
		s.handleMapLoaded()
	case packet.LearnNewSkill:
		s.handleLearnSkill(p)
	case packet.SetStatPoint:
		s.handleSetStatPoint(p)
	case packet.CharacterSkillBar:
		s.handleSkillBar(p)
	case packet.AutoAttackStop, packet.TargetClear:
		s.autoAttack = false
	case packet.ResurrectRequest:
		s.handleResurrect(p)
	case packet.UseVehicle:
		s.handleUseVehicle()
	case packet.CharacterMobAutoAttack:
		if len(p) >= 6 && s.char != nil {
			id, _ := packet.U32(p[2:])
			s.autoAttack, s.autoAttackTarget = true, id
			if !s.autoAttackSwing(id) {
				s.autoAttack = false
			}
		}
	case packet.SkillUse, packet.SkillUseNoTarget:
		s.handleUseSkill(op, p)
	case packet.TargetMobHpUpdate, packet.TargetMobGetState:
		if len(p) >= 6 && s.char != nil {
			id, _ := packet.U32(p[2:])
			if m, ok := MobByID(s.char.MapID, id); ok {
				if op == packet.TargetMobHpUpdate {
					s.send(buildMobTargetHP(&m), "0x0305 mob target hp")
				} else {
					s.send(buildMobMove(&m, 0), "0x0603 mob target position")
					s.send(buildMobState(&m), "0x0304 mob state")
				}
			}
		}
	case packet.PointBuy:
		s.handlePointBuy(p)
	case packet.PointGiftSend:
		s.handleGiftSend(p)
	case packet.PointStoredItemGet:
		s.handleGiftClaim(p)
	case packet.CashPoint:
		s.send(buildCashPoints(s.points), "0x2605 cash points")
	case packet.Ping:
		s.send(buildPing(), "0xA303 ping")
	case packet.Logout:
		s.handleLogout()
	}
}

func (s *Session) logErr(what string, err error) {
	if err != nil {
		s.log.Printf("Rapture World %s failed: %v", what, err)
	}
}

func (s *Session) handleCreateCharacter(p []byte) {
	if len(p) < 10 {
		return
	}
	nc := db.NewCharacter{Slot: p[2], Race: p[3], Mode: p[4], Hair: p[5], Face: p[6], Height: p[7], Class: p[8],
		Gender: p[9], Name: packet.FixedASCII(p[10:])}
	reject := func(why string) {
		s.log.Printf("Rapture World rejected create character name=%q slot=%d: %s", nc.Name, nc.Slot, why)
		s.send(buildCreateCharacterResult(false), "0x0102 create character rejected")
	}
	if nc.Slot >= MaxCharacterSlots {
		reject("slot out of range")
		return
	}
	// CUser::PacketCharacter: spaces are dropped and at least two
	// characters must remain; the family/job pair and mode must be valid.
	nc.Name = strings.ReplaceAll(nc.Name, " ", "")
	if len(nc.Name) < 2 {
		reject("name too short")
		return
	}
	if !allowedJob(nc.Race, nc.Class) || nc.Mode > s.maxMode {
		reject("invalid race, class or mode")
		return
	}
	if _, used, err := db.CharacterIDInSlot(s.accountID, nc.Slot); err != nil || used {
		reject("slot occupied")
		return
	}
	if taken, err := db.NameTaken(nc.Name); err != nil || taken {
		reject("name in use")
		return
	}
	// Map.ini start position per race; CUser::ProcessCharacterCreate stats
	// and pools.
	start := mapConfig.Start[nc.Race]
	nc.MapID, nc.PosX, nc.PosY, nc.PosZ = start.MapID, start.X, start.Y, start.Z
	nc.Str, nc.Dex, nc.Rec, nc.Int, nc.Wis, nc.Luc = startingStats(nc.Race, nc.Class)
	nc.HP, nc.MP, nc.SP = newCharacterPools(nc.Race, nc.Class, nc.Mode, nc.Rec, nc.Dex, nc.Wis)
	id, err := db.CreateCharacter(s.account, nc)
	if err != nil {
		s.log.Printf("Rapture World create character failed: %v", err)
		reject("database")
		return
	}
	for _, it := range starterItems(nc.Race, nc.Class) {
		if err := db.AddCharacterItem(id, &it); err != nil {
			s.log.Printf("Rapture World starter item %d/%d failed: %v", it.Type, it.TypeID, err)
		}
	}
	s.log.Printf("Rapture World created character id=%d name=%q map=%d", id, nc.Name, nc.MapID)
	s.send(buildCreateCharacterResult(true), "0x0102 create character ok")
	s.sendCharacterSelect()
}

func (s *Session) handleRename(p []byte) {
	if len(p) < 6 {
		return
	}
	id, _ := packet.U32(p[2:])
	name := packet.FixedASCII(p[6:])
	ok := false
	if strings.TrimSpace(name) != "" {
		if taken, err := db.NameTaken(name); err == nil && !taken {
			for _, c := range s.characters {
				if c.ID == id && c.RenamePending {
					ok, err = db.RenameCharacter(s.accountID, id, c.Name, name)
					s.logErr("rename character", err)
				}
			}
		}
	}
	s.log.Printf("Rapture World rename character id=%d name=%q ok=%t", id, name, ok)
	s.send(buildRenameResult(ok), "0x010E rename character")
}

func (s *Session) selectCharacter(id uint32) {
	ch, err := db.CharacterDetail(s.accountID, id)
	if err != nil {
		s.log.Printf("Rapture World select character id=%d failed: %v", id, err)
		s.send(buildCharResult(packet.SelectCharacter, false, id), "0x0104 select character failed")
		return
	}
	normalize(&ch)
	s.char = &ch
	s.log.SetTag(strings.SplitN(s.log.Tag(), "/", 3)[0] + "/" + s.account.UserID + "/" + ch.Name)
	s.items, err = db.CharacterItems(id)
	s.logErr("load items", err)
	s.skills, err = db.CharacterSkills(id)
	s.logErr("load skills", err)
	s.skillCooldowns, s.itemCooldowns = map[uint16]time.Time{}, map[uint8]time.Time{}
	for _, k := range s.skills {
		if k.Delay > 0 {
			s.skillCooldowns[k.SkillID] = time.Now().Add(time.Duration(k.Delay) * time.Second)
		}
	}
	s.buffs = restoreBuffs(id, s.log)
	s.quickSlots, err = db.QuickSlots(id)
	s.logErr("load quick slots", err)
	s.stored, err = db.StoredItems(s.accountID)
	s.logErr("load warehouse", err)
	s.points, err = db.Points(s.accountID)
	s.logErr("load points", err)
	s.loadQuests()
	s.mounted, s.dead, s.cast, s.autoAttack = false, false, nil, false
	s.lastRegenAt = time.Now()
	st := s.stats()
	s.log.Printf("Rapture World selected character id=%d items=%d skills=%d buffs=%d hp=%d/%d", id, len(s.items), len(s.skills), len(s.buffs), st.CurrentHP, st.MaxHP)
	s.send(buildCharResult(packet.SelectCharacter, true, id), "0x0104 select character ok")
	s.sendInitialState(&st)
	s.sendGiftBox()
}

// sendInitialState is the captured character-select sequence; afterwards
// the server->client direction switches to the expanded XOR scheme.
func (s *Session) sendInitialState(st *CharacterStats) {
	ch := s.char
	for _, p := range []labeled{
		{buildWorldDay(), "0x0404 world day"},
		{buildCharacterDetails(ch, st), "0x0105 character details"},
		{buildKillCountUpdate(0, ch.Kills), "0x020E kills"},
		{buildKillCountUpdate(1, ch.Deaths), "0x020E deaths"},
		{buildKillCountUpdate(2, ch.Victories), "0x020E victories"},
		{buildKillCountUpdate(3, ch.Defeats), "0x020E defeats"},
		{buildAdditionalStats(st), "0x0526 additional stats"},
		{buildMaxHPMPSP(ch, st), "0x051F max hp/mp/sp"},
		{buildMaxEnergy(ch, 0, st.MaxHP), "0x050B max hp"},
		{buildMaxEnergy(ch, 1, st.MaxSP), "0x050B max sp"},
		{buildMaxEnergy(ch, 2, st.MaxMP), "0x050B max mp"},
		{buildCurrentHitpoints(st), "0x0521 current hp/mp/sp"},
		{buildSetMoney(ch.Money), "0x0213 set money"},
	} {
		s.send(p.Packet, p.Label)
	}
	for _, p := range buildInventoryPackets(s.items) {
		s.send(p, "0x0106 inventory")
	}
	for _, p := range []labeled{
		{buildCharacterSkills(s.skills), "0x0108 skills"},
	} {
		s.send(p.Packet, p.Label)
	}
	for _, p := range s.questListPackets() {
		s.send(p, "0x0901/0x0906 quests")
	}
	for _, p := range []labeled{
		{buildCharacterActiveBuffs(s.buffs), "0x010A active buffs"},
		{buildBlessInit(s.faction), "0x0211 bless"},
		{buildZeroCount(packet.BankItemList), "0xB101 bank"},
		{buildWarehouseItemList(s.stored), "0x0711 warehouse"},
		{buildZeroCount(packet.TeleportSavePositionList), "0x0225 saved teleports"},
		{buildAccountPoints(s.points), "0x2601 account points"},
		{buildCharacterSkillBar(s.quickSlots), "0x010B skill bar"},
	} {
		s.send(p.Packet, p.Label)
	}
	s.expanded = true
}

// handleMapLoaded (0x0201 from the client): send the map state, register
// the player and exchange visibility with nearby players.
func (s *Session) handleMapLoaded() {
	ch := s.char
	if ch == nil {
		return
	}
	s.send(buildCharacterMapPosition(ch), "0x020B set map position")
	s.send(buildCharacterEnteredMap(ch, s.isAdmin, true), "0x0201 entered map")
	s.send(buildCharacterShape(ch, s.faction, s.items, 0), "0x0303 character shape")
	s.send(buildCharacterMove(ch, 0, 0), "0x0501 character move")
	if s.player == nil {
		s.player = RegisterPlayer(*ch, s.items, s.isAdmin, s.faction, s.out, s.logger)
	} else {
		s.clearView(false)
		s.player.SetMap(ch.MapID)
		s.player.UpdateCharacter(*ch)
		s.player.UpdateItems(s.items)
	}
	// CZone::EnterUser + SendCellInfoView: the 5x5 cells around the user.
	s.placeView()
	near := EntitiesInView(ch.MapID, cellFor(ch.PosX, ch.PosZ))
	others := s.player.knownIDs(viewUser)
	s.log.Printf("Rapture World map loaded map=%d npcs=%d mobs=%d players=%d", ch.MapID, len(near.NPCs), len(near.Mobs), len(others))
}

// handleMotion (0x0506): sit/stand persist; other motions are one-shot.
// The sender gets its own echo, which the client needs to play it.
func (s *Session) handleMotion(p []byte) {
	if len(p) < 3 || s.char == nil {
		return
	}
	motion := p[2]
	if s.player != nil {
		switch motion {
		case MotionSitDown:
			s.player.SetSitting(true)
		case MotionStandUp:
			s.player.SetSitting(false)
		}
	}
	s.broadcastNear(buildCharacterMotion(s.char.ID, motion), "0x0506 character motion", true)
}

func (s *Session) handleChat(op packet.Opcode, p []byte) {
	if s.char == nil {
		return
	}
	msg, err := readChatMessage(p)
	if err != nil {
		return
	}
	if isTownCommand(msg) {
		s.executeTown()
		return
	}
	s.handlePublicChat(op, msg)
}

// handleLogout (0x0107) returns to character select on the same
// connection; the account stays held.
func (s *Session) handleLogout() {
	id := uint32(0)
	if s.char != nil {
		id = s.char.ID
	}
	s.leaveWorld()
	s.buffs, s.cast, s.mounted, s.dead, s.autoAttack = nil, nil, false, false, false
	s.items, s.skills, s.quickSlots, s.stored, s.points = nil, nil, nil, nil, 0
	s.send(buildLeaveWorldAck(id), "0x0107 leave world ack")
	s.expanded = false
	s.sendCharacterSelect()
}
