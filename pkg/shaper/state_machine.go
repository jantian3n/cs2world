package shaper

import (
	"math"
	"math/rand"
	"sync"
	"time"

	"cs2-ghost/pkg/protocol"
)

// Config configures the AI-driven CS2 Sub-Tick Traffic Morphing engine.
type Config struct {
	Enabled         bool `json:"enabled"`
	TickRateHz      int  `json:"tick_rate_hz"`       // Default 64 Hz (15.625ms)
	IdleSleepSec    int  `json:"idle_sleep_seconds"` // Stop dummy ticks after N seconds of zero traffic
	CombatMeanBytes int  `json:"combat_mean_bytes"`  // Default 210 B
	FastDLChunkSize int  `json:"fastdl_chunk_size"`  // Default 1248 B
}

type State int

const (
	StateSleep  State = iota // No traffic for a long time; mimics disconnected/between-match pause
	StateIdle                // Spectating / Freeze time (~21Hz small 72B packets)
	StateCombat              // Active 64-Tick movement/shooting (browsing web / API calls)
	StateFastDL              // Workshop / Map Resource Downloading (4K video / large transfers)
)

// StateMachine models a realistic CS2 player's network rhythm using a Markovian
// transition tracker and bimodal Gaussian packet-size distribution.
type StateMachine struct {
	cfg          Config
	mu           sync.Mutex
	state        State
	lastRealTx   time.Time
	recentBytes  int
	windowStart  time.Time
	rng          *rand.Rand
}

func NewStateMachine(cfg Config) *StateMachine {
	if cfg.TickRateHz <= 0 {
		cfg.TickRateHz = 64
	}
	if cfg.IdleSleepSec <= 0 {
		cfg.IdleSleepSec = 600 // 10 minutes idle -> sleep (0 background bandwidth)
	}
	if cfg.CombatMeanBytes <= 0 {
		cfg.CombatMeanBytes = 210
	}
	if cfg.FastDLChunkSize <= 0 {
		cfg.FastDLChunkSize = 1248
	}

	return &StateMachine{
		cfg:         cfg,
		state:       StateIdle,
		lastRealTx:  time.Now(),
		windowStart: time.Now(),
		rng:         rand.New(rand.NewSource(time.Now().UnixNano())),
	}
}

func (sm *StateMachine) Enabled() bool {
	return sm.cfg.Enabled
}

func (sm *StateMachine) TickInterval() time.Duration {
	return time.Second / time.Duration(sm.cfg.TickRateHz)
}

// PlanPacket decides the CS2 channel mode and target morphed UDP packet size
// for an outgoing real KCP payload.
func (sm *StateMachine) PlanPacket(realPayloadLen int) (channelMode byte, targetSize int) {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	now := time.Now()
	sm.lastRealTx = now

	// Track sliding 250ms throughput window to switch smoothly between Combat and FastDL
	if now.Sub(sm.windowStart) > 250*time.Millisecond {
		sm.recentBytes = 0
		sm.windowStart = now
	}
	sm.recentBytes += realPayloadLen

	if !sm.cfg.Enabled {
		return protocol.ChannelCombatTick, realPayloadLen + protocol.MinPacketSize
	}

	// Transition logic: high sustained throughput or large single KCP segment -> FastDL
	if realPayloadLen > 480 || sm.recentBytes > 6000 {
		sm.state = StateFastDL
		return protocol.ChannelFastDL, sm.sampleFastDLSize(realPayloadLen)
	}

	sm.state = StateCombat
	return protocol.ChannelCombatTick, sm.sampleCombatSize(realPayloadLen)
}

// ShouldEmitDummyTick is called periodically by the 64-Tick background heartbeat loop.
// When real traffic is absent, it emits realistic CS2 Idle/Spectator state ticks
// until IdleSleepSec is exceeded (preventing infinite 24/7 idle streams).
func (sm *StateMachine) ShouldEmitDummyTick() (emit bool, targetSize int) {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	if !sm.cfg.Enabled {
		return false, 0
	}

	idleDur := time.Since(sm.lastRealTx)
	if idleDur > time.Duration(sm.cfg.IdleSleepSec)*time.Second {
		sm.state = StateSleep
		return false, 0
	}

	// Only emit dummy ticks if no real packet was sent in the last 35ms
	if idleDur < 35*time.Millisecond {
		return false, 0
	}

	sm.state = StateIdle
	// Bimodal idle heartbeat: ~72 bytes +/- 10 bytes (standard CS2 spectator keepalive)
	sz := int(math.Round(72 + sm.rng.NormFloat64()*10))
	if sz < 54 {
		sz = 54
	}
	if sz > 108 {
		sz = 108
	}
	return true, sz
}

func (sm *StateMachine) sampleCombatSize(realLen int) int {
	minNeeded := realLen + protocol.MinPacketSize
	// Gaussian distribution centered on CombatMeanBytes (e.g., 210B, stddev 38B)
	sampled := int(math.Round(float64(sm.cfg.CombatMeanBytes) + sm.rng.NormFloat64()*38))
	if sampled < minNeeded {
		// Add small jitter so padded length isn't a deterministic multiple
		return minNeeded + sm.rng.Intn(36)
	}
	return sampled
}

func (sm *StateMachine) sampleFastDLSize(realLen int) int {
	minNeeded := realLen + protocol.MinPacketSize
	// Align to Valve Source FastDL chunk size (1248B) when packet is large
	if minNeeded <= sm.cfg.FastDLChunkSize {
		// 85% probability align cleanly to 1248B chunk, 15% tail fragment
		if sm.rng.Float64() < 0.85 {
			return sm.cfg.FastDLChunkSize
		}
		return minNeeded + sm.rng.Intn(64)
	}
	return minNeeded
}
