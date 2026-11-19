package transport

import (
	"errors"
	"log"
	"net"
	"sync"
	"sync/atomic"
	"time"

	"cs2-ghost/pkg/a2s"
	"cs2-ghost/pkg/protocol"
	"cs2-ghost/pkg/shaper"
)

const sessionIdleTimeoutSec = 900 // 15 minutes

// GNSPacketConn wraps a raw UDP PacketConn to implement Valve GameNetworkingSockets
// 1-RTT X25519 PFS handshake, per-session AES-256-GCM encryption, 64-bit anti-replay window,
// AI 64-Tick traffic shaping, and active probe defense.
type GNSPacketConn struct {
	raw       net.PacketConn
	psk       string
	shaper    *shaper.StateMachine
	responder *a2s.Responder // non-nil in server mode
	isServer  bool

	// Client-side single active ephemeral session
	clientCodec *protocol.GNSCodec
	remoteAddr  net.Addr

	// Server-side multi-session tables (supports multiple concurrent devices with unique PFS keys)
	sessMu    sync.RWMutex
	byConnID  map[uint32]*protocol.GNSCodec
	byAddrStr map[string]*protocol.GNSCodec

	closed    int32
	closeCh   chan struct{}
	closeOnce sync.Once
	readBuf   []byte
	readMu    sync.Mutex

	// Telemetry counters
	A2SDeflected    uint64
	ProbesRejected  uint64
	ReplaysBlocked  uint64
	HandshakesOK    uint64
	DummyTicksSent  uint64
	DummyTicksRecvd uint64
}

func NewServerPacketConn(raw net.PacketConn, psk string, a2sCfg a2s.ServerConfig, shaperCfg shaper.Config) (*GNSPacketConn, error) {
	c := &GNSPacketConn{
		raw:       raw,
		psk:       psk,
		shaper:    shaper.NewStateMachine(shaperCfg),
		responder: a2s.NewResponder(a2sCfg),
		isServer:  true,
		byConnID:  make(map[uint32]*protocol.GNSCodec),
		byAddrStr: make(map[string]*protocol.GNSCodec),
		closeCh:   make(chan struct{}),
		readBuf:   make([]byte, 2048),
	}
	go c.sessionCleanupLoop()
	return c, nil
}

// NewClientPacketConn performs an authentic 1-RTT X25519 PFS Valve GNS handshake
// (ConnectRequest -> ConnectOK) with the remote server before returning the encrypted PacketConn.
func NewClientPacketConn(raw net.PacketConn, remoteAddr net.Addr, psk string, shaperCfg shaper.Config) (*GNSPacketConn, error) {
	reqPkt, hsState, err := protocol.BuildConnectRequest(psk)
	if err != nil {
		return nil, err
	}

	buf := make([]byte, 2048)
	var codec *protocol.GNSCodec

	// Up to 3 attempts (1.5s each) to complete 1-RTT X25519 handshake over UDP
	for attempt := 1; attempt <= 3; attempt++ {
		if _, err := raw.WriteTo(reqPkt, remoteAddr); err != nil {
			return nil, err
		}
		_ = raw.SetReadDeadline(time.Now().Add(1500 * time.Millisecond))
		n, _, rerr := raw.ReadFrom(buf)
		if rerr != nil {
			if ne, ok := rerr.(net.Error); ok && ne.Timeout() {
				continue
			}
			_ = raw.SetReadDeadline(time.Time{})
			return nil, rerr
		}
		c, herr := protocol.CompleteClientHandshake(buf[:n], hsState)
		if herr == nil {
			codec = c
			break
		}
	}
	_ = raw.SetReadDeadline(time.Time{})

	if codec == nil {
		return nil, errors.New("cs2world X25519 GNS handshake timed out or rejected by server")
	}

	c := &GNSPacketConn{
		raw:         raw,
		psk:         psk,
		clientCodec: codec,
		shaper:      shaper.NewStateMachine(shaperCfg),
		isServer:    false,
		remoteAddr:  remoteAddr,
		closeCh:     make(chan struct{}),
		readBuf:     make([]byte, 2048),
	}
	if c.shaper.Enabled() {
		go c.dummyHeartbeatLoop()
	}
	return c, nil
}

func (c *GNSPacketConn) sessionCleanupLoop() {
	ticker := time.NewTicker(60 * time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-c.closeCh:
			return
		case <-ticker.C:
			cutoff := time.Now().Unix() - sessionIdleTimeoutSec
			c.sessMu.Lock()
			for id, sess := range c.byConnID {
				if sess.LastActiveUnix() < cutoff {
					delete(c.byConnID, id)
				}
			}
			for addrStr, sess := range c.byAddrStr {
				if sess.LastActiveUnix() < cutoff {
					delete(c.byAddrStr, addrStr)
				}
			}
			c.sessMu.Unlock()
		}
	}
}

func (c *GNSPacketConn) dummyHeartbeatLoop() {
	ticker := time.NewTicker(46 * time.Millisecond)
	defer ticker.Stop()

	for {
		select {
		case <-c.closeCh:
			return
		case <-ticker.C:
			if atomic.LoadInt32(&c.closed) != 0 || c.remoteAddr == nil || c.clientCodec == nil {
				return
			}
			emit, targetSz := c.shaper.ShouldEmitDummyTick()
			if !emit {
				continue
			}
			dummyPkt := c.clientCodec.Seal(nil, protocol.ChannelIdleDummy, targetSz)
			if _, err := c.raw.WriteTo(dummyPkt, c.remoteAddr); err == nil {
				atomic.AddUint64(&c.DummyTicksSent, 1)
			}
		}
	}
}

