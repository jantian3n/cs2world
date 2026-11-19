//go:build windows

package transport

import (
	"errors"
	"net"
	"syscall"
	"unsafe"

	"github.com/sagernet/sing/common"
)

const (
	sioUDPConnReset = 0x9800000C
	wsaeConnReset   = syscall.Errno(10054)
	wsaeMsgSize     = syscall.Errno(10040)
)

// DisableUDPConnReset suppresses ICMP Port Unreachable -> WSAECONNRESET (10054)
// errors on Windows UDP sockets, which would otherwise abort active KCP/GNS reads.
func DisableUDPConnReset(conn net.PacketConn) {
	udpConn, ok := common.Cast[*net.UDPConn](conn)
	if !ok || udpConn == nil {
		return
	}
	rawConn, err := udpConn.SyscallConn()
	if err != nil {
		return
	}
	_ = rawConn.Control(func(fd uintptr) {
		var flag uint32
		var bytesReturned uint32
		_ = syscall.WSAIoctl(
			syscall.Handle(fd),
			sioUDPConnReset,
			(*byte)(unsafe.Pointer(&flag)),
			uint32(unsafe.Sizeof(flag)),
			nil,
			0,
			&bytesReturned,
			nil,
			0,
		)
	})
}

func isTransientUDPError(err error) bool {
	return errors.Is(err, wsaeConnReset) || errors.Is(err, wsaeMsgSize)
}
