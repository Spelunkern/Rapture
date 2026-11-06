package world

import (
	"sync"

	"github.com/Spelunkern/Rapture/internal/packet"
)

// Visibility as ps_game.exe's CZone runs it. A map is cut into 40-unit
// cells. Every user, mob and NPC belongs to one cell and keeps it while it
// stays inside that cell's 60-unit view window (cell*40-10 to cell*40+50:
// CZone::MoveUserLocal 0x00416370 / CZone::MoveMob 0x004169d0), so walking
// along a border does not flip it. A user sees the 5x5 cells around its
// own (CZone::SendCellInfoView 0x0041f290). When an entity changes cell
// the users that lose it get its leave packet and the users that gain it
// its enter packet (CZone::SendMoveUser 0x0041f350, SendMoveMob
// 0x0041f860); moves then go to the 5x5 view. Combat and other events go
// to users within 60 units of the spot (CZone::PSendView, radius
// 0x42700000 = 60.0, filtered by CCell::PSendRange).

const (
	viewMargin      = 10
	BroadcastRadius = 60.0
)

// holds reports whether (x, z) is inside the cell's view window.
func (c cell) holds(x, z float32) bool {
	lx, hx := float32(c.x*VisibilityCellSize-viewMargin), float32(c.x*VisibilityCellSize+VisibilityCellSize+viewMargin)
	lz, hz := float32(c.z*VisibilityCellSize-viewMargin), float32(c.z*VisibilityCellSize+VisibilityCellSize+viewMargin)
	return x >= lx && x < hx && z >= lz && z < hz
}

// cellAfterMove keeps the previous cell while the position stays in its
// view window, otherwise the cell under the position.
func cellAfterMove(prev cell, has bool, x, z float32) cell {
	if has && prev.holds(x, z) {
		return prev
	}
	return cellFor(x, z)
}

type viewKind uint8

const (
	viewMob viewKind = iota
	viewNpc
	viewItem
	viewUser
)

// playerView is a user's view cell and the entities its client knows.
type playerView struct {
	mu    sync.Mutex
	has   bool
	cell  cell
	known [4]map[uint32]bool
}

func (v *playerView) set(kind viewKind) map[uint32]bool {
	if v.known[kind] == nil {
		v.known[kind] = map[uint32]bool{}
	}
	return v.known[kind]
}

func (p *ConnectedPlayer) viewCell() (cell, bool) {
	p.view.mu.Lock()
	defer p.view.mu.Unlock()
	return p.view.cell, p.view.has
}

// moveViewCell applies the hysteresis; it returns whether the cell changed.
func (p *ConnectedPlayer) moveViewCell(x, z float32) bool {
	p.view.mu.Lock()
	defer p.view.mu.Unlock()
	next := cellAfterMove(p.view.cell, p.view.has, x, z)
	changed := !p.view.has || next != p.view.cell
	p.view.cell, p.view.has = next, true
	return changed
}

// resetView forgets everything (map change, teleport, leaving the world)
// and returns what was known.
func (p *ConnectedPlayer) resetView() [4]map[uint32]bool {
	p.view.mu.Lock()
	defer p.view.mu.Unlock()
	old := p.view.known
	p.view.known = [4]map[uint32]bool{}
	p.view.has = false
	return old
}

func (p *ConnectedPlayer) knows(kind viewKind, id uint32) bool {
	p.view.mu.Lock()
	defer p.view.mu.Unlock()
	return p.view.known[kind][id]
}

// know marks an entity known; true when it was not.
func (p *ConnectedPlayer) know(kind viewKind, id uint32) bool {
	p.view.mu.Lock()
	defer p.view.mu.Unlock()
	s := p.view.set(kind)
	if s[id] {
		return false
	}
	s[id] = true
	return true
}

// forget unmarks an entity; true when it was known.
func (p *ConnectedPlayer) forget(kind viewKind, id uint32) bool {
	p.view.mu.Lock()
	defer p.view.mu.Unlock()
	if !p.view.known[kind][id] {
		return false
	}
	delete(p.view.known[kind], id)
	return true
}

func (p *ConnectedPlayer) knownIDs(kind viewKind) []uint32 {
	p.view.mu.Lock()
	defer p.view.mu.Unlock()
	out := make([]uint32, 0, len(p.view.known[kind]))
	for id := range p.view.known[kind] {
		out = append(out, id)
	}
	return out
}

