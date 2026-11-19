package main

import (
	"encoding/json"
	"flag"
	"io"
	"log"
	"net"
	"os"
	"sync"
	"time"

	"github.com/xtaci/kcp-go/v5"
	"github.com/xtaci/smux"

	"cs2-ghost/pkg/a2s"
	"cs2-ghost/pkg/shaper"
	"cs2-ghost/pkg/transport"
)

type AppConfig struct {
	Mode       string `json:"mode"`        // "server" or "client"
	ListenAddr string `json:"listen_addr"` // server: "0.0.0.0:27015" (UDP), client: "127.0.0.1:9000" (TCP)
	TargetAddr string `json:"target_addr"` // server: "127.0.0.1:10086" (local sing-box), client: "vps_ip:27015" (UDP)
	PSK        string `json:"psk"`         // Pre-shared key (must match between client and server)

	KCP struct {
		DataShards   int `json:"data_shards"`   // FEC data shards (0 to disable FEC)
		ParityShards int `json:"parity_shards"` // FEC parity shards (e.g., 10/3 for lossy networks)
		MTU          int `json:"mtu"`           // Recommended: 1180 so GNS+GCM fits cleanly under 1248B FastDL chunk
		SndWnd       int `json:"snd_wnd"`       // Send window (default 1024)
		RcvWnd       int `json:"rcv_wnd"`       // Recv window (default 1024)
	} `json:"kcp"`

	A2S    a2s.ServerConfig `json:"a2s"`
	Shaper shaper.Config    `json:"shaper"`
}

func loadConfig(path string) (*AppConfig, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var cfg AppConfig
	if err := json.Unmarshal(raw, &cfg); err != nil {
		return nil, err
	}
	if cfg.KCP.MTU <= 0 {
		cfg.KCP.MTU = 1180
	}
	if cfg.KCP.SndWnd <= 0 {
		cfg.KCP.SndWnd = 1024
	}
	if cfg.KCP.RcvWnd <= 0 {
		cfg.KCP.RcvWnd = 1024
	}
	return &cfg, nil
}

func tuneKCP(sess *kcp.UDPSession, cfg *AppConfig) {
	sess.SetStreamMode(true)
	sess.SetWriteDelay(false)
	// Fast mode aligned to 64-Tick (15ms interval, fast resend=2, no congestion throttling)
	sess.SetNoDelay(1, 15, 2, 1)
	sess.SetWindowSize(cfg.KCP.SndWnd, cfg.KCP.RcvWnd)
	sess.SetMtu(cfg.KCP.MTU)
	sess.SetACKNoDelay(true)
}

func defaultSmuxConfig() *smux.Config {
	sc := smux.DefaultConfig()
	sc.KeepAliveInterval = 15 * time.Second
	sc.KeepAliveTimeout = 45 * time.Second
	sc.MaxFrameSize = 32768
	sc.MaxReceiveBuffer = 4 * 1024 * 1024
	sc.MaxStreamBuffer = 1 * 1024 * 1024
	return sc
}

type closeWriter interface {
	CloseWrite() error
}

func pipeBidirectional(a, b io.ReadWriteCloser) {
	done := make(chan struct{}, 2)
	go func() {
		_, _ = io.Copy(a, b)
		if cw, ok := a.(closeWriter); ok {
			_ = cw.CloseWrite()
		}
		done <- struct{}{}
	}()
	go func() {
		_, _ = io.Copy(b, a)
		if cw, ok := b.(closeWriter); ok {
			_ = cw.CloseWrite()
		}
		done <- struct{}{}
	}()
	<-done
	// Give the reverse direction a short grace window to flush remaining response bytes
	// when one side performs a TCP half-close (SHUT_WR).
	select {
	case <-done:
	case <-time.After(2 * time.Second):
	}
	_ = a.Close()
	_ = b.Close()
}

func runServer(cfg *AppConfig) {
	udpAddr, err := net.ResolveUDPAddr("udp", cfg.ListenAddr)
	if err != nil {
		log.Fatalf("invalid listen_addr: %v", err)
	}
	rawConn, err := net.ListenUDP("udp", udpAddr)
	if err != nil {
		log.Fatalf("listen udp failed: %v", err)
	}
	_ = rawConn.SetReadBuffer(4 * 1024 * 1024)
	_ = rawConn.SetWriteBuffer(4 * 1024 * 1024)

	gnsConn, err := transport.NewServerPacketConn(rawConn, cfg.PSK, cfg.A2S, cfg.Shaper)
	if err != nil {
		log.Fatalf("init GNS server conn failed: %v", err)
	}

	listener, err := kcp.ServeConn(nil, cfg.KCP.DataShards, cfg.KCP.ParityShards, gnsConn)
	if err != nil {
		log.Fatalf("kcp ServeConn failed: %v", err)
	}
	defer listener.Close()

	log.Printf("[CS2-Ghost Server] Listening on UDP %s -> Forwarding to %s", cfg.ListenAddr, cfg.TargetAddr)
	log.Printf("[CS2-Ghost Server] Mimicking CS2 Dedicated Server: %q | AI Shaper=%v", cfg.A2S.ServerName, cfg.Shaper.Enabled)

	for {
		kcpSess, err := listener.AcceptKCP()
		if err != nil {
			log.Printf("[Server] accept kcp error: %v", err)
			continue
		}
		tuneKCP(kcpSess, cfg)
		log.Printf("[Server] New authenticated CS2-Ghost session from %s", kcpSess.RemoteAddr())

		go handleServerSession(kcpSess, cfg.TargetAddr)
	}
}

