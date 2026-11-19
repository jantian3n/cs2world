package a2s

import (
	"bytes"
	"encoding/binary"
	"math"
	"math/rand"
	"sync"
	"time"
)

// Valve Source Engine Query Constants
// Reference: https://developer.valvesoftware.com/wiki/Server_queries
const (
	HeaderPrefix      = uint32(0xFFFFFFFF)
	ReqA2SInfo        = byte(0x54) // 'T' - TSource Engine Query\x00
	ReqA2SPlayer      = byte(0x55) // 'U'
	ReqA2SRules       = byte(0x56) // 'V'
	RespChallenge     = byte(0x41) // 'A'
	RespA2SInfo       = byte(0x49) // 'I'
	RespA2SPlayer     = byte(0x44) // 'D'
	CS2AppID          = uint16(730)
	SourceProtocolVer = byte(0x11) // Protocol 17
)

type ServerConfig struct {
	ServerName  string   `json:"server_name"`
	Maps        []string `json:"maps"`
	MaxPlayers  byte     `json:"max_players"`
	GameVersion string   `json:"game_version"`
	MapCycleMin int      `json:"map_cycle_minutes"`
}

type fakePlayer struct {
	Name      string
	Score     int32
	JoinedAt  time.Time
}

// Responder simulates an authentic Counter-Strike 2 community dedicated server
// capable of passing Steam Server Browser, SourceQuery tools, and GFW active probes.
type Responder struct {
	cfg       ServerConfig
	mu        sync.Mutex
	startTime time.Time
	players   []fakePlayer
}

var defaultPlayerPool = []string{
	"s1mple_cn", "Niko_Peek", "m0NESY_fan", "DonkStyle", "ZywOo_AWP",
	"KarriganIGL", "ropz_lurk", "B1T_Headshot", "JameTime", "Twistzz_HK",
	"Sh1ro_Clutch", "Device_95", "XANTARESpeek", "Stewie2k_Smoke", "FalleN_BR",
	"Frozen_SK", "Rain_Entry", "Broky_Sniper",
}

func NewResponder(cfg ServerConfig) *Responder {
	if cfg.ServerName == "" {
		cfg.ServerName = "[Asia/HK] CS2 5v5 Competitive Practice #3 | 128Tick"
	}
	if len(cfg.Maps) == 0 {
		cfg.Maps = []string{"de_mirage", "de_inferno", "de_dust2", "de_ancient", "de_nuke"}
	}
	if cfg.MaxPlayers == 0 {
		cfg.MaxPlayers = 20
	}
	if cfg.GameVersion == "" {
		cfg.GameVersion = "1.40.4.2"
	}
	if cfg.MapCycleMin <= 0 {
		cfg.MapCycleMin = 30
	}

	r := &Responder{
		cfg:       cfg,
		startTime: time.Now(),
	}
	r.refreshFakePlayers()
	return r
}

// IsA2SQuery checks if the incoming UDP payload starts with Valve's 0xFFFFFFFF header.
func IsA2SQuery(pkt []byte) bool {
	if len(pkt) < 5 {
		return false
	}
	return pkt[0] == 0xFF && pkt[1] == 0xFF && pkt[2] == 0xFF && pkt[3] == 0xFF
}

// HandleQuery processes A2S_INFO, A2S_PLAYER, and generic probes, returning a valid CS2 response.
func (r *Responder) HandleQuery(pkt []byte) []byte {
	if len(pkt) < 5 {
		return nil
	}
	reqType := pkt[4]
	switch reqType {
	case ReqA2SInfo:
		// Modern CS2 servers optionally respond directly to A2S_INFO or issue a 4-byte challenge.
		// Responding directly to standard "TSource Engine Query\x00" ensures maximum compatibility
		// with both legacy and modern query tools.
		return r.BuildA2SInfo()
	case ReqA2SPlayer:
		// If client sent -1 challenge (0xFFFFFFFF), return a challenge number first; otherwise player list.
		if len(pkt) >= 9 && bytes.Equal(pkt[5:9], []byte{0xFF, 0xFF, 0xFF, 0xFF}) {
			return r.BuildChallenge()
		}
		return r.BuildA2SPlayer()
	default:
		return r.BuildChallenge()
	}
}

func (r *Responder) currentMap() string {
	elapsedMin := int(time.Since(r.startTime).Minutes())
	idx := (elapsedMin / r.cfg.MapCycleMin) % len(r.cfg.Maps)
	return r.cfg.Maps[idx]
}

