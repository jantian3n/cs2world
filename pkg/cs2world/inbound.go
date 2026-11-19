package cs2world

import (
	"context"
	"fmt"
	"net"
	"net/netip"
	"strconv"
	"time"

	"github.com/sagernet/sing-box/adapter"
	"github.com/sagernet/sing-box/adapter/inbound"
	"github.com/sagernet/sing-box/common/uot"
	"github.com/sagernet/sing-box/log"
	E "github.com/sagernet/sing/common/exceptions"
	"github.com/sagernet/sing/common/logger"
	M "github.com/sagernet/sing/common/metadata"
	"github.com/xtaci/kcp-go/v5"
	"github.com/xtaci/smux"

	"cs2-ghost/pkg/transport"
)

func RegisterInbound(registry *inbound.Registry) {
	inbound.Register[InboundOptions](registry, TypeCS2World, NewInbound)
}

var _ adapter.Inbound = (*Inbound)(nil)

type Inbound struct {
	inbound.Adapter
	ctx      context.Context
	router   adapter.ConnectionRouterEx
	logger   logger.ContextLogger
	options  InboundOptions
	listener *kcp.Listener
	gnsConn  *transport.GNSPacketConn
}

func NewInbound(ctx context.Context, router adapter.Router, logger log.ContextLogger, tag string, options InboundOptions) (adapter.Inbound, error) {
	if options.PSK == "" {
		return nil, E.New("cs2world inbound requires non-empty 'psk'")
	}
	options.KCP.applyDefaults()
	return &Inbound{
		Adapter: inbound.NewAdapter(TypeCS2World, tag),
		ctx:     ctx,
		router:  uot.NewRouter(router, logger),
		logger:  logger,
		options: options,
	}, nil
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

func tuneKCP(sess *kcp.UDPSession, k KCPOptions) {
	sess.SetStreamMode(true)
	sess.SetWriteDelay(false)
	sess.SetNoDelay(1, 15, 2, 1) // 64-Tick aligned fast retransmission
	sess.SetWindowSize(k.SndWnd, k.RcvWnd)
	sess.SetMtu(k.MTU)
	sess.SetACKNoDelay(true)
}

func (h *Inbound) Start(stage adapter.StartStage, scope *adapter.Scope) error {
	if stage != adapter.StartStateStart {
		return nil
	}

	listenHost := "0.0.0.0"
	if h.options.Listen != nil {
		addr := h.options.Listen.Build(netip.IPv4Unspecified())
		if addr.IsValid() {
			listenHost = addr.String()
		}
	}
	listenAddrStr := net.JoinHostPort(listenHost, strconv.Itoa(int(h.options.ListenPort)))
	udpAddr, err := net.ResolveUDPAddr("udp", listenAddrStr)
	if err != nil {
		return E.Cause(err, "resolve cs2world udp listen address")
	}

	rawConn, err := net.ListenUDP("udp", udpAddr)
	if err != nil {
		return E.Cause(err, "listen cs2world udp ", listenAddrStr)
	}
	_ = rawConn.SetReadBuffer(4 * 1024 * 1024)
	_ = rawConn.SetWriteBuffer(4 * 1024 * 1024)

	gnsConn, err := transport.NewServerPacketConn(rawConn, h.options.PSK, h.options.A2S, h.options.Shaper)
	if err != nil {
		_ = rawConn.Close()
		return E.Cause(err, "init cs2world GNS server conn")
	}

	kcpListener, err := kcp.ServeConn(nil, h.options.KCP.DataShards, h.options.KCP.ParityShards, gnsConn)
	if err != nil {
		_ = gnsConn.Close()
		return E.Cause(err, "start cs2world kcp listener")
	}

	h.gnsConn = gnsConn
	h.listener = kcpListener

	h.logger.Info(fmt.Sprintf("cs2world server listening on udp %s (CS2 A2S=%q, AI Shaper=%v)", listenAddrStr, h.options.A2S.ServerName, h.options.Shaper.Enabled))

	scope.Add(func() error {
		_ = h.listener.Close()
		return h.gnsConn.Close()
	})

	go h.acceptLoop()
	return nil
}

func (h *Inbound) acceptLoop() {
	for {
		kcpSess, err := h.listener.AcceptKCP()
		if err != nil {
			return
		}
		tuneKCP(kcpSess, h.options.KCP)
		h.logger.Info("accepted cs2world GNS session from ", kcpSess.RemoteAddr())
		go h.handleSession(kcpSess)
	}
}

func (h *Inbound) handleSession(kcpSess *kcp.UDPSession) {
	defer kcpSess.Close()
	mux, err := smux.Server(kcpSess, defaultSmuxConfig())
	if err != nil {
		h.logger.Error("cs2world smux init error: ", err)
		return
	}
	defer mux.Close()

	source := M.SocksaddrFromNet(kcpSess.RemoteAddr()).Unwrap()
	for {
		stream, err := mux.AcceptStream()
		if err != nil {
			return
		}
		go h.handleStream(stream, source)
	}
}

func (h *Inbound) handleStream(stream *smux.Stream, source M.Socksaddr) {
	// Read target destination address (sing standard Socksaddr wire format: supports IPv4, IPv6, Domain, and UoT MagicAddress)
	destination, err := M.SocksaddrSerializer.ReadAddrPort(stream)
	if err != nil {
		_ = stream.Close()
		h.logger.Error("cs2world read stream destination failed: ", err)
		return
	}

	var metadata adapter.InboundContext
	metadata.Inbound = h.Tag()
	metadata.InboundType = h.Type()
	metadata.Source = source
	metadata.Destination = destination

	h.logger.InfoContext(h.ctx, "cs2world inbound connection to ", destination)
	h.router.RouteConnectionEx(h.ctx, stream, metadata, nil)
}
