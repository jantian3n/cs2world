package cs2world

import (
	"cs2-ghost/pkg/a2s"
	"cs2-ghost/pkg/shaper"
	"github.com/sagernet/sing-box/option"
)

const TypeCS2World = "cs2world"

type KCPOptions struct {
	DataShards   int `json:"data_shards,omitempty"`
	ParityShards int `json:"parity_shards,omitempty"`
	MTU          int `json:"mtu,omitempty"`
	SndWnd       int `json:"snd_wnd,omitempty"`
	RcvWnd       int `json:"rcv_wnd,omitempty"`
}

func (k *KCPOptions) applyDefaults() {
	if k.MTU <= 0 {
		k.MTU = 1180
	}
	if k.SndWnd <= 0 {
		k.SndWnd = 1024
	}
	if k.RcvWnd <= 0 {
		k.RcvWnd = 1024
	}
}

// InboundOptions defines the native sing-box inbound schema for "type": "cs2world".
type InboundOptions struct {
	option.ListenOptions
	PSK    string           `json:"psk"`
	KCP    KCPOptions       `json:"kcp,omitempty"`
	A2S    a2s.ServerConfig `json:"a2s,omitempty"`
	Shaper shaper.Config    `json:"shaper,omitempty"`
}

// OutboundOptions defines the native sing-box outbound schema for "type": "cs2world".
type OutboundOptions struct {
	option.DialerOptions
	option.ServerOptions
	PSK    string        `json:"psk"`
	KCP    KCPOptions    `json:"kcp,omitempty"`
	Shaper shaper.Config `json:"shaper,omitempty"`
}
