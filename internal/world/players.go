package world

import (
	"net"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/Spelunkern/Rapture/internal/crypto"
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
	"github.com/Spelunkern/Rapture/internal/packet"
)

const chatCooldown = 500 * time.Millisecond

// connWriter serializes every write to one client socket: the owning
// session and broadcasts from other sessions share it.
type connWriter struct {
	mu   sync.Mutex
	conn net.Conn
}

func (w *connWriter) write(b []byte) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.conn.SetWriteDeadline(time.Now().Add(5 * time.Second))
	_, err := w.conn.Write(b)
	return err
}

type pendingTeleport struct {
	MapID   uint16
	X, Y, Z float32
}

type pendingKill struct {
	MobID    uint16
	MobLevel uint16
	MobExp   uint32
}

type pendingDamage struct {
	MobGlobalID uint32
	MobID       uint16
	Attack      uint32
}

// ConnectedPlayer is a player in the world, reachable from any session for
// broadcasts. Cross-session work is queued in mailboxes and applied by the
// owning session's loop, which alone mutates its character.
type ConnectedPlayer struct {
	CharacterID   uint32
	CharacterName string
	IsAdmin       bool
	out           *connWriter
	log           *logx.Logger

	mu        sync.Mutex
	mapID     uint16
	character db.Character
	items     []db.Item
	faction   uint8
	motion    uint8
	direction uint16
	sitting   bool
	gmVisible bool

	mailMu       sync.Mutex
	teleport     *pendingTeleport
	tradeRefresh *tradeRefresh
	kills        []pendingKill
	damage       []pendingDamage

	alive atomic.Bool
}

var (
	registryMu sync.Mutex
	registry   = map[uint16]map[uint32]*ConnectedPlayer{}
)

func bucket(mapID uint16) map[uint32]*ConnectedPlayer {
	b, ok := registry[mapID]
	if !ok {
		b = map[uint32]*ConnectedPlayer{}
		registry[mapID] = b
	}
	return b
}

// RegisterPlayer adds a player to its map's registry.
func RegisterPlayer(ch db.Character, items []db.Item, isAdmin bool, faction uint8, out *connWriter, log *logx.Logger) *ConnectedPlayer {
	p := &ConnectedPlayer{CharacterID: ch.ID, CharacterName: ch.Name, IsAdmin: isAdmin, out: out, log: log,
		mapID: ch.MapID, character: ch, items: append([]db.Item(nil), items...), faction: faction}
	p.alive.Store(true)
	registryMu.Lock()
	bucket(ch.MapID)[ch.ID] = p
	registryMu.Unlock()
	return p
}

// Unregister removes the player and cancels any trade it was part of.
func (p *ConnectedPlayer) Unregister() {
	registryMu.Lock()
	delete(bucket(p.MapID()), p.CharacterID)
	registryMu.Unlock()
	tradeCancelForDisconnect(p.CharacterID, p.log)
}

func (p *ConnectedPlayer) SetMap(mapID uint16) {
	p.mu.Lock()
	old := p.mapID
	p.mapID = mapID
	p.mu.Unlock()
	if old == mapID {
		return
	}
	registryMu.Lock()
	if cur, ok := bucket(old)[p.CharacterID]; ok {
		delete(bucket(old), p.CharacterID)
		bucket(mapID)[p.CharacterID] = cur
	}
	registryMu.Unlock()
}

func (p *ConnectedPlayer) MapID() uint16 {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.mapID
}

func (p *ConnectedPlayer) PositionXZ() (float32, float32) {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.character.PosX, p.character.PosZ
}

func (p *ConnectedPlayer) UpdateCharacter(ch db.Character) {
	p.mu.Lock()
	p.character = ch
	p.mu.Unlock()
}

func (p *ConnectedPlayer) Snapshot() db.Character {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.character
}

func (p *ConnectedPlayer) UpdateItems(items []db.Item) {
	p.mu.Lock()
	p.items = append([]db.Item(nil), items...)
	p.mu.Unlock()
}

func (p *ConnectedPlayer) UpdateMotion(motion uint8, direction uint16) {
	p.mu.Lock()
	p.motion, p.direction = motion, direction
	p.mu.Unlock()
}

