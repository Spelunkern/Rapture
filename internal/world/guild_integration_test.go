//go:build integration

package world

import (
	"io"
	"net"
	"os"
	"testing"
	"time"

	"github.com/Spelunkern/Rapture/internal/config"
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
	"github.com/Spelunkern/Rapture/internal/packet"
)

// TestGuildLifecycle runs the CUser::PacketGuild flows with seven
// synthetic users: creation by a party of 7 (all accept, 1000 gold from the
// master), invitation, application and acceptance, rank change, remark,
// expel, leave and disband by vote, checking the registry and the
// GuildChars rows. Its rows are removed afterwards.
func TestGuildLifecycle(t *testing.T) {
	if _, err := os.Stat("data/game"); err != nil {
		os.Chdir("../..")
	}
	config.LoadRuntimeEnv()
	if err := db.Open(db.UserData, db.GameData, db.GameDefs); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Definitions(); err != nil {
		t.Fatal(err)
	}
	logger, err := logx.New("rapture_world_test")
	if err != nil {
		t.Fatal(err)
	}
	if err := LoadGuilds(logger); err != nil {
		t.Fatal(err)
	}
	const mapID = 60030
	session := func(id uint32, name string) *Session {
		a, b := net.Pipe()
		go io.Copy(io.Discard, b)
		ch := &db.Character{ID: id, Name: name, Level: 40, Mode: growNormal, Class: 1, Money: 5000, MapID: mapID, PosX: 100, PosZ: 100}
		s := &Session{char: ch, log: logger.Tagged(name), logger: logger, faction: FactionLight,
			skillCooldowns: map[uint16]time.Time{}, out: &connWriter{conn: a}}
		s.account.UserID = "guildtest"
		s.player = RegisterPlayer(*ch, nil, false, FactionLight, s.out, logger)
		t.Cleanup(s.player.Unregister)
		return s
	}
	var ss []*Session
	pt := &party{id: 990001, itemDivision: 1}
	for i := range 7 {
		s := session(uint32(990101+i), "GTest"+string(rune('A'+i)))
		ss = append(ss, s)
		pt.members = append(pt.members, s.player)
	}
	partyMu.Lock()
	for _, s := range ss {
		s.player.party = pt
	}
	partyMu.Unlock()
	outsider := session(990120, "GTestOut")
	applicant := session(990121, "GTestApp")
	var created uint32
	defer func() {
		pool, _ := db.Pool(db.GameData)
		if created != 0 {
			pool.Exec(`DELETE FROM dbo.GuildChars WHERE GuildID = @p1; DELETE FROM dbo.GuildDetails WHERE GuildID = @p1;
				DELETE FROM dbo.Guilds WHERE GuildID = @p1;
				DELETE FROM PS_UserData.dbo.CreatedGuilds WHERE ServerID = 1 AND GuildID = @p1`, int32(created))
		}
		pool.Exec(`DELETE FROM dbo.GuildChars WHERE CharID BETWEEN 990100 AND 990199`)
	}()
	guildOf := func(s *Session) *guild {
		guildMu.Lock()
		defer guildMu.Unlock()
		return s.player.guild.guild
	}
	rows := func(id uint32) (members, applicants int) {
		pool, _ := db.Pool(db.GameData)
		pool.QueryRow(`SELECT COUNT(CASE WHEN GuildLevel > 0 THEN 1 END), COUNT(CASE WHEN GuildLevel = 0 THEN 1 END)
			FROM dbo.GuildChars WHERE GuildID = @p1 AND Del = 0`, int32(id)).Scan(&members, &applicants)
		return
	}

	name := make([]byte, 25)
	copy(name, "RaptureTestGuild")
	req := append([]byte{0x21, 0x0d}, name...)
	req = append(req, make([]byte, 65)...)
	copy(req[0x1b:], "hello")
	ss[0].handleGuild(packet.GuildCreate, req)
	for i, s := range ss {
		if guildOf(s) != nil {
			t.Fatalf("guild created before member %d accepted", i)
		}
		s.handleGuild(packet.GuildCreateAgree, []byte{0x22, 0x0d, 1})
	}
	g := guildOf(ss[0])
	if g == nil || g.Name != "RaptureTestGuild" || g.Remark != "hello" || len(g.members) != 7 || g.members[ss[0].char.ID].Rank != guildRankMaster {
		t.Fatalf("guild not created: %+v", g)
	}
	created = g.ID
	ss[0].takeGuildFee()
	if ss[0].char.Money != 4000 {
		t.Fatalf("master money %d, want 4000", ss[0].char.Money)
	}
	if m, _ := rows(g.ID); m != 7 {
		t.Fatalf("GuildChars members %d, want 7", m)
	}

	// Invitation by the master, accepted.
	ss[0].guildInvite(outsider.char.ID)
	outsider.guildInviteAnswer(true)
	if guildOf(outsider) != g || g.members[outsider.char.ID].Rank != guildRankMember {
		t.Fatal("invited user did not join")
	}
	// Application, accepted by the master.
	applicant.guildApply(g.ID)
	if _, a := rows(g.ID); a != 1 || g.applicants[applicant.char.ID] == nil {
		t.Fatal("application not stored")
	}
	ss[0].guildAnswerApplicant(true, applicant.char.ID)
	if guildOf(applicant) != g {
		t.Fatal("applicant not accepted")
	}
	if m, a := rows(g.ID); m != 9 || a != 0 {
		t.Fatalf("GuildChars members %d applicants %d, want 9/0", m, a)
	}
	// Rank: promote member 1 to sub-master (rank 9 -> 8 per step).
	for range 7 {
		ss[0].guildChangeRank(0, ss[1].char.ID)
	}
	if r := g.members[ss[1].char.ID].Rank; r != guildRankAdmin {
		t.Fatalf("promoted rank %d, want 2", r)
	}
	// A sub-master cannot promote to its own rank.
	ss[1].guildChangeRank(0, ss[2].char.ID)
	ss[1].guildChangeRank(0, ss[2].char.ID)
	before := g.members[ss[2].char.ID].Rank
	for range 7 {
		ss[1].guildChangeRank(0, ss[2].char.ID)
	}
	if r := g.members[ss[2].char.ID].Rank; r != 3 || before != 7 {
		t.Fatalf("sub-master promotion rank %d (before %d), want 3", r, before)
	}
	// Remark, expel, leave.
	ss[0].guildRemark(append([]byte{0x20, 0x0d}, []byte("new notice\x00")...))
	if g.Remark != "new notice" {
		t.Fatalf("remark %q", g.Remark)
	}
	ss[1].guildExpel(outsider.char.ID)
	applicant.guildLeave()
	ss[0].guildLeave() // the master cannot leave a populated guild
	if guildOf(outsider) != nil || guildOf(applicant) != nil || guildOf(ss[0]) != g {
		t.Fatal("expel/leave state wrong")
	}
	if m, _ := rows(g.ID); m != 7 {
		t.Fatalf("GuildChars members %d after expel+leave, want 7", m)
	}
	// Disband by vote: the master and two party admins.
	ss[0].guildChangeRank(0, ss[3].char.ID)
	for range 7 {
		ss[0].guildChangeRank(0, ss[3].char.ID)
	}
	ss[0].guildDisbandRequest()
	ss[0].guildDisbandAnswer(true)
	ss[1].guildDisbandAnswer(true)
	if guildOf(ss[0]) == nil {
		t.Fatal("disbanded with two acceptances")
	}
	ss[3].guildDisbandAnswer(true)
	if guildOf(ss[0]) != nil || guildOf(ss[4]) != nil {
		t.Fatal("guild not disbanded")
	}
	var del int
	pool, _ := db.Pool(db.GameData)
	pool.QueryRow(`SELECT Del FROM dbo.Guilds WHERE GuildID = @p1`, int32(g.ID)).Scan(&del)
	if del != 1 {
		t.Fatalf("Guilds.Del %d, want 1", del)
	}
}
