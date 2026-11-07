package world

import (
	"fmt"
	"math"
	"strings"
	"time"

	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/packet"
)

const positionSaveInterval = 5 * time.Second

func (s *Session) usablePortal(x, y, z float32, index *uint8) (MapPortal, bool) {
	ch := s.char
	var portal MapPortal
	var ok bool
	if index != nil {
		portal, ok = FindPortalByIndex(ch.MapID, *index)
	} else {
		portal, ok = FindPortalAt(ch.MapID, x, y, z)
	}
	if !ok {
		return MapPortal{}, false
	}
	switch {
	case !portal.IsOpen():
		s.log.Printf("Rapture World rejected closed portal index=%d map=%d", portal.Index, ch.MapID)
	case !portal.AllowsFaction(s.faction):
		s.log.Printf("Rapture World rejected portal faction index=%d map=%d faction=%d", portal.Index, ch.MapID, s.faction)
	case !portal.AllowsLevel(ch.Level):
		s.log.Printf("Rapture World rejected portal level index=%d map=%d level=%d", portal.Index, ch.MapID, ch.Level)
	default:
		return portal, true
	}
	return MapPortal{}, false
}

// savePosition persists the character (usp_Save_Char_Info_E).
func (s *Session) savePosition() {
	if s.char == nil {
		return
	}
	if err := db.SaveCharacter(s.char); err != nil {
		s.log.Printf("Rapture World failed to save character_id=%d: %v", s.char.ID, err)
	}
	s.positionDirty = false
	s.lastPositionSave = time.Now()
	s.saveWornItems()
}

func (s *Session) syncPlayer() {
	if s.player != nil && s.char != nil {
		s.player.UpdateCharacter(*s.char)
	}
}

// applyTeleport is the single path for every teleport (portals, town,
// resurrection): moves the character, persists it, acks the client,
// introduces it to viewers at the destination and refreshes visibility.
func (s *Session) applyTeleport(mapID uint16, x, y, z float32, label string) {
	ch := s.char
	// CWorld::GetZoneByMapID: a guild ranking map resolves to the guild's
	// instance while the battle is open; without one the move fails.
	if !isInstanceZone(mapID) && mapCreateType(mapID) == grbCreateType {
		zone, ok := s.enterGuildRanking(mapID)
		if !ok {
			s.log.Printf("Rapture World guild ranking map %d refused (closed or no guild)", mapID)
			return
		}
		mapID = zone
	}
	// LeaveUser / EnterUser: the old view goes (on the same map the client
	// keeps its entities, so they get leave packets), then the new one.
	s.clearView(mapID == ch.MapID)
	ch.MapID, ch.PosX, ch.PosY, ch.PosZ = mapID, x, y, z
	if s.player != nil {
		s.player.SetMap(mapID)
		s.player.UpdateCharacter(*ch)
		s.player.UpdateMotion(0, 0)
	}
	s.savePosition()
	s.send(buildCharacterMapPosition(ch), "0x020B "+label)
	s.send(buildCharacterMove(ch, 0, 0), "0x0501 "+label+" move")
	s.placeView()
}

// handleCharacterMove (0x0501): motion:u8, direction:u16, x, y, z:f32.
func (s *Session) handleCharacterMove(p []byte) {
	if len(p) < 17 || s.char == nil {
		return
	}
	motion := p[2]
	direction, _ := packet.U16(p[3:])
	x, _ := packet.F32(p[5:])
	y, _ := packet.F32(p[9:])
	z, _ := packet.F32(p[13:])
	ch := s.char
	if x != ch.PosX || z != ch.PosZ {
		s.cancelMove()
	}
	ch.PosX, ch.PosY, ch.PosZ = x, y, z
	s.positionDirty = true
	if s.player != nil {
		s.player.UpdateCharacter(*ch)
		s.player.UpdateMotion(motion, direction)
	}
	if portal, ok := s.usablePortal(x, y, z, nil); ok {
		s.applyTeleport(portal.TargetMapID, portal.TargetX, portal.TargetY, portal.TargetZ, "portal teleport")
		return
	}
	// CZone::MoveUser: a cell change updates the view first, then the move
	// goes to the users that show this one.
	if s.player != nil && s.player.moveViewCell(x, z) {
		s.refreshView()
	}
	s.sendToViewers(buildCharacterMove(ch, motion, direction), "0x0501 character move (broadcast)")
	if time.Since(s.lastPositionSave) >= positionSaveInterval {
		s.savePosition()
	}
}

