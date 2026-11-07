package world

import (
	"strings"
	"sync"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// Guilds follow ps_game.exe CGuildMgr / CGuild / CGuildCreate and
// CUser::PacketGuild 0x004764a0. The original game server delegated every
// write to the DB agent (0x0Bxx); Rapture calls the same procedures.
//
// Ranks are the guild levels: 1 master, 2 sub-masters (with the master the
// "admins", at most 7), 3-9 members (new members join at 9); 0 is a
// pending applicant.

const (
	guildRankMaster  = 1
	guildRankAdmin   = 2
	guildRankMember  = 9
	guildMaxAdmins   = 7
	guildCreateParty = 7    // the creating party has exactly 7 members
	guildCreateFee   = 1000 // gold taken from the master
	guildMinLevel    = 11
	guildInviteTime  = 30 * time.Second
	guildRejoinTime  = 72 * time.Hour // join date + 72 h (usp_Read_Char_GuildJoinDate_R)
	guildListBatch   = 14             // 0x0D2F entries per packet
	guildMemberBatch = 40             // 0x0D12/0x0D13/0x0D16 entries per packet

	// 0x0D0C status modes; a level byte (1-80) is a character level change
	// and 200 + rank a rank change.
	guildStatusDisband = 0x65
	guildStatusLeft    = 0x66
	guildStatusExpel   = 0x67
	guildStatusOnline  = 0x68
	guildStatusOffline = 0x69
	guildStatusRank    = 200
)

type guildMember struct {
	CharID uint32
	Name   string
	Job    uint8
	Level  uint16
	Rank   uint8
	player *ConnectedPlayer // nil when offline
}

type guild struct {
	ID         uint32
	Name       string
	Master     string
	Remark     string
	Country    uint8
	Points     uint32
	Rank       uint8
	Etin       uint32
	members    map[uint32]*guildMember
	applicants map[uint32]*guildMember
}

// guildCreate is CGuildCreate: a creation (disband false) or disband vote.
type guildCreate struct {
	disband  bool
	name     string
	remark   string
	masterID uint32
	master   *ConnectedPlayer
	pending  map[uint32]bool // ids that have not accepted yet
	players  []*ConnectedPlayer
	accepted int
	guild    *guild // the guild being disbanded
}

// Per-player guild state, guarded by guildMu.
type playerGuildState struct {
	guild       *guild
	create      *guildCreate
	inviterID   uint32
	inviteUntil time.Time
	applied     *guild
	rejoinUntil time.Time
	userID      string
}

var (
	guildMu       sync.Mutex
	guildsByID    = map[uint32]*guild{}
	guildOfChar   = map[uint32]*guild{} // members and applicants
	nextGuildID   uint32
	guildsLoaded  bool
	guildFeeQueue = map[*ConnectedPlayer]uint32{}
)

// LoadGuilds is CGuildMgr::InitAddGuild / InitAddUser from the database.
func LoadGuilds(log *logx.Logger) error {
	rows, members, err := db.LoadGuilds()
	if err != nil {
		return err
	}
	next, err := db.NextGuildID()
	if err != nil {
		return err
	}
	guildMu.Lock()
	defer guildMu.Unlock()
	guildsByID, guildOfChar = map[uint32]*guild{}, map[uint32]*guild{}
	for _, r := range rows {
		guildsByID[r.ID] = &guild{ID: r.ID, Name: r.Name, Master: r.MasterName, Remark: r.Remark, Country: r.Country,
			Points: r.Points, Rank: r.Rank, Etin: r.Etin, members: map[uint32]*guildMember{}, applicants: map[uint32]*guildMember{}}
		next = max(next, r.ID+1)
	}
	for _, m := range members {
		g := guildsByID[m.GuildID]
		if g == nil {
			continue
		}
		e := &guildMember{CharID: m.CharID, Name: m.Name, Job: m.Job, Level: m.Level, Rank: m.Rank}
		// InitAddUser: the guild's master name always holds rank 1.
		if strings.EqualFold(m.Name, g.Master) {
			e.Rank = guildRankMaster
		}
		if e.Rank == 0 {
			g.applicants[e.CharID] = e
		} else {
			g.members[e.CharID] = e
		}
		guildOfChar[e.CharID] = g
	}
	nextGuildID, guildsLoaded = next, true
	log.Printf("Rapture World loaded guilds=%d members=%d next_id=%d", len(rows), len(members), next)
	return nil
}

func (g *guild) admins() int {
	n := 0
	for _, m := range g.members {
		if m.Rank <= guildRankAdmin {
			n++
		}
	}
	return n
}

// send is CGuild::Send: every online member.
func (g *guild) send(frame []byte, label string, except *ConnectedPlayer) {
	for _, m := range g.members {
		if m.player != nil && m.player != except {
			m.player.Send(frame, label)
		}
	}
}

// sendAdmin is CGuild::SendAdmin: the online master and sub-masters.
func (g *guild) sendAdmin(frame []byte, label string) {
	for _, m := range g.members {
		if m.player != nil && m.Rank <= guildRankAdmin {
			m.player.Send(frame, label)
		}
	}
}

func guildStatus(mode uint8, charID uint32) []byte {
	w := packet.New(packet.GuildStatus)
	w.U8(mode)
	w.U32(charID)
	return w.MustFinish()
}

func writeGuildMember(w *packet.Writer, m *guildMember) {
	w.U32(m.CharID)
	w.U8(m.Rank)
	w.U16(m.Level)
	w.U8(m.Job)
	w.FixedString(m.Name, 21)
}

// buildGuildMemberLists is SendGuildUserList: 0x0D12 online and 0x0D13
// offline members, 29 bytes each, 40 per packet, none sent when empty.
func (g *guild) memberListPackets() [][]byte {
	var out [][]byte
	for _, online := range []bool{true, false} {
		op := packet.GuildOnlineList
		if !online {
			op = packet.GuildOfflineList
		}
		var batch []*guildMember
		flush := func() {
			if len(batch) == 0 {
				return
			}
			w := packet.New(op)
			w.U8(uint8(len(batch)))
			for _, m := range batch {
				writeGuildMember(w, m)
			}
			out = append(out, w.MustFinish())
			batch = nil
		}
		for _, m := range g.members {
			if (m.player != nil) == online {
				batch = append(batch, m)
				if len(batch) == guildMemberBatch {
					flush()
				}
			}
		}
		flush()
	}
	return out
}

func writeGuildApplicant(w *packet.Writer, m *guildMember) {
	w.U32(m.CharID)
	w.U16(m.Level)
	w.U8(m.Job)
	w.FixedString(m.Name, 21)
}

// applicantListPackets is SendGuildJoinList (0x0D16, 28 bytes each).
func (g *guild) applicantListPackets() [][]byte {
	var out [][]byte
	var batch []*guildMember
	flush := func() {
		if len(batch) == 0 {
			return
		}
		w := packet.New(packet.GuildApplicantList)
		w.U8(uint8(len(batch)))
		for _, m := range batch {
			writeGuildApplicant(w, m)
		}
		out = append(out, w.MustFinish())
		batch = nil
	}
	for _, m := range g.applicants {
		batch = append(batch, m)
		if len(batch) == guildMemberBatch {
			flush()
		}
	}
	flush()
	return out
}

// writeGuildInfo is the 120-byte guild record of 0x0D2F and 0x0D30.
func writeGuildInfo(w *packet.Writer, g *guild) {
	w.U32(g.ID)
	w.FixedString(g.Name, 25)
	w.FixedString(g.Master, 21)
	w.FixedString(g.Remark, 65)
	w.U8(g.Rank)
	w.U32(g.Points)
}

// sendWorld is CWorld::SendAll restricted to a country.
func guildSendCountry(country uint8, frame []byte, label string) {
	for _, p := range AllPlayers() {
		if p.Faction() == country {
			p.Send(frame, label)
		}
	}
}

func buildGuildCreateFailed(result uint8, name string) []byte {
	w := packet.New(packet.GuildCreateFailed)
	w.U8(result)
	w.U32(0)
	w.U8(0)
	w.FixedString(name, 25)
	w.Bytes(make([]byte, 98-2-31)) // the original sends a 98-byte frame
	return w.MustFinish()
}

func buildGuildCreateResult(result uint8, id uint32, rank uint8, name, remark string) []byte {
	w := packet.New(packet.GuildCreate)
	w.U8(result)
	w.U32(id)
	w.U8(rank)
	w.FixedString(name, 25)
	w.FixedString(remark, 65)
	return w.MustFinish()
}

func buildGuildJoined(result uint8, g *guild, rank uint8) []byte {
	w := packet.New(packet.GuildJoinResult)
	w.U8(result)
	w.U32(g.ID)
	w.U8(rank)
	w.FixedString(g.Name, 25)
	return w.MustFinish()
}

func guildResult(op packet.Opcode, result uint8) []byte {
	w := packet.New(op)
	w.U8(result)
	return w.MustFinish()
}

func guildResultID(op packet.Opcode, result uint8, id uint32) []byte {
	w := packet.New(op)
	w.U8(result)
	w.U32(id)
	return w.MustFinish()
}

func guildID32(op packet.Opcode, id uint32) []byte {
	w := packet.New(op)
	w.U32(id)
	return w.MustFinish()
}

// guildNameFor / guildIDFor feed the character packets (0x0105, 0x0201,
// 0x0303). They must not be called with a player's mu held.
func guildNameFor(charID uint32) string {
	guildMu.Lock()
	defer guildMu.Unlock()
	if g := guildOfChar[charID]; g != nil && g.members[charID] != nil {
		return g.Name
	}
	return ""
}

func guildIDFor(charID uint32) uint32 {
	guildMu.Lock()
	defer guildMu.Unlock()
	if g := guildOfChar[charID]; g != nil && g.members[charID] != nil {
		return g.ID
	}
	return 0
}

// guildConnect is CUser::InitGuild + CGuildMgr::ConnectUser +
// SendCharacterGuild at world entry, then CGuildMgr::SendGuildList.
func (s *Session) guildConnect() {
	p, ch := s.player, s.char
	if p == nil || ch == nil {
		return
	}
	hours, err := db.GuildJoinRemainHours(ch.ID)
	s.logErr("guild join date", err)
	guildMu.Lock()
	p.guild.userID = s.account.UserID
	if hours > 0 {
		p.guild.rejoinUntil = time.Now().Add(time.Duration(hours) * time.Hour)
	}
	var lists [][]byte
	if g := guildOfChar[ch.ID]; g != nil {
		if m := g.members[ch.ID]; m != nil {
			m.player, m.Level, m.Job, m.Name = p, ch.Level, ch.Class, ch.Name
			p.guild.guild = g
			g.send(guildStatus(guildStatusOnline, ch.ID), "0x0D0C guild member online", p)
			lists = append(lists, g.memberListPackets()...)
			if m.Rank <= guildRankAdmin {
				lists = append(lists, g.applicantListPackets()...)
			}
		} else if g.applicants[ch.ID] != nil {
			p.guild.applied = g
		}
	}
	lists = append(lists, guildListPackets(s.faction)...)
	guildMu.Unlock()
	for _, f := range lists {
		s.send(f, "guild list")
	}
}

// guildListPackets is CGuildMgr::SendGuildList: 0x0D0D, the country's
// guilds in 0x0D2F batches of 14, 0x0D0E.
func guildListPackets(country uint8) [][]byte {
	out := [][]byte{packet.New(packet.GuildListBegin).MustFinish()}
	var batch []*guild
	flush := func() {
		if len(batch) == 0 {
			return
		}
		w := packet.New(packet.GuildList)
		w.U8(uint8(len(batch)))
		for _, g := range batch {
			writeGuildInfo(w, g)
		}
		out = append(out, w.MustFinish())
		batch = nil
	}
	for _, g := range guildsByID {
		if g.Country == country {
			batch = append(batch, g)
			if len(batch) == guildListBatch {
				flush()
			}
		}
	}
	flush()
	return append(out, packet.New(packet.GuildListEnd).MustFinish())
}

// guildDisconnect is CUser::ReleaseGuild / CGuild::DisconnectUser at world
// exit; a pending creation or disband vote fails as a refusal.
func (s *Session) guildDisconnect() {
	p := s.player
	if p == nil || s.char == nil {
		return
	}
	guildMu.Lock()
	defer guildMu.Unlock()
	if c := p.guild.create; c != nil {
		c.fail(7)
	}
	if g := p.guild.guild; g != nil {
		if m := g.members[s.char.ID]; m != nil {
			m.player = nil
		}
		g.send(guildStatus(guildStatusOffline, s.char.ID), "0x0D0C guild member offline", p)
	}
	p.guild = playerGuildState{}
}

// guildLevelChanged is CGuild::ChangeUserLevel on a level-up.
func (s *Session) guildLevelChanged() {
	guildMu.Lock()
	defer guildMu.Unlock()
	if g := s.player.guild.guild; g != nil {
		if m := g.members[s.char.ID]; m != nil {
			m.Level = s.char.Level
			g.send(guildStatus(uint8(s.char.Level), s.char.ID), "0x0D0C guild member level", nil)
		}
	}
}

// guildChat (0x1104) is CGuild::SendAllGuildChat: sender name, text.
func (s *Session) guildChat(message string) {
	guildMu.Lock()
	defer guildMu.Unlock()
	g := s.player.guild.guild
	if g == nil {
		return
	}
	w := packet.New(packet.ChatGuild)
	w.FixedString(s.char.Name, 21)
	writeChatText(w, message)
	g.send(w.MustFinish(), "0x1104 guild chat", nil)
}

// handleGuild dispatches the 0x0Dxx requests of CUser::PacketGuild.
func (s *Session) handleGuild(op packet.Opcode, p []byte) {
	if s.char == nil || s.player == nil {
		return
	}
	switch op {
	case packet.GuildCreate:
		s.guildCreateRequest(p)
	case packet.GuildCreateAgree:
		if len(p) >= 3 {
			s.guildCreateAnswer(p[2] == 1)
		}
	case packet.GuildDisband:
		s.guildDisbandRequest()
	case packet.GuildDisbandAgree:
		if len(p) >= 3 {
			s.guildDisbandAnswer(p[2] == 1)
		}
	case packet.GuildInvite:
		if len(p) >= 6 {
			id, _ := packet.U32(p[2:])
			s.guildInvite(id)
		}
	case packet.GuildInviteAnswer:
		if len(p) >= 3 {
			s.guildInviteAnswer(p[2] == 1)
		}
	case packet.GuildApply:
		if len(p) >= 6 {
			id, _ := packet.U32(p[2:])
			s.guildApply(id)
		}
	case packet.GuildJoinResult:
		if len(p) >= 7 {
			id, _ := packet.U32(p[3:])
			s.guildAnswerApplicant(p[2] == 1, id)
		}
	case packet.GuildLeave:
		s.guildLeave()
	case packet.GuildExpel:
		if len(p) >= 6 {
			id, _ := packet.U32(p[2:])
			s.guildExpel(id)
		}
	case packet.GuildStatus:
		if len(p) >= 7 {
			id, _ := packet.U32(p[3:])
			s.guildChangeRank(p[2], id)
		}
	case packet.GuildRemark:
		s.guildRemark(p)
	case packet.GuildEtin:
		guildMu.Lock()
		g := s.player.guild.guild
		var etin uint32
		if g != nil {
			etin = g.Etin
		}
		guildMu.Unlock()
		if g != nil {
			s.send(guildID32(packet.GuildEtin, etin), "0x0D23 guild etin")
		}
	}
}

func fixedText(p []byte, at, n int) string {
	if len(p) < at {
		return ""
	}
	b := p[at:min(len(p), at+n)]
	for i, c := range b {
		if c == 0 {
			b = b[:i]
			break
		}
	}
	return string(b)
}

// guildCreateRequest (0x0D21): name[25], remark[65] (64 characters).
func (s *Session) guildCreateRequest(p []byte) {
	ch, me := s.char, s.player
	name, remark := fixedText(p, 2, 24), fixedText(p, 0x1b, 64)
	fail := func(result uint8) {
		s.send(buildGuildCreateResult(result, 0, 0, name, remark), "0x0D21 guild create refused")
	}
	if ch.Level < guildMinLevel {
		fail(4)
		return
	}
	if ch.Mode < growNormal {
		fail(5)
		return
	}
	partyMu.Lock()
	var party []*ConnectedPlayer
	if pt := partyOf(me); pt != nil {
		party = append(party, pt.members...)
	}
	partyMu.Unlock()
	guildMu.Lock()
	defer guildMu.Unlock()
	if me.guild.guild != nil || me.guild.create != nil {
		fail(8)
		return
	}
	if len(party) != guildCreateParty {
		fail(3)
		return
	}
	now := time.Now()
	for _, m := range party {
		switch {
		case m.guild.guild != nil:
			fail(8)
			return
		case m.Snapshot().Mode < growNormal:
			fail(5)
			return
		case m.guild.create != nil || m.MapID() != ch.MapID:
			fail(3)
			return
		case now.Before(m.guild.rejoinUntil):
			fail(9)
			return
		}
	}
	for _, g := range guildsByID {
		if strings.EqualFold(g.Name, name) {
			fail(6)
			return
		}
	}
	c := &guildCreate{name: name, remark: remark, masterID: ch.ID, master: me, pending: map[uint32]bool{}}
	for _, m := range party {
		if g := m.guild.applied; g != nil {
			g.cancelApplication(m)
		}
		c.pending[m.CharacterID] = true
		c.players = append(c.players, m)
		m.guild.create = c
	}
	// SendGuildCreateAgree (0x0D22): master id, name, remark to all.
	w := packet.New(packet.GuildCreateAgree)
	w.U32(ch.ID)
	w.FixedString(name, 25)
	w.FixedString(remark, 65)
	c.sendAll(w.MustFinish(), "0x0D22 guild create agree")
}

func (c *guildCreate) sendAll(frame []byte, label string) {
	for _, m := range c.players {
		m.Send(frame, label)
	}
}

// fail is SendGuildCreateFailed / SendGuildFireFailed then ResetCreate.
func (c *guildCreate) fail(result uint8) {
	if c.disband {
		c.sendAll(guildResult(packet.GuildDisband, result), "0x0D03 guild disband failed")
	} else {
		c.sendAll(buildGuildCreateFailed(result, c.name), "0x0D01 guild create failed")
	}
	c.reset()
}

func (c *guildCreate) reset() {
	for _, m := range c.players {
		if m.guild.create == c {
			m.guild.create = nil
		}
	}
}

// guildCreateAnswer (0x0D22): accept byte; 7 acceptances create it.
func (s *Session) guildCreateAnswer(accept bool) {
	guildMu.Lock()
	defer guildMu.Unlock()
	me := s.player
	c := me.guild.create
	if me.guild.guild != nil || c == nil || c.disband {
		return
	}
	if !accept {
		c.fail(7)
		return
	}
	if !c.pending[s.char.ID] {
		return
	}
	delete(c.pending, s.char.ID)
	if c.accepted++; c.accepted >= guildCreateParty {
		c.complete(s.logger)
		c.reset()
	}
}

// complete is CGuildMgr::CreateGuild 0x004321e0.
func (c *guildCreate) complete(log *logx.Logger) {
	for _, g := range guildsByID {
		if strings.EqualFold(g.Name, c.name) {
			c.sendAll(buildGuildCreateFailed(6, c.name), "0x0D01 guild create failed")
			return
		}
	}
	master := c.master.Snapshot()
	if master.Money < guildCreateFee {
		c.sendAll(buildGuildCreateFailed(2, c.name), "0x0D01 guild create failed")
		return
	}
	g := &guild{ID: nextGuildID, Name: c.name, Master: master.Name, Remark: c.remark, Country: c.master.Faction(),
		Rank: 31, members: map[uint32]*guildMember{}, applicants: map[uint32]*guildMember{}}
	if err := db.CreateGuild(g.ID, g.Name, c.master.guild.userID, master.ID, g.Master, g.Country, g.Remark); err != nil {
		log.Printf("Rapture World guild create failed: %v", err)
		c.sendAll(buildGuildCreateFailed(6, c.name), "0x0D01 guild create failed")
		return
	}
	nextGuildID++
	guildsByID[g.ID] = g
	guildFeeQueue[c.master] += guildCreateFee
	for _, p := range c.players {
		snap := p.Snapshot()
		rank := uint8(guildRankMember)
		if p == c.master {
			rank = guildRankMaster
		}
		g.members[p.CharacterID] = &guildMember{CharID: p.CharacterID, Name: snap.Name, Job: snap.Class, Level: snap.Level,
			Rank: rank, player: p}
		guildOfChar[p.CharacterID] = g
		p.guild.guild, p.guild.applied = g, nil
		if err := db.AddGuildChar(g.ID, p.CharacterID, rank); err != nil {
			log.Printf("Rapture World guild member insert failed: %v", err)
		}
	}
	for _, p := range c.players {
		p.Send(buildGuildCreateResult(1, g.ID, g.members[p.CharacterID].Rank, g.Name, g.Remark), "0x0D21 guild created")
		for _, f := range g.memberListPackets() {
			p.Send(f, "0x0D12 guild members")
		}
	}
	w := packet.New(packet.GuildNew)
	writeGuildInfo(w, g)
	guildSendCountry(g.Country, w.MustFinish(), "0x0D30 new guild")
	log.Printf("Rapture World guild created id=%d name=%q master=%q", g.ID, g.Name, g.Master)
}

// takeGuildFee applies a queued creation fee (CreateGuild's 0x0213).
func (s *Session) takeGuildFee() {
	guildMu.Lock()
	fee := guildFeeQueue[s.player]
	delete(guildFeeQueue, s.player)
	guildMu.Unlock()
	if fee == 0 {
		return
	}
	s.char.Money = satSub(s.char.Money, fee)
	s.persist()
	s.send(buildSetMoney(s.char.Money), "0x0213 guild fee")
}

// guildDisbandRequest (0x0D03): a master with at most 2 members disbands
// at once; otherwise two more party admins of the guild must agree.
func (s *Session) guildDisbandRequest() {
	me := s.player
	partyMu.Lock()
	var party []*ConnectedPlayer
	if pt := partyOf(me); pt != nil {
		party = append(party, pt.members...)
	}
	partyMu.Unlock()
	guildMu.Lock()
	defer guildMu.Unlock()
	g := me.guild.guild
	m := (*guildMember)(nil)
	if g != nil {
		m = g.members[s.char.ID]
	}
	if g == nil || me.guild.create != nil || m == nil || m.Rank != guildRankMaster {
		s.send(guildResult(packet.GuildDisband, 0), "0x0D03 guild disband refused")
		return
	}
	if len(g.members) <= 2 {
		g.fire(s.logger)
		return
	}
	c := &guildCreate{disband: true, masterID: s.char.ID, master: me, guild: g, pending: map[uint32]bool{}}
	c.players = append(c.players, me)
	c.pending[s.char.ID] = true
	for _, p := range party {
		if p == me || p.MapID() != s.char.MapID || p.guild.create != nil {
			continue
		}
		if pm := g.members[p.CharacterID]; pm != nil && pm.Rank <= guildRankAdmin {
			c.players = append(c.players, p)
			c.pending[p.CharacterID] = true
		}
	}
	if len(c.players) < 3 {
		s.send(guildResult(packet.GuildDisband, 2), "0x0D03 guild disband refused")
		return
	}
	for _, p := range c.players {
		p.guild.create = c
	}
	c.sendAll(guildID32(packet.GuildDisbandAgree, s.char.ID), "0x0D04 guild disband agree")
}

// guildDisbandAnswer (0x0D04): more than 2 acceptances disband it.
func (s *Session) guildDisbandAnswer(accept bool) {
	guildMu.Lock()
	defer guildMu.Unlock()
	c := s.player.guild.create
	if c == nil || !c.disband || s.player.guild.guild == nil {
		return
	}
	if !accept {
		c.fail(3)
		return
	}
	if !c.pending[s.char.ID] {
		return
	}
	delete(c.pending, s.char.ID)
	if c.accepted++; c.accepted > 2 {
		c.reset()
		c.guild.fire(s.logger)
	}
}

// fire is CGuildMgr::FireGuild / CGuild::Fire.
func (g *guild) fire(log *logx.Logger) {
	g.send(guildStatus(guildStatusDisband, 0), "0x0D0C guild disbanded", nil)
	for id, m := range g.members {
		if m.player != nil {
			m.player.guild.guild = nil
		}
		delete(guildOfChar, id)
	}
	for id, m := range g.applicants {
		if p := FindPlayerByID(m.CharID); p != nil {
			p.guild.applied = nil
		}
		delete(guildOfChar, id)
	}
	delete(guildsByID, g.ID)
	if err := db.DeleteGuild(g.ID); err != nil {
		log.Printf("Rapture World guild delete failed: %v", err)
	}
	guildSendCountry(g.Country, guildID32(packet.GuildDisbanded, g.ID), "0x0D11 guild disbanded")
	log.Printf("Rapture World guild disbanded id=%d name=%q", g.ID, g.Name)
}

// guildInvite (0x0D05): an admin invites a user of its map and country.
func (s *Session) guildInvite(targetID uint32) {
	me, ch := s.player, s.char
	refuse := func() { s.send(guildResultID(packet.GuildInviteAnswer, 0, targetID), "0x0D06 guild invite refused") }
	t := FindPlayerByID(targetID)
	if t == nil || t == me || t.MapID() != ch.MapID || t.Faction() != s.faction {
		refuse()
		return
	}
	guildMu.Lock()
	defer guildMu.Unlock()
	g := me.guild.guild
	if g == nil || g.members[ch.ID] == nil || g.members[ch.ID].Rank > guildRankAdmin {
		refuse()
		return
	}
	now := time.Now()
	invited := t.guild.inviterID != 0 && now.Before(t.guild.inviteUntil)
	if t.guild.guild != nil || t.guild.create != nil || invited || t.guild.applied != nil {
		refuse()
		return
	}
	t.guild.inviterID, t.guild.inviteUntil = ch.ID, now.Add(guildInviteTime)
	t.Send(guildID32(packet.GuildInvite, ch.ID), "0x0D05 guild invitation")
}

// guildInviteAnswer (0x0D06): accept byte, answered to the inviter.
func (s *Session) guildInviteAnswer(accept bool) {
	me := s.player
	guildMu.Lock()
	defer guildMu.Unlock()
	inviterID := me.guild.inviterID
	valid := inviterID != 0 && time.Now().Before(me.guild.inviteUntil)
	me.guild.inviterID, me.guild.inviteUntil = 0, time.Time{}
	if !valid {
		return
	}
	inviter := FindPlayerByID(inviterID)
	if inviter == nil || inviter.MapID() != s.char.MapID {
		return
	}
	if !accept {
		inviter.Send(guildResultID(packet.GuildInviteAnswer, 0, inviterID), "0x0D06 guild invite declined")
		return
	}
	g := inviter.guild.guild
	if g == nil || g.members[inviterID] == nil || g.members[inviterID].Rank > guildRankAdmin || me.guild.guild != nil {
		return
	}
	g.joinUser(me, s.logger)
	inviter.Send(guildResultID(packet.GuildInviteAnswer, 1, inviterID), "0x0D06 guild invite accepted")
}

// joinUser is CGuild::JoinUser: an online user joins at rank 9.
func (g *guild) joinUser(p *ConnectedPlayer, log *logx.Logger) {
	snap := p.Snapshot()
	m := &guildMember{CharID: p.CharacterID, Name: snap.Name, Job: snap.Class, Level: snap.Level, Rank: guildRankMember, player: p}
	g.members[m.CharID] = m
	guildOfChar[m.CharID] = g
	p.guild.guild = g
	p.guild.rejoinUntil = time.Now().Add(guildRejoinTime)
	if err := db.AddGuildChar(g.ID, m.CharID, m.Rank); err != nil {
		log.Printf("Rapture World guild join insert failed: %v", err)
	}
	p.Send(buildGuildJoined(1, g, m.Rank), "0x0D08 guild joined")
	g.send(buildGuildMemberAdded(m), "0x0D14 guild member added", nil)
	for _, f := range g.memberListPackets() {
		p.Send(f, "0x0D12 guild members")
	}
}

// buildGuildMemberAdded (0x0D14): online byte and the 29-byte member.
func buildGuildMemberAdded(m *guildMember) []byte {
	w := packet.New(packet.GuildMemberAdded)
	if m.player != nil {
		w.U8(1)
	} else {
		w.U8(0)
	}
	writeGuildMember(w, m)
	return w.MustFinish()
}

// guildApply (0x0D07): guild id; CGuild::JoinRequest.
func (s *Session) guildApply(guildID uint32) {
	me, ch := s.player, s.char
	guildMu.Lock()
	defer guildMu.Unlock()
	if time.Now().Before(me.guild.rejoinUntil) {
		s.send(guildResult(packet.GuildApply, 2), "0x0D07 guild apply too soon")
		return
	}
	g := guildsByID[guildID]
	invited := me.guild.inviterID != 0 && time.Now().Before(me.guild.inviteUntil)
	if ch.Mode == growEasy || me.guild.guild != nil || me.guild.create != nil || invited || g == nil ||
		g.Country != s.faction || g.applicants[ch.ID] != nil {
		s.send(guildResult(packet.GuildApply, 0), "0x0D07 guild apply refused")
		return
	}
	if old := me.guild.applied; old != nil {
		old.cancelApplication(me)
	}
	m := &guildMember{CharID: ch.ID, Name: ch.Name, Job: ch.Class, Level: ch.Level}
	g.applicants[ch.ID] = m
	guildOfChar[ch.ID] = g
	me.guild.applied = g
	if err := db.AddGuildChar(g.ID, ch.ID, 0); err != nil {
		s.log.Printf("Rapture World guild application insert failed: %v", err)
	}
	w := packet.New(packet.GuildApplicant)
	writeGuildApplicant(w, m)
	g.sendAdmin(w.MustFinish(), "0x0D17 guild applicant")
	s.send(guildResult(packet.GuildApply, 1), "0x0D07 guild applied")
}

// cancelApplication is CGuild::JoinRequestCancle.
func (g *guild) cancelApplication(p *ConnectedPlayer) {
	if g.applicants[p.CharacterID] == nil {
		return
	}
	delete(g.applicants, p.CharacterID)
	delete(guildOfChar, p.CharacterID)
	p.guild.applied = nil
	_ = db.DeleteGuildChar(g.ID, p.CharacterID)
	g.sendAdmin(guildID32(packet.GuildApplicantRemoved, p.CharacterID), "0x0D18 guild applicant removed")
}

// guildAnswerApplicant (0x0D08): accept byte, char id; JoinOK / JoinNO.
func (s *Session) guildAnswerApplicant(accept bool, charID uint32) {
	guildMu.Lock()
	defer guildMu.Unlock()
	g := s.player.guild.guild
	if g == nil || g.members[s.char.ID] == nil || g.members[s.char.ID].Rank > guildRankAdmin {
		return
	}
	a := g.applicants[charID]
	if a == nil {
		return
	}
	delete(g.applicants, charID)
	p := FindPlayerByID(charID)
	if !accept {
		delete(guildOfChar, charID)
		_ = db.DeleteGuildChar(g.ID, charID)
		if p != nil {
			p.guild.applied = nil
			p.Send(buildGuildJoined(0, g, guildRankMember), "0x0D08 guild application declined")
		}
		g.sendAdmin(guildID32(packet.GuildApplicantRemoved, charID), "0x0D18 guild applicant removed")
		return
	}
	a.Rank, a.player = guildRankMember, p
	g.members[charID] = a
	if err := db.SaveGuildCharRank(charID, a.Rank); err != nil {
		s.log.Printf("Rapture World guild accept failed: %v", err)
	}
	g.sendAdmin(guildID32(packet.GuildApplicantRemoved, charID), "0x0D18 guild applicant removed")
	g.send(buildGuildMemberAdded(a), "0x0D14 guild member added", nil)
	if p != nil {
		p.guild.guild, p.guild.applied = g, nil
		p.guild.rejoinUntil = time.Now().Add(guildRejoinTime)
		p.Send(buildGuildJoined(1, g, a.Rank), "0x0D08 guild application accepted")
		for _, f := range g.memberListPackets() {
			p.Send(f, "0x0D12 guild members")
		}
	}
}

// guildLeave (0x0D09): CGuild::JoinLeave; the master leaves only alone,
// and an emptied guild is disbanded.
func (s *Session) guildLeave() {
	me := s.player
	guildMu.Lock()
	defer guildMu.Unlock()
	g := me.guild.guild
	if g == nil || me.guild.create != nil {
		return
	}
	m := g.members[s.char.ID]
	if m == nil || (m.Rank == guildRankMaster && len(g.members) != 1) {
		s.send(guildResult(packet.GuildLeave, 0), "0x0D09 guild leave refused")
		return
	}
	delete(g.members, s.char.ID)
	delete(guildOfChar, s.char.ID)
	me.guild.guild = nil
	_ = db.DeleteGuildChar(g.ID, s.char.ID)
	s.send(guildResult(packet.GuildLeave, 1), "0x0D09 guild left")
	g.send(guildStatus(guildStatusLeft, s.char.ID), "0x0D0C guild member left", nil)
	if len(g.members) == 0 {
		g.fire(s.logger)
	}
}

// guildExpel (0x0D0A): CGuild::JoinExpel, only lower ranks.
func (s *Session) guildExpel(charID uint32) {
	guildMu.Lock()
	defer guildMu.Unlock()
	g := s.player.guild.guild
	if g == nil || s.player.guild.create != nil {
		return
	}
	me := g.members[s.char.ID]
	t := g.members[charID]
	if me == nil || me.Rank > guildRankAdmin {
		return
	}
	if t == nil || t.Rank <= me.Rank {
		s.send(guildResultID(packet.GuildExpel, 0, charID), "0x0D0A guild expel refused")
		return
	}
	delete(g.members, charID)
	delete(guildOfChar, charID)
	_ = db.DeleteGuildChar(g.ID, charID)
	if t.player != nil {
		t.player.Send(guildResultID(packet.GuildExpel, 1, charID), "0x0D0A guild expelled")
		t.player.guild.guild = nil
	}
	s.send(guildResultID(packet.GuildExpel, 1, charID), "0x0D0A guild expel done")
	g.send(guildStatus(guildStatusExpel, charID), "0x0D0C guild member expelled", nil)
}

// guildChangeRank (0x0D0C): direction (0 promote, 1 demote), char id;
// CGuild::ChangeGuildUserLevel.
func (s *Session) guildChangeRank(direction uint8, charID uint32) {
	if charID == s.char.ID || direction > 1 {
		return
	}
	guildMu.Lock()
	defer guildMu.Unlock()
	g := s.player.guild.guild
	if g == nil {
		return
	}
	me, t := g.members[s.char.ID], g.members[charID]
	if me == nil || me.Rank > guildRankAdmin || t == nil || t.Rank <= me.Rank {
		return
	}
	next := int(t.Rank) + 1
	if direction == 0 {
		next = int(t.Rank) - 1
	}
	if next < guildRankAdmin || next > guildRankMember {
		return
	}
	if next <= guildRankAdmin && t.Rank > guildRankAdmin && g.admins() >= guildMaxAdmins {
		return
	}
	if me.Rank == guildRankAdmin && next == guildRankAdmin {
		return
	}
	t.Rank = uint8(next)
	if err := db.SaveGuildCharRank(charID, t.Rank); err != nil {
		s.log.Printf("Rapture World guild rank save failed: %v", err)
	}
	g.send(guildStatus(guildStatusRank+t.Rank, charID), "0x0D0C guild rank", nil)
}

// guildRemark (0x0D20): remark[65]; master only, no quote characters.
func (s *Session) guildRemark(p []byte) {
	remark := fixedText(p, 2, 64)
	reply := func(result uint8) {
		w := packet.New(packet.GuildRemark)
		w.U8(result)
		w.FixedString(remark, 65)
		s.send(w.MustFinish(), "0x0D20 guild remark")
	}
	guildMu.Lock()
	defer guildMu.Unlock()
	g := s.player.guild.guild
	if g == nil {
		return
	}
	if m := g.members[s.char.ID]; m == nil || m.Rank != guildRankMaster || strings.ContainsRune(remark, '\'') {
		reply(0)
		return
	}
	g.Remark = remark
	w := packet.New(packet.GuildRemark)
	w.U8(1)
	w.FixedString(remark, 65)
	g.send(w.MustFinish(), "0x0D20 guild remark", nil)
	if err := db.SaveGuildRemark(g.ID, remark); err != nil {
		s.log.Printf("Rapture World guild remark save failed: %v", err)
	}
}
