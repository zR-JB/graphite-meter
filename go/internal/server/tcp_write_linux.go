package server

import (
	"net"

	"golang.org/x/sys/unix"
)

// configureHTTP2TCP holds unsent downloads in the HTTP/2 scheduler, where control replies interleave.
func configureHTTP2TCP(c *net.TCPConn) {
	raw, err := c.SyscallConn()
	if err != nil {
		return
	}
	_ = raw.Control(func(fd uintptr) {
		_ = unix.SetsockoptInt(int(fd), unix.IPPROTO_TCP, unix.TCP_NOTSENT_LOWAT, 64<<10)
	})
}