// viewing reports whether the player's 5x5 view covers the cell.
func (p *ConnectedPlayer) viewing(c cell) bool {
	vc, ok := p.viewCell()
	return ok && isNeighborCell(vc, c)
}

// PlayersViewing are the users whose view covers the cell.
func PlayersViewing(mapID uint16, c cell) []*ConnectedPlayer {
	return playersWhere(mapID, func(p *ConnectedPlayer) bool { return p.viewing(c) })
}

// sendView is CZone::SendView: the frame to the users viewing the cell of
// (x, z).
func sendView(mapID uint16, x, z float32, frame []byte, label string) {
	for _, p := range PlayersViewing(mapID, cellFor(x, z)) {
		p.Send(frame, label)
	}
}

// viewEnter sends an entity's enter packet to a user that did not know it.
func viewEnter(p *ConnectedPlayer, kind viewKind, id uint32, frame []byte, label string) {
	if p.know(kind, id) {
		p.Send(frame, label)
	}
}

// viewLeave sends an entity's leave packet to a user that knew it.
func viewLeave(p *ConnectedPlayer, kind viewKind, id uint32, frame []byte, label string) {
	if p.forget(kind, id) {
		p.Send(frame, label)
	}
}

// entityMove is one mob or NPC update for the users around it: a cell
// change (or an appearance with Fresh), then its move packet.
type entityMove struct {
	MapID    uint16
	Kind     viewKind
	ID       uint32
	From, To cell
	Fresh    bool // just appeared: no previous cell
	Changed  bool
	Move     []byte // 0x0603 / 0x0E03, nil for an appearance
	Enter    []byte // 0x0601 / 0x0E01 at the new position
}

func leaveFrame(kind viewKind, id uint32) []byte {
	switch kind {
	case viewMob:
		return buildMobLeave(id)
	case viewNpc:
		return buildNpcLeave(id)
	case viewItem:
		return buildMapRemoveItem(id)
	}
	return buildUserLeave(id)
}

// dispatchEntityMove is SendMoveMob / SendMoveNpc followed by the move.
func dispatchEntityMove(e entityMove) {
	viewers := PlayersViewing(e.MapID, e.To)
	if e.Changed && !e.Fresh {
		for _, p := range PlayersViewing(e.MapID, e.From) {
			if !p.viewing(e.To) {
				viewLeave(p, e.Kind, e.ID, leaveFrame(e.Kind, e.ID), "view leave (cell change)")
			}
		}
	}
	for _, p := range viewers {
		if !p.knows(e.Kind, e.ID) {
			viewEnter(p, e.Kind, e.ID, e.Enter, "view enter")
			continue
		}
		if e.Move != nil {
			p.Send(e.Move, "view move")
		}
	}
}

// dispatchEntityLeave removes an entity from every user that knows it
// (a mob's corpse disappearing, an item picked up or expired).
func dispatchEntityLeave(mapID uint16, kind viewKind, id uint32, label string) {
	frame := leaveFrame(kind, id)
	for _, p := range PlayersOnMap(mapID) {
		viewLeave(p, kind, id, frame, label)
	}
}

// dispatchItemEnter shows a new map item to the users viewing its cell.
func dispatchItemEnter(mapID uint16, it *MapItem, label string) {
	frame := buildMapAddItem(it)
	for _, p := range PlayersViewing(mapID, cellFor(it.PosX, it.PosZ)) {
		viewEnter(p, viewItem, it.ID, frame, label)
	}
}

// buildUserLeave (0x0202): the character id.
func buildUserLeave(id uint32) []byte {
	w := packet.New(0x0202)
	w.U32(id)
	return w.MustFinish()
}

