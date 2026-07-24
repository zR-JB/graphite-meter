package transport

import "github.com/quic-go/quic-go"

// QUICInitialPacketSize keeps the first datagrams below a 1280-byte tunnel MTU.
// DPLPMTUD remains enabled, so established paths can still use larger packets.
const QUICInitialPacketSize uint16 = 1200

// NewQUICConfig returns the shared MTU-safe defaults for clients and servers.
// Datagrams carry the WebTransport latency bus, and a peer without them is
// refused at session upgrade.
func NewQUICConfig() *quic.Config {
	return &quic.Config{InitialPacketSize: QUICInitialPacketSize, EnableDatagrams: true}
}
