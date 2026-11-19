package protocol

import (
	"bytes"
	"encoding/binary"
	"errors"
	"testing"
	"time"
)

func TestX25519HandshakeAndAntiReplay(t *testing.T) {
	psk := "super-secret-cs2world-test-key"

	// 1. Complete 1-RTT X25519 Handshake
	req, cliState, err := BuildConnectRequest(psk)
	if err != nil {
		t.Fatalf("BuildConnectRequest failed: %v", err)
	}
	if len(req) != 148 {
		t.Fatalf("expected 148B ConnectRequest, got %d", len(req))
	}

	okPkt, srvCodec, err := HandleConnectRequest(req, psk)
	if err != nil {
		t.Fatalf("HandleConnectRequest failed: %v", err)
	}
	if len(okPkt) != 92 {
		t.Fatalf("expected 92B ConnectOK, got %d", len(okPkt))
	}

	cliCodec, err := CompleteClientHandshake(okPkt, cliState)
	if err != nil {
		t.Fatalf("CompleteClientHandshake failed: %v", err)
	}
	if cliCodec.ConnID() != srvCodec.ConnID() {
		t.Fatalf("connID mismatch: client=%d server=%d", cliCodec.ConnID(), srvCodec.ConnID())
	}

	// 2. Verify wrong PSK is rejected during handshake
	if _, _, err := HandleConnectRequest(req, "wrong-psk"); !errors.Is(err, ErrHandshakeInvalid) {
		t.Fatalf("expected ErrHandshakeInvalid for wrong PSK, got %v", err)
	}

	// 3. Verify expired timestamp handshake replay (>60s old) is rejected
	oldReq := append([]byte(nil), req...)
	binary.BigEndian.PutUint64(oldReq[37:45], uint64(time.Now().Unix()-120))
	if _, _, err := HandleConnectRequest(oldReq, psk); !errors.Is(err, ErrHandshakeInvalid) {
		t.Fatalf("expected ErrHandshakeInvalid for stale timestamp, got %v", err)
	}

	// 4. Seal & Open normal bidirectional traffic
	msg := []byte("cs2world-v0.0.1-pfs-payload")
	wire1 := cliCodec.Seal(msg, ChannelCombatTick, 210)
	if len(wire1) != 210 {
		t.Fatalf("expected morphed 210B packet, got %d", len(wire1))
	}
	got, mode, _, err := srvCodec.Open(wire1)
	if err != nil {
		t.Fatalf("srvCodec.Open failed: %v", err)
	}
	if mode != ChannelCombatTick || !bytes.Equal(got, msg) {
		t.Fatalf("payload mismatch: mode=%d got=%q", mode, got)
	}

	// 5. Replay attack check: re-sending exact wire1 must trigger ErrReplayDetected
	if _, _, _, err := srvCodec.Open(wire1); !errors.Is(err, ErrReplayDetected) {
		t.Fatalf("expected ErrReplayDetected on duplicate packet, got %v", err)
	}

	// 6. Tamper outer AAD header check: flipping 1 bit in Seq must trigger ErrAuthFailed
	wire2 := cliCodec.Seal(msg, ChannelCombatTick, 210)
	tampered := append([]byte(nil), wire2...)
	tampered[8] ^= 0x01
	if _, _, _, err := srvCodec.Open(tampered); !errors.Is(err, ErrAuthFailed) {
		t.Fatalf("expected ErrAuthFailed on tampered AAD header, got %v", err)
	}

	// 7. Out-of-order delivery within 64-packet window should succeed
	wire3 := cliCodec.Seal([]byte("pkt-3"), ChannelCombatTick, 210)
	wire4 := cliCodec.Seal([]byte("pkt-4"), ChannelCombatTick, 210)
	if _, _, _, err := srvCodec.Open(wire4); err != nil {
		t.Fatalf("wire4 open failed: %v", err)
	}
	if _, _, _, err := srvCodec.Open(wire3); err != nil {
		t.Fatalf("out-of-order wire3 within 64-bit window should succeed, got: %v", err)
	}
	// Replaying wire3 again must fail
	if _, _, _, err := srvCodec.Open(wire3); !errors.Is(err, ErrReplayDetected) {
		t.Fatalf("replayed wire3 must fail with ErrReplayDetected, got %v", err)
	}
}
