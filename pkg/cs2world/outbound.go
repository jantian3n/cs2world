package cs2world

import (
	"context"
	"net"
	"strconv"
	"sync"

	"github.com/sagernet/sing-box/adapter"
	"github.com/sagernet/sing-box/adapter/outbound"
	"github.com/sagernet/sing-box/log"
	E "github.com/sagernet/sing/common/exceptions"
	"github.com/sagernet/sing/common/logger"
	M "github.com/sagernet/sing/common/metadata"
	N "github.com/sagernet/sing/common/network"
	"github.com/sagernet/sing/common/uot"
	"github.com/xtaci/kcp-go/v5"
	"github.com/xtaci/smux"

	"cs2-ghost/pkg/transport"
)

func RegisterOutbound(registry *outbound.Registry) {
	outbound.Register[OutboundOptions](registry, TypeCS2World, NewOutbound)
}

var _ adapter.Outbound = (*Outbound)(nil)

type Outbound struct {
	outbound.Adapter
	logger    logger.ContextLogger
	options   OutboundOptions
	uotClient *uot.Client

	mu      sync.Mutex
	mux     *smux.Session
	gnsConn *transport.GNSPacketConn
}

func NewOutbound(ctx context.Context, router adapter.Router, logger log.ContextLogger, tag string, options OutboundOptions) (adapter.Outbound, error) {
	if options.PSK == "" {
		return nil, E.New("cs2world outbound requires non-empty 'psk'")
	}
	if options.Server == "" || options.ServerPort == 0 {
		return nil, E.New("cs2world outbound requires 'server' and 'server_port'")
	}
	options.KCP.applyDefaults()

	o := &Outbound{
		Adapter: outbound.NewAdapterWithDialerOptions(TypeCS2World, tag, []string{N.NetworkTCP, N.NetworkUDP}, options.DialerOptions),
		logger:  logger,
		options: options,
	}
	// Enable UDP-over-TCP (UoT v2) transparently over cs2world smux streams
	o.uotClient = &uot.Client{
		Dialer:  o,
		Version: uot.Version,
	}
	return o, nil
}

func (o *Outbound) openStream() (*smux.Stream, error) {
	o.mu.Lock()
	defer o.mu.Unlock()

	if o.mux != nil && !o.mux.IsClosed() {
		if st, err := o.mux.OpenStream(); err == nil {
			return st, nil
		}
		_ = o.mux.Close()
		if o.gnsConn != nil {
			_ = o.gnsConn.Close()
		}
		o.mux = nil
	}

	remoteStr := net.JoinHostPort(o.options.Server, strconv.Itoa(int(o.options.ServerPort)))
	remoteUDP, err := net.ResolveUDPAddr("udp", remoteStr)
	if err != nil {
		return nil, E.Cause(err, "resolve cs2world server ", remoteStr)
	}

	rawConn, err := net.ListenUDP("udp", nil)
	if err != nil {
		return nil, E.Cause(err, "open local udp socket for cs2world")
	}
	_ = rawConn.SetReadBuffer(4 * 1024 * 1024)
	_ = rawConn.SetWriteBuffer(4 * 1024 * 1024)

	gnsConn, err := transport.NewClientPacketConn(rawConn, remoteUDP, o.options.PSK, o.options.Shaper)
	if err != nil {
		_ = rawConn.Close()
		return nil, E.Cause(err, "init cs2world GNS client conn")
	}

	kcpSess, err := kcp.NewConn(remoteStr, nil, o.options.KCP.DataShards, o.options.KCP.ParityShards, gnsConn)
	if err != nil {
		_ = gnsConn.Close()
		return nil, E.Cause(err, "dial cs2world kcp session")
	}
	tuneKCP(kcpSess, o.options.KCP)

	mux, err := smux.Client(kcpSess, defaultSmuxConfig())
	if err != nil {
		_ = kcpSess.Close()
		_ = gnsConn.Close()
		return nil, E.Cause(err, "init cs2world smux client")
	}

	o.mux = mux
	o.gnsConn = gnsConn
	o.logger.Info("connected cs2world GNS tunnel to ", remoteStr, " (AI Shaper=", o.options.Shaper.Enabled, ")")

	return o.mux.OpenStream()
}

func (o *Outbound) DialContext(ctx context.Context, network string, destination M.Socksaddr) (net.Conn, error) {
	ctx, metadata := adapter.ExtendContext(ctx)
	metadata.Outbound = o.Tag()
	metadata.Destination = destination

	switch N.NetworkName(network) {
	case N.NetworkTCP:
		o.logger.InfoContext(ctx, "cs2world outbound connection to ", destination)
		stream, err := o.openStream()
		if err != nil {
			return nil, err
		}
		if err := M.SocksaddrSerializer.WriteAddrPort(stream, destination); err != nil {
			_ = stream.Close()
			return nil, E.Cause(err, "write cs2world target destination")
		}
		return stream, nil
	case N.NetworkUDP:
		o.logger.InfoContext(ctx, "cs2world outbound UoT packet connection to ", destination)
		return o.uotClient.DialContext(ctx, network, destination)
	default:
		return nil, E.Extend(N.ErrUnknownNetwork, network)
	}
}

func (o *Outbound) ListenPacket(ctx context.Context, destination M.Socksaddr) (net.PacketConn, error) {
	ctx, metadata := adapter.ExtendContext(ctx)
	metadata.Outbound = o.Tag()
	metadata.Destination = destination
	o.logger.InfoContext(ctx, "cs2world outbound UoT listen packet to ", destination)
	return o.uotClient.ListenPacket(ctx, destination)
}

func (o *Outbound) Close() error {
	o.mu.Lock()
	defer o.mu.Unlock()
	if o.mux != nil {
		_ = o.mux.Close()
		o.mux = nil
	}
	if o.gnsConn != nil {
		_ = o.gnsConn.Close()
		o.gnsConn = nil
	}
	return nil
}