func (s *Session) handleEnteredPortal(index uint8) {
	if s.char == nil {
		return
	}
	if portal, ok := s.usablePortal(s.char.PosX, s.char.PosY, s.char.PosZ, &index); ok {
		s.applyTeleport(portal.TargetMapID, portal.TargetX, portal.TargetY, portal.TargetZ, "explicit portal teleport")
	}
}

func isTownCommand(message string) bool {
	fields := strings.Fields(message)
	return len(fields) == 1 && (strings.EqualFold(fields[0], "/town") || strings.EqualFold(fields[0], "town"))
}

// executeTown moves the character to the nearest faction spawn area on
// its map (also the escape hatch while dead).
func (s *Session) executeTown() {
	ch := s.char
	area, ok := FindNearestSpawnArea(ch.MapID, s.faction, ch.PosX, ch.PosZ)
	if !ok {
		s.log.Printf("Rapture World town command found no spawn area map=%d faction=%d", ch.MapID, s.faction)
		s.send(buildChatNormal(ch.ID, "No town spawn area was found for this map/faction."), "0x1101 town command result")
		return
	}
	s.clearView(true)
	ch.PosX, ch.PosY, ch.PosZ = area.RandomPoint()
	if s.player != nil {
		s.player.UpdateCharacter(*ch)
		s.player.UpdateMotion(0, 0)
	}
	s.savePosition()
	s.send(buildCharacterMapPosition(ch), "0x020B town teleport")
	s.send(buildCharacterMove(ch, 0, 0), "0x0501 town teleport move")
	s.placeView()
	s.send(buildChatNormal(ch.ID, fmt.Sprintf("Returned to nearest town spawn (%.2f, %.2f, %.2f).", ch.PosX, ch.PosY, ch.PosZ)),
		"0x1101 town command result")
}

// deathExpPercent is CUser::Death's drop without charms (deathExpLoss).
const deathExpPercent = 5

// expDrop is CUser::ExpDrop (0x0045ad50): above level 10, percent of the
// level's exp span, never below the level's start.
func expDrop(ch *db.Character, percent int) uint32 {
	if ch.Level <= 10 || percent <= 0 {
		return 0
	}
	prev, next := expBounds(ch)
	loss := int64(math.RoundToEven(float64(float32(percent)) / 100 * float64(float32(int32(next-prev)))))
	room := int64(ch.Exp) - int64(prev)
	if room < 1 {
		return 0
	}
	loss = min(loss, room)
	ch.Exp -= uint32(loss)
	return uint32(loss)
}

// returnToTownDelay is 0x0556's tick + 10000 (CUser::PacketPC).
const returnToTownDelay = 10 * time.Second

// handleReturnToTown (0x0556): alive and with no move pending, the user
// is moved to its bind point 10 s later (UpdateResetPosition, type 0).
func (s *Session) handleReturnToTown() {
	if s.char == nil || s.dead || !s.returnAt.IsZero() {
		return
	}
	s.returnAt = time.Now().Add(returnToTownDelay)
}

// cancelMove is CUser::CancleMove for the type-0 move: moving, attacking,
// casting or being hit drops it without a reply.
func (s *Session) cancelMove() { s.returnAt = time.Time{} }

func (s *Session) tickReturnToTown() {
	if s.returnAt.IsZero() || time.Now().Before(s.returnAt) || s.char == nil {
		return
	}
	s.returnAt = time.Time{}
	if s.dead {
		return
	}
	ch := s.char
	if m, x, y, z, ok := bindPositionFound(ch.MapID, s.faction, ch.PosX, ch.PosY, ch.PosZ); ok {
		s.applyTeleport(m, x, y, z, "return to town")
	}
}
