package transport

import "github.com/quic-go/quic-go"

// QUICInitialPacketSize keeps the first datagrams below a 1280-byte tunnel MTU.
// DPLPMTUD remains enabled, so established paths can still use larger packets.
const QUICInitialPacketSize uint16 = 1200

// NewQUICConfig returns the shared MTU-safe defaults for clients and servers.
// WebTransport requires datagrams for the latency bus and partial delivery on
// stream resets; a peer without either is refused at session upgrade.
func NewQUICConfig() *quic.Config {
	return &quic.Config{
		InitialPacketSize:                QUICInitialPacketSize,
		EnableDatagrams:                  true,
		EnableStreamResetPartialDelivery: true,
	}
}
