package protocol

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/ecdh"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"math"
	"sync"
	"sync/atomic"
	"time"
)

// CS2-World Protocol Version
const Version = "0.0.1"

// Valve GameNetworkingSockets (GNS) Mimicry Wire Format v0.0.1
//
// Control Handshake (1-RTT X25519 Perfect Forward Secrecy):
//   Client -> Server: ConnectRequest (Flag 0x01, padded to ~148B)
//     [0]      Flag = 0x01
//     [1..4]   ClientTmpID (uint32)
//     [5..36]  Client X25519 Ephemeral Public Key (32B)
//     [37..44] UnixTimestampSec (int64 BigEndian, +/- 60s anti-replay window)
//     [45..60] HMAC-SHA256(PSK, pkt[0:45])[:16]
//     [61..N]  Random Padding (mimics Valve GNS ConnectRequest length)
//
//   Server -> Client: ConnectOK (Flag 0x03, padded to ~92B)
//     [0]      Flag = 0x03
//     [1..4]   Assigned SessionConnID (uint32)
//     [5..36]  Server X25519 Ephemeral Public Key (32B)
//     [37..52] HMAC-SHA256(PSK, pkt[0:37] || ClientTmpID || ClientPub)[:16]
//     [53..N]  Random Padding (mimics Valve GNS ConnectOK length)
//
// Encrypted Data Packet (Flag 0x82):
//   Outer Unencrypted Header (9 bytes, authenticated as AEAD AAD):
//     [0]      Flags = 0x82 (Data packet with 32-bit sequence number)
//     [1..4]   SessionConnID (uint32, assigned during X25519 handshake)
//     [5..8]   SequenceNumber (uint32, monotonically increasing, checked by 64-bit ReplayWindow)
//   Encrypted Body (Per-session Ephemeral AES-256-GCM):
//     [0]      InnerChannelMode (0x00=IdleDummy, 0x01=CombatTick, 0x02=FastDL)
//     [1..2]   RealPayloadLen (uint16 BigEndian)
//     [3..3+N] Real KCP Payload
//     [3+N..M] AI Morphing Padding
//   Trailer:
//     16 bytes AES-256-GCM Authentication Tag

const (
	GNSFlagConnectReq   = byte(0x01)
	GNSFlagConnClosed   = byte(0x02)
	GNSFlagConnectOK    = byte(0x03)
	GNSFlagData32BitSeq = byte(0x82)

	ConnectReqMinSize = 1 + 4 + 32 + 8 + 16 // 61 bytes
	ConnectOKMinSize  = 1 + 4 + 32 + 16     // 53 bytes

	OuterHeaderSize = 9
	InnerHeaderSize = 3
	GCMTagSize      = 16
	MinPacketSize   = OuterHeaderSize + InnerHeaderSize + GCMTagSize

	ChannelIdleDummy  = byte(0x00)
	ChannelCombatTick = byte(0x01)
	ChannelFastDL     = byte(0x02)

	HandshakeMaxClockSkewSec = 60
)

var (
	ErrPacketTooShort   = errors.New("gns packet too short")
	ErrAuthFailed       = errors.New("gns aead authentication failed")
	ErrInvalidLength    = errors.New("gns inner payload length exceeds frame")
	ErrReplayDetected   = errors.New("gns replay packet detected")
	ErrHandshakeInvalid = errors.New("gns handshake authentication or timestamp invalid")
)

// ReplayWindow implements a 64-bit sliding window anti-replay filter (RFC 6479 style).
type ReplayWindow struct {
	mu     sync.Mutex
	maxSeq uint64
	bitmap uint64
}

// CheckAndAccept verifies whether seq is fresh (not a duplicate and not older than maxSeq-63).
// Returns true if accepted, false if it is a replay attack.
func (w *ReplayWindow) CheckAndAccept(seq uint32) bool {
	if seq == 0 {
		return false
	}
	s := uint64(seq)
	w.mu.Lock()
	defer w.mu.Unlock()

	if w.maxSeq == 0 {
		w.maxSeq = s
		w.bitmap = 1
		return true
	}

	if s > w.maxSeq {
		shift := s - w.maxSeq
		if shift >= 64 {
			w.bitmap = 1
		} else {
			w.bitmap = (w.bitmap << shift) | 1
		}
		w.maxSeq = s
		return true
	}

	diff := w.maxSeq - s
	if diff >= 64 {
		return false // Too old (outside sliding window)
	}
	mask := uint64(1) << diff
	if (w.bitmap & mask) != 0 {
		return false // Duplicate / Replay detected
	}
	w.bitmap |= mask
	return true
}