func (p *ConnectedPlayer) SetSitting(v bool) {
	p.mu.Lock()
	p.sitting = v
	p.mu.Unlock()
}

func (p *ConnectedPlayer) IsSitting() bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.sitting
}

func (p *ConnectedPlayer) GMVisible() bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.gmVisible
}

func (p *ConnectedPlayer) IsAlive() bool   { return p.alive.Load() }
func (p *ConnectedPlayer) SetAlive(v bool) { p.alive.Store(v) }

func (p *ConnectedPlayer) QueueTeleport(mapID uint16, x, y, z float32) {
	p.mailMu.Lock()
	p.teleport = &pendingTeleport{mapID, x, y, z}
	p.mailMu.Unlock()
}

func (p *ConnectedPlayer) TakeTeleport() *pendingTeleport {
	p.mailMu.Lock()
	defer p.mailMu.Unlock()
	t := p.teleport
	p.teleport = nil
	return t
}

// tradeRefresh tells the other side of a completed trade to reload its
// items and apply the money change itself (only the owner mutates its
// character).
type tradeRefresh struct {
	Packets    [][]byte
	MoneyDelta int64
}

func (p *ConnectedPlayer) QueueTradeRefresh(packets [][]byte, moneyDelta int64) {
	p.mailMu.Lock()
	p.tradeRefresh = &tradeRefresh{packets, moneyDelta}
	p.mailMu.Unlock()
}

func (p *ConnectedPlayer) TakeTradeRefresh() *tradeRefresh {
	p.mailMu.Lock()
	defer p.mailMu.Unlock()
	r := p.tradeRefresh
	p.tradeRefresh = nil
	return r
}

func (p *ConnectedPlayer) QueueMobKill(mobID, level uint16, exp uint32) {
	p.mailMu.Lock()
	p.kills = append(p.kills, pendingKill{mobID, level, exp})
	p.mailMu.Unlock()
}

func (p *ConnectedPlayer) TakeMobKills() []pendingKill {
	p.mailMu.Lock()
	defer p.mailMu.Unlock()
	k := p.kills
	p.kills = nil
	return k
}

func (p *ConnectedPlayer) QueueDamage(mobGlobalID uint32, mobID uint16, attack uint32) {
	p.mailMu.Lock()
	p.damage = append(p.damage, pendingDamage{mobGlobalID, mobID, attack})
	p.mailMu.Unlock()
}

func (p *ConnectedPlayer) TakeDamage() []pendingDamage {
	p.mailMu.Lock()
	defer p.mailMu.Unlock()
	d := p.damage
	p.damage = nil
	return d
}

func (p *ConnectedPlayer) SitMotionPacket() []byte {
	return buildCharacterMotion(p.CharacterID, MotionSitDown)
}

// EnteredMapPacket introduces an already-present character to a viewer.
func (p *ConnectedPlayer) EnteredMapPacket() []byte {
	p.mu.Lock()
	defer p.mu.Unlock()
	return buildCharacterEnteredMap(&p.character, p.IsAdmin, false)
}

// ShapePacket never embeds sitting; sit is sent as a separate 0x0506.
func (p *ConnectedPlayer) ShapePacket() []byte {
	p.mu.Lock()
	defer p.mu.Unlock()
	return buildCharacterShape(&p.character, p.faction, p.items, 0)
}

func (p *ConnectedPlayer) MovePacket() []byte {
	p.mu.Lock()
	defer p.mu.Unlock()
	return buildCharacterMove(&p.character, p.motion, p.direction)
}

func (p *ConnectedPlayer) EquipmentPacket(slot uint8) []byte {
	p.mu.Lock()
	defer p.mu.Unlock()
	return buildCharacterEquipment(&p.character, p.items, slot)
}

// Send writes a frame to this player with the post-handshake expanded XOR
// encoding, which is stateless and safe from any goroutine.
func (p *ConnectedPlayer) Send(frame []byte, label string) error {
	out := frame
	if len(frame) > 2 {
		out = append(append([]byte{}, frame[:2]...), crypto.ExpandedXOR(frame[2:])...)
	}
	return p.out.write(out)
}

