package server

import (
	"net"

	"golang.org/x/sys/unix"
)

// Keep download bytes waiting for the network in the HTTP/2 scheduler, where
// control responses can still be interleaved. Unlike SO_SNDBUF, this preserves
// the kernel's send-buffer autotuning and the space for bytes already in flight.
func configureHTTP2TCP(c *net.TCPConn) {
	raw, err := c.SyscallConn()
	if err != nil {
		return
	}
	_ = raw.Control(func(fd uintptr) {
		_ = unix.SetsockoptInt(int(fd), unix.IPPROTO_TCP, unix.TCP_NOTSENT_LOWAT, 64<<10)
	})
}
