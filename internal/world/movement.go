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
}

func (s *Session) syncPlayer() {
	if s.player != nil && s.char != nil {
		s.player.UpdateCharacter(*s.char)
	}
}

// introduceToOthers (re)announces this character to nearby viewers.
func (s *Session) introduceToOthers(label string) {
	if s.player == nil {
		return
	}
	ch := s.char
	others := PlayersNearExcluding(ch.MapID, ch.ID, ch.PosX, ch.PosZ, ViewportRadius)
	if len(others) == 0 {
		return
	}
	frames := [][]byte{s.player.EnteredMapPacket(), s.player.ShapePacket(), s.player.MovePacket()}
	if s.player.IsSitting() {
		frames = append(frames, s.player.SitMotionPacket())
	}
	for _, o := range others {
		if !canSeeCharacter(o.IsAdmin, s.isAdmin, s.player.GMVisible()) {
			continue
		}
		for _, f := range frames {
			o.Send(f, label+" introduce")
		}
	}
}

// applyTeleport is the single path for every teleport (portals, town,
// resurrection): moves the character, persists it, acks the client,
// introduces it to viewers at the destination and refreshes visibility.
func (s *Session) applyTeleport(mapID uint16, x, y, z float32, label string) {
	ch := s.char
	ch.MapID, ch.PosX, ch.PosY, ch.PosZ = mapID, x, y, z
	s.visibility = EntityVisibility{}
	if s.player != nil {
		s.player.SetMap(mapID)
		s.player.UpdateCharacter(*ch)
		s.player.UpdateMotion(0, 0)
	}
	s.savePosition()
	s.send(buildCharacterMapPosition(ch), "0x020B "+label)
	s.send(buildCharacterMove(ch, 0, 0), "0x0501 "+label+" move")
	s.introduceToOthers(label)
	for _, p := range VisibilityDelta(ch.MapID, ch.PosX, ch.PosZ, &s.visibility) {
		s.send(p.Packet, p.Label)
	}
}

// resurrect is CUser::KillSoul: the death's exp loss (ExpDrop), then
// rebirth at the bind point with half HP/MP/SP, using the captured
// sequence 0x0406, teleport, 0x0551, 0x0505.
func (s *Session) resurrect() bool {
	ch := s.char
	if ch == nil {
		return false
	}
	// Map.ini RebirthMapPos(1/2) when the map defines one (dungeons and
	// some fields), otherwise the faction's nearest svmap spawn area.
	mapID, x, y, z := ch.MapID, ch.PosX, ch.PosY, ch.PosZ
	if p, ok := rebirthPosition(ch.MapID, s.faction); ok {
		mapID, x, y, z = p.MapID, p.X, p.Y, p.Z
	} else if area, ok := FindNearestSpawnArea(ch.MapID, s.faction, ch.PosX, ch.PosZ); ok {
		x, y, z = area.Center()
	}
	lost := expDrop(ch, deathExpPercent)
	ch.CurrentHP, ch.CurrentMP, ch.CurrentSP = ch.MaxHP, ch.MaxMP, ch.MaxSP
	full := StatsFor(ch, s.equipment())
	ch.CurrentHP = satSub(ch.CurrentHP, full.CurrentHP-full.MaxHP/2)
	ch.CurrentMP = satSub(ch.CurrentMP, full.CurrentMP-full.MaxMP/2)
	ch.CurrentSP = satSub(ch.CurrentSP, full.CurrentSP-full.MaxSP/2)
	s.persist()
	stats := StatsFor(ch, s.equipment())
	s.send(buildCharacterReviveInit(ch.ID), "0x0406 revive init")
	s.applyTeleport(mapID, x, y, z, "resurrect")
	s.send(buildCharacterResurrect(ch.ID, x, y, z), "0x0551 resurrect reposition")
	s.send(buildCharacterReviveHitpoints(ch.ID, stats.CurrentHP, stats.CurrentMP, stats.CurrentSP), "0x0505 revive hitpoints")
	if s.player != nil {
		s.player.SetAlive(true)
	}
	s.log.Printf("Rapture World resurrected character_id=%d map=%d pos=(%.2f, %.2f, %.2f) exp_lost=%d", ch.ID, mapID, x, y, z, lost)
	return true
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
	if others := PlayersNearExcluding(ch.MapID, ch.ID, ch.PosX, ch.PosZ, ViewportRadius); len(others) > 0 {
		frame := buildCharacterMove(ch, motion, direction)
		visible := s.player != nil && s.player.GMVisible()
		for _, o := range others {
			if canSeeCharacter(o.IsAdmin, s.isAdmin, visible) {
				o.Send(frame, "0x0501 character move (broadcast)")
			}
		}
	}
	for _, d := range VisibilityDelta(ch.MapID, ch.PosX, ch.PosZ, &s.visibility) {
		s.send(d.Packet, d.Label)
	}
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
	for _, p := range VisibilityClear(&s.visibility) {
		s.send(p.Packet, p.Label)
	}
	ch.PosX, ch.PosY, ch.PosZ = area.Center()
	if s.player != nil {
		s.player.UpdateCharacter(*ch)
		s.player.UpdateMotion(0, 0)
	}
	s.savePosition()
	s.send(buildCharacterMapPosition(ch), "0x020B town teleport")
	s.send(buildCharacterMove(ch, 0, 0), "0x0501 town teleport move")
	for _, p := range VisibilityDelta(ch.MapID, ch.PosX, ch.PosZ, &s.visibility) {
		s.send(p.Packet, p.Label)
	}
	s.send(buildChatNormal(ch.ID, fmt.Sprintf("Returned to nearest town spawn (%.2f, %.2f, %.2f).", ch.PosX, ch.PosY, ch.PosZ)),
		"0x1101 town command result")
}

// deathExpPercent is CUser::Death's drop: 5% (amulet benefits that lower
// it to 1% or 0% are not modeled yet).
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