func playersWhere(mapID uint16, keep func(*ConnectedPlayer) bool) []*ConnectedPlayer {
	registryMu.Lock()
	candidates := make([]*ConnectedPlayer, 0, len(registry[mapID]))
	for _, p := range registry[mapID] {
		candidates = append(candidates, p)
	}
	registryMu.Unlock()
	out := candidates[:0]
	for _, p := range candidates {
		if keep(p) {
			out = append(out, p)
		}
	}
	return out
}

func PlayersOnMap(mapID uint16) []*ConnectedPlayer {
	return playersWhere(mapID, func(*ConnectedPlayer) bool { return true })
}

func PlayersNear(mapID uint16, x, z, radius float32) []*ConnectedPlayer {
	return playersWhere(mapID, func(p *ConnectedPlayer) bool {
		px, pz := p.PositionXZ()
		return DistanceXZ(px, pz, x, z) <= radius
	})
}

func PlayersNearExcluding(mapID uint16, characterID uint32, x, z, radius float32) []*ConnectedPlayer {
	return playersWhere(mapID, func(p *ConnectedPlayer) bool {
		if p.CharacterID == characterID {
			return false
		}
		px, pz := p.PositionXZ()
		return DistanceXZ(px, pz, x, z) <= radius
	})
}

func AllPlayers() []*ConnectedPlayer {
	registryMu.Lock()
	defer registryMu.Unlock()
	var out []*ConnectedPlayer
	for _, b := range registry {
		for _, p := range b {
			out = append(out, p)
		}
	}
	return out
}

func FindPlayerByName(name string) *ConnectedPlayer {
	for _, p := range AllPlayers() {
		if strings.EqualFold(p.CharacterName, name) {
			return p
		}
	}
	return nil
}

func FindPlayerByID(id uint32) *ConnectedPlayer {
	for _, p := range AllPlayers() {
		if p.CharacterID == id {
			return p
		}
	}
	return nil
}

// canSeeCharacter: everyone is visible. The GM invisibility toggle was a
// Rapture GM command and is gone with them; the hook stays for the
// original admin-visibility rules if they come back.
func canSeeCharacter(viewerIsAdmin, subjectIsAdmin, subjectGMVisible bool) bool {
	return true
}

// broadcast sends frame to each player, logging failures.
func broadcast(players []*ConnectedPlayer, frame []byte, label string, log *logx.Logger) {
	for _, p := range players {
		if err := p.Send(frame, label); err != nil && log != nil {
			log.Printf("Rapture World failed to deliver %s to character_id=%d: %v", label, p.CharacterID, err)
		}
	}
}

// handlePublicChat delivers normal/map chat to the sender's map and world
// chat to everyone. Admin channels are admin-only and relay as public chat.
func (s *Session) handlePublicChat(op packet.Opcode, message string) {
	adminChannel := op == packet.ChatNormalAdmin || op == packet.ChatWorldAdmin
	if adminChannel && !s.isAdmin {
		s.log.Printf("Rapture World rejected admin chat from non-admin account_id=%d", s.accountID)
		s.send(buildChatError(1), "0x1106 chat error (not admin)")
		return
	}
	if strings.TrimSpace(message) == "" || len(message) > maxChatMessageLen ||
		(!s.lastChatAt.IsZero() && time.Since(s.lastChatAt) < chatCooldown) {
		s.log.Printf("Rapture World rejected chat (empty, too long or cooldown)")
		s.send(buildChatError(1), "0x1106 chat error")
		return
	}
	s.lastChatAt = time.Now()
	ch := s.char
	var frame []byte
	var recipients []*ConnectedPlayer
	switch op {
	case packet.ChatNormal, packet.ChatNormalAdmin:
		frame, recipients = buildChatByID(packet.ChatNormal, ch.ID, message), PlayersOnMap(ch.MapID)
	case packet.ChatWorld, packet.ChatWorldAdmin:
		frame, recipients = buildChatByName(packet.ChatWorld, ch.Name, message), AllPlayers()
	default:
		frame, recipients = buildChatByName(packet.ChatMap, ch.Name, message), PlayersOnMap(ch.MapID)
	}
	broadcast(recipients, frame, "chat", s.logger)
	s.log.Printf("Rapture World delivered chat opcode=0x%04X to %d recipient(s)", uint16(op), len(recipients))
}