// GNSCodec holds an ephemeral per-session AES-256-GCM cipher derived via X25519 ECDH,
// plus a 64-bit anti-replay window.
type GNSCodec struct {
	aead         cipher.AEAD
	txSalt       [4]byte
	rxSalt       [4]byte
	connID       uint32
	txSeq        uint32
	replayWindow ReplayWindow
	lastActive   int64 // unix seconds
}

// deriveSessionCodec derives independent directional nonce salts and an ephemeral AES-256-GCM key
// from X25519 shared secret + PSK + transcript (clientPub || serverPub).
func deriveSessionCodec(sharedSecret []byte, psk string, connID uint32, clientPub, serverPub []byte, isServer bool) (*GNSCodec, error) {
	// Extract & expand using HMAC-SHA256 (HKDF-style)
	prkMac := hmac.New(sha256.New, []byte("cs2world-pfs-v0.0.1:"+psk))
	prkMac.Write(sharedSecret)
	prkMac.Write(clientPub)
	prkMac.Write(serverPub)
	prk := prkMac.Sum(nil)

	// Session AES-256 Key
	keyMac := hmac.New(sha256.New, prk)
	keyMac.Write([]byte("aes-256-gcm-key"))
	aesKey := keyMac.Sum(nil)

	// Directional Nonce Salts (C2S and S2C never share nonces even if seq numbers match)
	c2sMac := hmac.New(sha256.New, prk)
	c2sMac.Write([]byte("c2s-nonce-salt"))
	c2sSalt := c2sMac.Sum(nil)

	s2cMac := hmac.New(sha256.New, prk)
	s2cMac.Write([]byte("s2c-nonce-salt"))
	s2cSalt := s2cMac.Sum(nil)

	block, err := aes.NewCipher(aesKey)
	if err != nil {
		return nil, err
	}
	aead, err := cipher.NewGCM(block)
	if err != nil {
		return nil, err
	}

	c := &GNSCodec{
		aead:       aead,
		connID:     connID,
		lastActive: time.Now().Unix(),
	}
	if isServer {
		copy(c.txSalt[:], s2cSalt[:4])
		copy(c.rxSalt[:], c2sSalt[:4])
	} else {
		copy(c.txSalt[:], c2sSalt[:4])
		copy(c.rxSalt[:], s2cSalt[:4])
	}
	return c, nil
}

// ClientHandshakeState stores ephemeral X25519 private key during the 1-RTT handshake.
type ClientHandshakeState struct {
	PrivKey     *ecdh.PrivateKey
	ClientTmpID uint32
	PSK         string
}

// BuildConnectRequest creates a realistic Valve GNS ConnectRequest (~148B) with X25519 public key.
func BuildConnectRequest(psk string) ([]byte, *ClientHandshakeState, error) {
	priv, err := ecdh.X25519().GenerateKey(rand.Reader)
	if err != nil {
		return nil, nil, err
	}
	var idBuf [4]byte
	_, _ = rand.Read(idBuf[:])
	tmpID := binary.LittleEndian.Uint32(idBuf[:])

	// Pad to ~148 bytes (realistic Valve GNS ConnectRequest size)
	pkt := make([]byte, 148)
	pkt[0] = GNSFlagConnectReq
	binary.LittleEndian.PutUint32(pkt[1:5], tmpID)
	copy(pkt[5:37], priv.PublicKey().Bytes())
	binary.BigEndian.PutUint64(pkt[37:45], uint64(time.Now().Unix()))

	mac := hmac.New(sha256.New, []byte("cs2world-hs-req:"+psk))
	mac.Write(pkt[0:45])
	copy(pkt[45:61], mac.Sum(nil)[:16])

	// Fill remaining bytes with random padding
	_, _ = rand.Read(pkt[61:])

	return pkt, &ClientHandshakeState{
		PrivKey:     priv,
		ClientTmpID: tmpID,
		PSK:         psk,
	}, nil
}