// dynamicPlayerCount simulates realistic Asia-Pacific peak/off-peak player activity curve.
func (r *Responder) dynamicPlayerCount() int {
	hour := time.Now().Hour()
	// Sine curve peaking around 21:00 and troughing around 05:00
	ratio := 0.5 + 0.42*math.Sin(float64(hour-13)*math.Pi/12.0)
	target := int(math.Round(ratio * float64(r.cfg.MaxPlayers-2)))
	if target < 2 {
		target = 2
	}
	if target > int(r.cfg.MaxPlayers)-1 {
		target = int(r.cfg.MaxPlayers) - 1
	}
	return target
}

func (r *Responder) refreshFakePlayers() {
	count := r.dynamicPlayerCount()
	r.players = make([]fakePlayer, 0, count)
	perm := rand.Perm(len(defaultPlayerPool))
	now := time.Now()
	for i := 0; i < count && i < len(perm); i++ {
		r.players = append(r.players, fakePlayer{
			Name:     defaultPlayerPool[perm[i]],
			Score:    int32(rand.Intn(28) + 2),
			JoinedAt: now.Add(-time.Duration(rand.Intn(2400)+120) * time.Second),
		})
	}
}

// BuildA2SInfo constructs a byte-accurate Valve A2S_INFO (0x49) packet for CS2.
func (r *Responder) BuildA2SInfo() []byte {
	r.mu.Lock()
	defer r.mu.Unlock()

	playerCount := byte(r.dynamicPlayerCount())
	var buf bytes.Buffer

	// 4-byte 0xFFFFFFFF + 1-byte 'I' (0x49)
	buf.Write([]byte{0xFF, 0xFF, 0xFF, 0xFF, RespA2SInfo})
	// Protocol version
	buf.WriteByte(SourceProtocolVer)
	// Server Name (null-terminated)
	buf.WriteString(r.cfg.ServerName)
	buf.WriteByte(0x00)
	// Current Map (null-terminated)
	buf.WriteString(r.currentMap())
	buf.WriteByte(0x00)
	// Folder ("csgo" is still used by CS2 dedicated servers)
	buf.WriteString("csgo")
	buf.WriteByte(0x00)
	// Game Name
	buf.WriteString("Counter-Strike 2")
	buf.WriteByte(0x00)
	// Steam AppID (uint16 little-endian = 730)
	_ = binary.Write(&buf, binary.LittleEndian, CS2AppID)
	// Players, MaxPlayers, Bots
	buf.WriteByte(playerCount)
	buf.WriteByte(r.cfg.MaxPlayers)
	buf.WriteByte(0x00)
	// Server Type ('d' = dedicated), Environment ('l' = Linux), Visibility (0 = public), VAC (1 = secured)
	buf.WriteByte('d')
	buf.WriteByte('l')
	buf.WriteByte(0x00)
	buf.WriteByte(0x01)
	// Game Version
	buf.WriteString(r.cfg.GameVersion)
	buf.WriteByte(0x00)
	// Extra Data Flag (EDF = 0x80: includes Port, or 0x00 minimal)
	buf.WriteByte(0x00)

	return buf.Bytes()
}

// BuildA2SPlayer constructs a valid Valve A2S_PLAYER (0x44) packet.
func (r *Responder) BuildA2SPlayer() []byte {
	r.mu.Lock()
	defer r.mu.Unlock()

	if len(r.players) == 0 {
		r.refreshFakePlayers()
	}

	var buf bytes.Buffer
	buf.Write([]byte{0xFF, 0xFF, 0xFF, 0xFF, RespA2SPlayer})
	buf.WriteByte(byte(len(r.players)))

	now := time.Now()
	for idx, p := range r.players {
		buf.WriteByte(byte(idx))
		buf.WriteString(p.Name)
		buf.WriteByte(0x00)
		_ = binary.Write(&buf, binary.LittleEndian, p.Score)
		duration := float32(now.Sub(p.JoinedAt).Seconds())
		_ = binary.Write(&buf, binary.LittleEndian, duration)
	}
	return buf.Bytes()
}

// BuildChallenge constructs a standard Valve S2C_CHALLENGE (0x41) response.
func (r *Responder) BuildChallenge() []byte {
	resp := make([]byte, 9)
	resp[0], resp[1], resp[2], resp[3] = 0xFF, 0xFF, 0xFF, 0xFF
	resp[4] = RespChallenge
	binary.LittleEndian.PutUint32(resp[5:9], rand.Uint32())
	return resp
}
