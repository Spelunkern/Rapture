// Command rapture_login is the Shaiya 5.4 login server: RSA/AES key
// exchange, credential check through the original
// PS_UserData.dbo.usp_Try_GameLogin_Taiwan, server list and the session
// handoff to rapture_world.
package main

import (
	"fmt"
	"net"
	"os"
	"sync/atomic"

	"github.com/Spelunkern/Rapture/internal/config"
	"github.com/Spelunkern/Rapture/internal/crypto"
	"github.com/Spelunkern/Rapture/internal/db"
	"github.com/Spelunkern/Rapture/internal/logx"
	"github.com/Spelunkern/Rapture/internal/netx"
	"github.com/Spelunkern/Rapture/internal/packet"
	"github.com/Spelunkern/Rapture/internal/session"
)

const maxConnections = 200

var nextConnection atomic.Int64

func main() {
	config.LoadRuntimeEnv()
	logger, err := logx.New("rapture_login")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	logger.Printf("Starting Rapture Login")
	if err := db.Open(db.UserData); err != nil {
		logger.Printf("Rapture Login database error: %v", err)
		os.Exit(1)
	}
	listener, err := net.Listen("tcp", config.LoginListen())
	if err != nil {
		logger.Printf("Rapture Login failed to bind %s: %v", config.LoginListen(), err)
		os.Exit(1)
	}
	logger.Printf("Rapture Login listening on %s", config.LoginListen())
	limiter := netx.NewLimiter(maxConnections)
	for {
		conn, err := listener.Accept()
		if err != nil {
			logger.Printf("Rapture Login accept error: %v", err)
			continue
		}
		if !limiter.Acquire() {
			logger.Printf("Rapture Login rejected connection: at capacity (%d)", maxConnections)
			conn.Close()
			continue
		}
		netx.Tune(conn)
		go func() {
			defer limiter.Release()
			defer conn.Close()
			log := logger.Tagged(fmt.Sprintf("conn%d", nextConnection.Add(1)))
			if err := handle(conn, log); err != nil {
				log.Printf("Rapture Login client error: %v", err)
			}
		}()
	}
}

func handshake() []byte {
	exponent, modulus := crypto.LoginPublicKey()
	w := packet.New(packet.LoginHandshake)
	w.U8(0)
	w.U8(uint8(len(exponent)))
	w.U8(uint8(len(modulus)))
	w.Bytes(exponent[:])
	w.Bytes(modulus[:])
	return w.MustFinish()
}

func loginReply(result uint8, userUID uint32, permission uint8, identity [16]byte) []byte {
	w := packet.New(packet.LoginRequest)
	w.U8(result)
	if result == 0 {
		w.U32(userUID)
		w.U8(permission)
		w.Bytes(identity[:])
	}
	return w.MustFinish()
}

func serverList() []byte {
	w := packet.New(packet.ServerList)
	w.U8(1)    // server count
	w.U8(1)    // server id
	w.U8(0)    // status
	w.U16(0)   // users
	w.U16(100) // capacity
	w.FixedString("Rapture", 32)
	return w.MustFinish()
}

func selectServerReply() []byte {
	w := packet.New(packet.SelectServer)
	w.U8(0)
	ip := net.ParseIP(config.PublicIP()).To4()
	if ip == nil {
		ip = net.IPv4(127, 0, 0, 1).To4()
	}
	w.Bytes(ip)
	return w.MustFinish()
}

type client struct {
	conn      net.Conn
	log       *logx.Tag
	send      *crypto.AESCTR
	recv      *crypto.AESCTR
	material  [32]byte
	userUID   uint32
	sessionID int64
}

func (c *client) write(frame []byte, label string) error {
	out := frame
	if c.send != nil && len(frame) > 2 {
		out = append(append([]byte{}, frame[:2]...), c.send.Transform(frame[2:])...)
	}
	c.log.Printf("Rapture Login sending %s (%d bytes)", label, len(frame))
	_, err := c.conn.Write(out)
	return err
}

func handle(conn net.Conn, log *logx.Tag) error {
	c := &client{conn: conn, log: log}
	remote, _, _ := net.SplitHostPort(conn.RemoteAddr().String())
	log.Printf("Rapture Login client connected from %s", remote)
	if err := c.write(handshake(), "0xA101 handshake"); err != nil {
		return err
	}
	var frames netx.Frames
	buf := make([]byte, 4096)
	for {
		n, err := conn.Read(buf)
		if n == 0 || err != nil {
			log.Printf("Rapture Login client disconnected")
			return nil
		}
		list, ok := frames.Push(buf[:n])
		for _, frame := range list {
			if err := c.frame(frame, remote); err != nil {
				return err
			}
		}
		if !ok {
			return fmt.Errorf("malformed frame")
		}
	}
}

func (c *client) frame(frame []byte, remote string) error {
	if c.recv == nil {
		opcode, _ := packet.U16(frame[2:4])
		if packet.Opcode(opcode) != packet.LoginHandshake || len(frame) < 5 {
			return nil
		}
		size := int(frame[4])
		if size == 0 || len(frame) < 5+size {
			return fmt.Errorf("short RSA block")
		}
		// The RSA plaintext and derived AES material are never logged.
		c.material = crypto.DeriveLoginAESMaterial(crypto.DecryptLoginRSA(frame[5 : 5+size]))
		var key, iv [16]byte
		copy(key[:], c.material[:16])
		copy(iv[:], c.material[16:])
		c.recv = crypto.NewAESCTR(key, iv)
		c.send = crypto.NewAESCTR(key, iv)
		c.log.Printf("Rapture Login key exchange complete")
		return nil
	}
	payload := c.recv.Transform(frame[2:])
	if len(payload) < 2 {
		return nil
	}
	opcode, _ := packet.U16(payload)
	switch packet.Opcode(opcode) {
	case packet.LoginRequest:
		if len(payload) < 50 {
			return nil
		}
		username := packet.FixedASCII(payload[2:21])
		password := packet.FixedASCII(payload[34:50]) // never logged
		c.log.Printf("Rapture Login request username=%q", username)
		c.sessionID = session.NewSessionID()
		login, err := db.TryGameLogin(username, password, c.sessionID, remote)
		if err != nil {
			c.log.Printf("Rapture Login database error: %v", err)
			return c.write(loginReply(1, 0, 11, [16]byte{}), "0xA102 login failed")
		}
		result, permission := db.LoginDecision(login)
		if result != 0 {
			c.log.Printf("Rapture Login rejected username=%q status=%d result=%d", username, login.Status, result)
			return c.write(loginReply(result, 0, permission, [16]byte{}), "0xA102 login failed")
		}
		c.userUID = uint32(login.UserUID)
		c.log.SetTag(c.log.Tag() + "/" + username)
		identity := session.NewIdentity()
		if err := session.Save(session.Handoff{Identity: identity, AESMaterial: c.material,
			UserUID: c.userUID, SessionID: c.sessionID}); err != nil {
			return err
		}
		c.log.Printf("Rapture Login authenticated user_uid=%d permission=%d", c.userUID, permission)
		if err := c.write(loginReply(0, c.userUID, permission, identity), "0xA102 login success"); err != nil {
			return err
		}
		return c.write(serverList(), "0xA201 server list")
	case packet.SelectServer:
		if len(payload) < 7 || c.userUID == 0 {
			return nil
		}
		version, _ := packet.U32(payload[3:7])
		c.log.Printf("Rapture Login select server world_id=%d client_version=%d", payload[2], version)
		return c.write(selectServerReply(), "0xA202 select server")
	}
	return nil
}