func (c *GNSPacketConn) ReadFrom(p []byte) (int, net.Addr, error) {
	c.readMu.Lock()
	defer c.readMu.Unlock()

	for {
		n, addr, err := c.raw.ReadFrom(c.readBuf)
		if err != nil {
			return 0, addr, err
		}
		pkt := c.readBuf[:n]

		if c.isServer {
			// Step 1: Valve A2S Active Probe Defense
			if c.responder != nil && a2s.IsA2SQuery(pkt) {
				if resp := c.responder.HandleQuery(pkt); len(resp) > 0 {
					_, _ = c.raw.WriteTo(resp, addr)
					cnt := atomic.AddUint64(&c.A2SDeflected, 1)
					if cnt <= 20 || cnt%100 == 0 {
						log.Printf("[A2S Defense] Deflected Source Engine Query from %s (total=%d)", addr, cnt)
					}
				}
				continue
			}

			// Step 2: 1-RTT X25519 PFS Handshake (ConnectRequest 0x01 -> ConnectOK 0x03)
			if len(pkt) >= protocol.ConnectReqMinSize && pkt[0] == protocol.GNSFlagConnectReq {
				resp, newCodec, herr := protocol.HandleConnectRequest(pkt, c.psk)
				if herr != nil {
					reject := protocol.BuildGNSRejectPacket("Server is full (20/20). VAC authentication required.")
					_, _ = c.raw.WriteTo(reject, addr)
					atomic.AddUint64(&c.ProbesRejected, 1)
					continue
				}
				c.sessMu.Lock()
				c.byConnID[newCodec.ConnID()] = newCodec
				c.byAddrStr[addr.String()] = newCodec
				c.sessMu.Unlock()

				_, _ = c.raw.WriteTo(resp, addr)
				atomic.AddUint64(&c.HandshakesOK, 1)
				log.Printf("[CS2World PFS] X25519 handshake completed with %s (connID=0x%08x)", addr, newCodec.ConnID())
				continue
			}

			// Step 3: Lookup Ephemeral Session by SessionConnID
			connID, ok := protocol.ExtractConnID(pkt)
			if !ok {
				reject := protocol.BuildGNSRejectPacket("Server is full (20/20). VAC authentication required.")
				_, _ = c.raw.WriteTo(reject, addr)
				atomic.AddUint64(&c.ProbesRejected, 1)
				continue
			}

			c.sessMu.RLock()
			sess := c.byConnID[connID]
			c.sessMu.RUnlock()

			if sess == nil {
				reject := protocol.BuildGNSRejectPacket("Server is full (20/20). VAC authentication required.")
				_, _ = c.raw.WriteTo(reject, addr)
				atomic.AddUint64(&c.ProbesRejected, 1)
				continue
			}

			// Step 4: AEAD Decryption + 64-bit Anti-Replay Sliding Window Check
			realPayload, mode, _, openErr := sess.Open(pkt)
			if openErr != nil {
				if errors.Is(openErr, protocol.ErrReplayDetected) {
					// Silently drop replay attacks without leaking any side-channel response
					cnt := atomic.AddUint64(&c.ReplaysBlocked, 1)
					if cnt <= 20 || cnt%100 == 0 {
						log.Printf("[Anti-Replay] Silently dropped replayed packet from %s (connID=0x%08x, total=%d)", addr, connID, cnt)
					}
					continue
				}
				reject := protocol.BuildGNSRejectPacket("Server is full (20/20). VAC authentication required.")
				_, _ = c.raw.WriteTo(reject, addr)
				atomic.AddUint64(&c.ProbesRejected, 1)
				continue
			}

			// Keep addr -> sess updated in case client NAT port migrates
			c.sessMu.Lock()
			c.byAddrStr[addr.String()] = sess
			c.sessMu.Unlock()

			if mode == protocol.ChannelIdleDummy || len(realPayload) == 0 {
				atomic.AddUint64(&c.DummyTicksRecvd, 1)
				continue
			}
			copied := copy(p, realPayload)
			return copied, addr, nil
		}

		// Client mode: decrypt using active ephemeral session codec
		realPayload, mode, _, openErr := c.clientCodec.Open(pkt)
		if openErr != nil {
			continue
		}
		if mode == protocol.ChannelIdleDummy || len(realPayload) == 0 {
			atomic.AddUint64(&c.DummyTicksRecvd, 1)
			continue
		}
		copied := copy(p, realPayload)
		return copied, addr, nil
	}
}

func (c *GNSPacketConn) WriteTo(p []byte, addr net.Addr) (int, error) {
	var codec *protocol.GNSCodec
	if c.isServer {
		c.sessMu.RLock()
		codec = c.byAddrStr[addr.String()]
		c.sessMu.RUnlock()
		if codec == nil {
			return 0, errors.New("cs2world server: no active X25519 session for remote addr")
		}
	} else {
		codec = c.clientCodec
	}

	mode, targetSz := c.shaper.PlanPacket(len(p))
	wirePkt := codec.Seal(p, mode, targetSz)
	_, err := c.raw.WriteTo(wirePkt, addr)
	if err != nil {
		return 0, err
	}
	return len(p), nil
}

func (c *GNSPacketConn) Close() error {
	c.closeOnce.Do(func() {
		atomic.StoreInt32(&c.closed, 1)
		close(c.closeCh)
	})
	return c.raw.Close()
}

func (c *GNSPacketConn) LocalAddr() net.Addr                { return c.raw.LocalAddr() }
func (c *GNSPacketConn) SetDeadline(t time.Time) error      { return c.raw.SetDeadline(t) }
func (c *GNSPacketConn) SetReadDeadline(t time.Time) error  { return c.raw.SetReadDeadline(t) }
func (c *GNSPacketConn) SetWriteDeadline(t time.Time) error { return c.raw.SetWriteDeadline(t) }