func handleServerSession(kcpSess *kcp.UDPSession, targetAddr string) {
	defer kcpSess.Close()
	mux, err := smux.Server(kcpSess, defaultSmuxConfig())
	if err != nil {
		log.Printf("[Server] smux init error: %v", err)
		return
	}
	defer mux.Close()

	for {
		stream, err := mux.AcceptStream()
		if err != nil {
			log.Printf("[Server] session %s closed: %v", kcpSess.RemoteAddr(), err)
			return
		}
		go func(st *smux.Stream) {
			defer st.Close()
			backend, err := net.DialTimeout("tcp", targetAddr, 5*time.Second)
			if err != nil {
				log.Printf("[Server] dial backend %s failed: %v", targetAddr, err)
				return
			}
			defer backend.Close()
			pipeBidirectional(st, backend)
		}(stream)
	}
}

type clientMuxManager struct {
	cfg     *AppConfig
	mu      sync.Mutex
	mux     *smux.Session
	gnsConn *transport.GNSPacketConn
}

func (m *clientMuxManager) getStream() (*smux.Stream, error) {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.mux != nil && !m.mux.IsClosed() {
		if st, err := m.mux.OpenStream(); err == nil {
			return st, nil
		}
		_ = m.mux.Close()
		if m.gnsConn != nil {
			_ = m.gnsConn.Close()
		}
		m.mux = nil
	}

	// Establish new UDP -> GNS -> KCP -> smux session
	remoteUDP, err := net.ResolveUDPAddr("udp", m.cfg.TargetAddr)
	if err != nil {
		return nil, err
	}
	rawConn, err := net.ListenUDP("udp", nil)
	if err != nil {
		return nil, err
	}
	_ = rawConn.SetReadBuffer(4 * 1024 * 1024)
	_ = rawConn.SetWriteBuffer(4 * 1024 * 1024)

	gnsConn, err := transport.NewClientPacketConn(rawConn, remoteUDP, m.cfg.PSK, m.cfg.Shaper)
	if err != nil {
		_ = rawConn.Close()
		return nil, err
	}

	kcpSess, err := kcp.NewConn(m.cfg.TargetAddr, nil, m.cfg.KCP.DataShards, m.cfg.KCP.ParityShards, gnsConn)
	if err != nil {
		_ = gnsConn.Close()
		return nil, err
	}
	tuneKCP(kcpSess, m.cfg)

	mux, err := smux.Client(kcpSess, defaultSmuxConfig())
	if err != nil {
		_ = kcpSess.Close()
		_ = gnsConn.Close()
		return nil, err
	}

	m.mux = mux
	m.gnsConn = gnsConn
	log.Printf("[Client] Connected CS2-Ghost GNS tunnel to %s (AI Shaper=%v, %dHz)", m.cfg.TargetAddr, m.cfg.Shaper.Enabled, m.cfg.Shaper.TickRateHz)

	return m.mux.OpenStream()
}

func runClient(cfg *AppConfig) {
	tcpListener, err := net.Listen("tcp", cfg.ListenAddr)
	if err != nil {
		log.Fatalf("listen tcp %s failed: %v", cfg.ListenAddr, err)
	}
	defer tcpListener.Close()

	mgr := &clientMuxManager{cfg: cfg}
	log.Printf("[CS2-Ghost Client] Local TCP listener on %s -> Remote CS2-Ghost UDP %s", cfg.ListenAddr, cfg.TargetAddr)

	for {
		localConn, err := tcpListener.Accept()
		if err != nil {
			log.Printf("[Client] accept tcp error: %v", err)
			continue
		}
		go func(lc net.Conn) {
			defer lc.Close()
			stream, err := mgr.getStream()
			if err != nil {
				log.Printf("[Client] open GNS tunnel stream failed: %v", err)
				return
			}
			defer stream.Close()
			pipeBidirectional(lc, stream)
		}(localConn)
	}
}

func main() {
	cfgPath := flag.String("config", "configs/server.json", "Path to CS2-Ghost JSON configuration file")
	flag.Parse()

	cfg, err := loadConfig(*cfgPath)
	if err != nil {
		log.Fatalf("failed to load config %s: %v", *cfgPath, err)
	}
	if cfg.PSK == "" {
		log.Fatal("psk must not be empty")
	}

	switch cfg.Mode {
	case "server":
		runServer(cfg)
	case "client":
		runClient(cfg)
	default:
		log.Fatalf("unknown mode %q (expected 'server' or 'client')", cfg.Mode)
	}
}