// refreshView is CZone::SendMoveUser for the user's own cell change (and
// its first placement): the leave packets of what left the 5x5 view, the
// enter packets of what entered it (SendCellInfoLeave / SendCellInfoEnter),
// and the user's own enter or leave for the other users concerned.
func (s *Session) refreshView() {
	p, ch := s.player, s.char
	if p == nil || ch == nil {
		return
	}
	c, ok := p.viewCell()
	if !ok {
		return
	}
	near := EntitiesInView(ch.MapID, c)
	want := map[uint32]bool{}
	for i := range near.Mobs {
		m := &near.Mobs[i]
		want[m.ID] = true
		viewEnter(p, viewMob, m.ID, buildMobEnter(m, false), "0x0601 mob enter")
	}
	for _, id := range p.knownIDs(viewMob) {
		if !want[id] {
			viewLeave(p, viewMob, id, buildMobLeave(id), "0x0602 mob leave")
		}
	}
	want = map[uint32]bool{}
	for i := range near.NPCs {
		n := &near.NPCs[i]
		want[n.ID] = true
		viewEnter(p, viewNpc, n.ID, buildNpcEnter(n), "0x0E01 npc enter")
	}
	for _, id := range p.knownIDs(viewNpc) {
		if !want[id] {
			viewLeave(p, viewNpc, id, buildNpcLeave(id), "0x0E02 npc leave")
		}
	}
	want = map[uint32]bool{}
	for _, it := range itemsOnMap(ch.MapID) {
		if isNeighborCell(c, cellFor(it.PosX, it.PosZ)) {
			want[it.ID] = true
			viewEnter(p, viewItem, it.ID, buildMapAddItem(&it), "0x0401 map item enter")
		}
	}
	for _, id := range p.knownIDs(viewItem) {
		if !want[id] {
			viewLeave(p, viewItem, id, buildMapRemoveItem(id), "0x0402 map item leave")
		}
	}
	mine := s.userEnterFrames()
	for _, o := range PlayersOnMap(ch.MapID) {
		if o == p {
			continue
		}
		oc, ok := o.viewCell()
		if ok && isNeighborCell(c, oc) {
			if p.know(viewUser, o.CharacterID) {
				for _, f := range o.enterFrames() {
					p.Send(f, "other character enter")
				}
			}
			if o.know(viewUser, ch.ID) {
				for _, f := range mine {
					o.Send(f, "character enter (other's view)")
				}
			}
			continue
		}
		if p.forget(viewUser, o.CharacterID) {
			p.Send(buildUserLeave(o.CharacterID), "0x0202 other character leave")
		}
		if o.forget(viewUser, ch.ID) {
			o.Send(buildUserLeave(ch.ID), "0x0202 character leave (other's view)")
		}
	}
}

// enterFrames is what CCell::SendCellInfoEnter shows of a user.
func (p *ConnectedPlayer) enterFrames() [][]byte {
	frames := [][]byte{p.EnteredMapPacket(), p.ShapePacket(), p.MovePacket()}
	if shape := p.mountShape(); shape != 0 {
		w := packet.New(packet.CharacterMountShape)
		w.U32(p.CharacterID)
		w.U8(shape)
		frames = append(frames, w.MustFinish())
	}
	if p.IsSitting() {
		frames = append(frames, p.SitMotionPacket())
	}
	return frames
}

func (s *Session) userEnterFrames() [][]byte {
	if s.player == nil {
		return nil
	}
	return s.player.enterFrames()
}

// clearView forgets the user's view: every known entity gets its leave
// packet (sendLeaves) and the other users that saw it get 0x0202.
func (s *Session) clearView(sendLeaves bool) {
	p := s.player
	if p == nil {
		return
	}
	known := p.resetView()
	if sendLeaves {
		for kind, ids := range known {
			if viewKind(kind) == viewUser {
				continue
			}
			for id := range ids {
				p.Send(leaveFrame(viewKind(kind), id), "view cleared")
			}
		}
		for id := range known[viewUser] {
			p.Send(buildUserLeave(id), "0x0202 view cleared")
		}
	}
	p.dropFromOthers()
}

// dropFromOthers sends 0x0202 to every user that knows this one.
func (p *ConnectedPlayer) dropFromOthers() {
	for _, o := range PlayersOnMap(p.MapID()) {
		if o != p && o.forget(viewUser, p.CharacterID) {
			o.Send(buildUserLeave(p.CharacterID), "0x0202 character leave")
		}
	}
}

// sendToViewers is CZone::SendView for the user's own updates: the users
// that currently show it.
func (s *Session) sendToViewers(frame []byte, label string) {
	p := s.player
	if p == nil {
		return
	}
	for _, o := range PlayersOnMap(p.MapID()) {
		if o != p && o.knows(viewUser, p.CharacterID) {
			o.Send(frame, label)
		}
	}
}

// placeView puts the user at its current position and sends its view.
func (s *Session) placeView() {
	if s.player == nil || s.char == nil {
		return
	}
	s.player.moveViewCell(s.char.PosX, s.char.PosZ)
	s.refreshView()
}

func (p *ConnectedPlayer) setMountShape(shape uint8) {
	p.mu.Lock()
	p.mount = shape
	p.mu.Unlock()
}

func (p *ConnectedPlayer) mountShape() uint8 {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.mount
}