// HandleConnectRequest validates an incoming client ConnectRequest on the server,
// performs X25519 ECDH, destroys the ephemeral server private key, and returns
// (ConnectOK packet, newly derived ephemeral GNSCodec, error).
func HandleConnectRequest(pkt []byte, psk string) ([]byte, *GNSCodec, error) {
	if len(pkt) < ConnectReqMinSize || pkt[0] != GNSFlagConnectReq {
		return nil, nil, ErrHandshakeInvalid
	}

	// 1. Verify HMAC-SHA256 using PSK
	mac := hmac.New(sha256.New, []byte("cs2world-hs-req:"+psk))
	mac.Write(pkt[0:45])
	expectedMac := mac.Sum(nil)[:16]
	if !hmac.Equal(pkt[45:61], expectedMac) {
		return nil, nil, ErrHandshakeInvalid
	}

	// 2. Verify Unix timestamp within +/- 60 seconds to prevent handshake replay
	clientTs := int64(binary.BigEndian.Uint64(pkt[37:45]))
	skew := math.Abs(float64(time.Now().Unix() - clientTs))
	if skew > HandshakeMaxClockSkewSec {
		return nil, nil, ErrHandshakeInvalid
	}

	clientTmpIDBytes := pkt[1:5]
	clientPubBytes := pkt[5:37]
	clientPub, err := ecdh.X25519().NewPublicKey(clientPubBytes)
	if err != nil {
		return nil, nil, ErrHandshakeInvalid
	}

	// 3. Generate ephemeral server X25519 key pair & compute shared secret
	serverPriv, err := ecdh.X25519().GenerateKey(rand.Reader)
	if err != nil {
		return nil, nil, err
	}
	sharedSecret, err := serverPriv.ECDH(clientPub)
	if err != nil {
		return nil, nil, ErrHandshakeInvalid
	}
	serverPubBytes := serverPriv.PublicKey().Bytes()

	// 4. Assign non-zero random 32-bit SessionConnID
	var idBuf [4]byte
	_, _ = rand.Read(idBuf[:])
	connID := binary.LittleEndian.Uint32(idBuf[:])
	if connID == 0 {
		connID = 1
	}

	// 5. Derive ephemeral per-session AES-256-GCM codec
	codec, err := deriveSessionCodec(sharedSecret, psk, connID, clientPubBytes, serverPubBytes, true)
	if err != nil {
		return nil, nil, err
	}

	// 6. Build Valve GNS ConnectOK (~92B) response
	resp := make([]byte, 92)
	resp[0] = GNSFlagConnectOK
	binary.LittleEndian.PutUint32(resp[1:5], connID)
	copy(resp[5:37], serverPubBytes)

	respMac := hmac.New(sha256.New, []byte("cs2world-hs-ok:"+psk))
	respMac.Write(resp[0:37])
	respMac.Write(clientTmpIDBytes)
	respMac.Write(clientPubBytes)
	copy(resp[37:53], respMac.Sum(nil)[:16])
	_, _ = rand.Read(resp[53:])

	return resp, codec, nil
}

// CompleteClientHandshake verifies the server's ConnectOK packet and derives the matching
// ephemeral AES-256-GCM session codec on the client.
func CompleteClientHandshake(resp []byte, state *ClientHandshakeState) (*GNSCodec, error) {
	if len(resp) < ConnectOKMinSize || resp[0] != GNSFlagConnectOK {
		return nil, ErrHandshakeInvalid
	}

	var tmpIDBytes [4]byte
	binary.LittleEndian.PutUint32(tmpIDBytes[:], state.ClientTmpID)
	clientPubBytes := state.PrivKey.PublicKey().Bytes()

	respMac := hmac.New(sha256.New, []byte("cs2world-hs-ok:"+state.PSK))
	respMac.Write(resp[0:37])
	respMac.Write(tmpIDBytes[:])
	respMac.Write(clientPubBytes)
	if !hmac.Equal(resp[37:53], respMac.Sum(nil)[:16]) {
		return nil, ErrHandshakeInvalid
	}

	connID := binary.LittleEndian.Uint32(resp[1:5])
	serverPubBytes := resp[5:37]
	serverPub, err := ecdh.X25519().NewPublicKey(serverPubBytes)
	if err != nil {
		return nil, ErrHandshakeInvalid
	}

	sharedSecret, err := state.PrivKey.ECDH(serverPub)
	if err != nil {
		return nil, ErrHandshakeInvalid
	}

	return deriveSessionCodec(sharedSecret, state.PSK, connID, clientPubBytes, serverPubBytes, false)
}

// ExtractConnID reads the 32-bit SessionConnID from a Data (0x82) packet header.
func ExtractConnID(packet []byte) (uint32, bool) {
	if len(packet) < MinPacketSize || packet[0] != GNSFlagData32BitSeq {
		return 0, false
	}
	return binary.LittleEndian.Uint32(packet[1:5]), true
}

