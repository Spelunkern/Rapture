// Package netx holds TCP helpers shared by the login and world servers.
package netx

import (
	"net"
	"sync/atomic"
	"time"
)

// Tune enables TCP keepalive (idle clients are never reaped; vanished peers
// are) and disables Nagle so small packets such as skill acks go out
// immediately.
func Tune(conn net.Conn) {
	tcp, ok := conn.(*net.TCPConn)
	if !ok {
		return
	}
	tcp.SetNoDelay(true)
	tcp.SetKeepAliveConfig(net.KeepAliveConfig{Enable: true, Idle: 10 * time.Second, Interval: 5 * time.Second, Count: 3})
}

// Limiter caps simultaneous connections.
type Limiter struct {
	max    int64
	active atomic.Int64
}

func NewLimiter(max int64) *Limiter { return &Limiter{max: max} }

func (l *Limiter) Acquire() bool {
	for {
		current := l.active.Load()
		if current >= l.max {
			return false
		}
		if l.active.CompareAndSwap(current, current+1) {
			return true
		}
	}
}

func (l *Limiter) Release()      { l.active.Add(-1) }
func (l *Limiter) Active() int64 { return l.active.Load() }

// Frames splits a byte stream into length-prefixed Shaiya frames
// (length:u16 counts the whole frame).
type Frames struct{ buf []byte }

// Push appends received bytes and returns every complete frame. A frame
// length below 4 is a protocol error.
func (f *Frames) Push(data []byte) ([][]byte, bool) {
	f.buf = append(f.buf, data...)
	var frames [][]byte
	for len(f.buf) >= 2 {
		length := int(f.buf[0]) | int(f.buf[1])<<8
		if length < 4 {
			return frames, false
		}
		if len(f.buf) < length {
			break
		}
		frame := make([]byte, length)
		copy(frame, f.buf[:length])
		frames = append(frames, frame)
		f.buf = f.buf[length:]
	}
	if len(f.buf) == 0 {
		f.buf = nil
	}
	return frames, true
}
