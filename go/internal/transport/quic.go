package transport

import "github.com/quic-go/quic-go"

// QUICInitialPacketSize keeps the first datagrams below a 1280-byte tunnel MTU.
const QUICInitialPacketSize uint16 = 1200

// Receive-window ceilings for flow-control autotuning.
const (
	quicMaxStreamReceiveWindow     = 32 << 20
	quicMaxConnectionReceiveWindow = 48 << 20
)

// NewQUICConfig returns the shared MTU-safe defaults for clients and servers.
func NewQUICConfig() *quic.Config {
	return &quic.Config{
		InitialPacketSize:                QUICInitialPacketSize,
		EnableDatagrams:                  true,
		EnableStreamResetPartialDelivery: true,
		MaxStreamReceiveWindow:           quicMaxStreamReceiveWindow,
		MaxConnectionReceiveWindow:       quicMaxConnectionReceiveWindow,
	}
}