func (c *GNSCodec) ConnID() uint32 {
	return c.connID
}

func (c *GNSCodec) LastActiveUnix() int64 {
	return atomic.LoadInt64(&c.lastActive)
}

func (c *GNSCodec) touch() {
	atomic.StoreInt64(&c.lastActive, time.Now().Unix())
}

// Seal wraps a KCP payload (or empty dummy payload) into a Valve GNS mimicked UDP datagram,
// padding the final UDP datagram size to targetTotalLen according to the AI Traffic Shaper.
func (c *GNSCodec) Seal(payload []byte, channelMode byte, targetTotalLen int) []byte {
	c.touch()
	seq := atomic.AddUint32(&c.txSeq, 1)

	minTotal := OuterHeaderSize + InnerHeaderSize + len(payload) + GCMTagSize
	if targetTotalLen < minTotal {
		targetTotalLen = minTotal
	}
	if targetTotalLen > 1380 && minTotal <= 1380 {
		targetTotalLen = 1380
	}
	padLen := targetTotalLen - minTotal

	outer := make([]byte, OuterHeaderSize, targetTotalLen)
	outer[0] = GNSFlagData32BitSeq
	binary.LittleEndian.PutUint32(outer[1:5], c.connID)
	binary.BigEndian.PutUint32(outer[5:9], seq)

	plain := make([]byte, InnerHeaderSize+len(payload)+padLen)
	plain[0] = channelMode
	binary.BigEndian.PutUint16(plain[1:3], uint16(len(payload)))
	copy(plain[3:3+len(payload)], payload)
	if padLen > 0 {
		_, _ = rand.Read(plain[3+len(payload):])
	}

	var nonce [12]byte
	copy(nonce[0:4], c.txSalt[:])
	binary.LittleEndian.PutUint32(nonce[4:8], c.connID)
	binary.BigEndian.PutUint32(nonce[8:12], seq)

	return c.aead.Seal(outer, nonce[:], plain, outer)
}

// Open authenticates, checks anti-replay sliding window, and decrypts an incoming GNS packet.
func (c *GNSCodec) Open(packet []byte) ([]byte, byte, uint32, error) {
	if len(packet) < MinPacketSize {
		return nil, 0, 0, ErrPacketTooShort
	}
	if packet[0] != GNSFlagData32BitSeq {
		return nil, 0, 0, ErrAuthFailed
	}

	outer := packet[:OuterHeaderSize]
	senderConnID := binary.LittleEndian.Uint32(outer[1:5])
	if senderConnID != c.connID {
		return nil, 0, 0, ErrAuthFailed
	}
	seq := binary.BigEndian.Uint32(outer[5:9])

	var nonce [12]byte
	copy(nonce[0:4], c.rxSalt[:])
	binary.LittleEndian.PutUint32(nonce[4:8], senderConnID)
	binary.BigEndian.PutUint32(nonce[8:12], seq)

	// Decrypt & authenticate FIRST so attackers cannot poison replayWindow with forged seqs
	plain, err := c.aead.Open(nil, nonce[:], packet[OuterHeaderSize:], outer)
	if err != nil {
		return nil, 0, 0, ErrAuthFailed
	}

	// Check 64-bit sliding window anti-replay AFTER AEAD verification succeeds
	if !c.replayWindow.CheckAndAccept(seq) {
		return nil, 0, 0, ErrReplayDetected
	}
	c.touch()

	if len(plain) < InnerHeaderSize {
		return nil, 0, 0, ErrInvalidLength
	}

	mode := plain[0]
	realLen := int(binary.BigEndian.Uint16(plain[1:3]))
	if InnerHeaderSize+realLen > len(plain) {
		return nil, 0, 0, ErrInvalidLength
	}

	return plain[InnerHeaderSize : InnerHeaderSize+realLen], mode, senderConnID, nil
}

// BuildGNSRejectPacket returns a realistic Valve GameNetworkingSockets ConnectionClosed packet
// when an unauthorized CS2 client or active prober sends a non-A2S handshake to our UDP port.
func BuildGNSRejectPacket(reason string) []byte {
	if reason == "" {
		reason = "Server is full (20/20). Please try again later."
	}
	msg := make([]byte, 0, 16+len(reason))
	msg = append(msg, GNSFlagConnClosed, 0x00, 0x00, 0x00, 0x00, 0x8B, 0x13, 0x00, 0x00)
	msg = append(msg, []byte(reason)...)
	msg = append(msg, 0x00)
	return msg
}
