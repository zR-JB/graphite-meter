//go:build !linux

package server

import "net"

func configureHTTP2TCP(*net.TCPConn) {}
