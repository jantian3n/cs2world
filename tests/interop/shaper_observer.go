// The test client sees an authenticated GNS endpoint. Recording the decrypted
// channel and payload length distinguishes an idle dummy from a short KCP ACK.
package main

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
	"time"

	"cs2-ghost/pkg/protocol"
)

type sample struct {
	Millis        float64
	Mode          byte
	Payload, Wire int
}

func observe() error {
	key := "loopback-shaper-fixture"
	socket, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		return err
	}
	defer socket.Close()
	fmt.Println(socket.LocalAddr().(*net.UDPAddr).Port)
	if err = socket.SetReadDeadline(time.Now().Add(10 * time.Second)); err != nil {
		return err
	}
	var codec *protocol.GNSCodec
	var start time.Time
	var samples []sample
	var packet [2048]byte
	for {
		n, addr, err := socket.ReadFromUDP(packet[:])
		if err != nil {
			if e, ok := err.(net.Error); ok && e.Timeout() && codec != nil {
				break
			}
			return err
		}
		pkt := packet[:n]
		if len(pkt) > 0 && pkt[0] == protocol.GNSFlagConnectReq {
			response, c, err := protocol.HandleConnectRequest(pkt, key)
			if err != nil {
				return err
			}
			if _, err = socket.WriteToUDP(response, addr); err != nil {
				return err
			}
			codec = c
			start = time.Now()
			socket.SetReadDeadline(start.Add(1800 * time.Millisecond))
			continue
		}
		if codec == nil {
			continue
		}
		payload, mode, _, err := codec.Open(pkt)
		if err != nil {
			return err
		}
		samples = append(samples, sample{float64(time.Since(start)) / float64(time.Millisecond), mode, len(payload), n})
	}
	return json.NewEncoder(os.Stdout).Encode(samples)
}
func main() {
	if err := observe(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
