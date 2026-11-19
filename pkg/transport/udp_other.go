//go:build !windows

package transport

import "net"

// DisableUDPConnReset is a no-op on non-Windows platforms.
func DisableUDPConnReset(conn net.PacketConn) {}

func isTransientUDPError(err error) bool {
	return false
}
