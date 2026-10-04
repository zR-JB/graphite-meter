//! Which routes each listener mounts, as Go's listener table and `newMux` decide them.
use crate::{
    auth::policy::{Connection, Listener},
    config::NativeKind,
};
use graphite_meter_core::route::Route;
use std::net::SocketAddr;

/// What a listener mounts, as Go's `muxTopology`. Every listener mounts the probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Topology {
    /// The browser app and the authentication pages, which also answer every path no route claims.
    pub(crate) spa: bool,
    /// `/preflight` and `/servers`.
    discovery: bool,
    /// The WebSocket ticket and ping routes.
    latency: bool,
    /// `/download` and `/upload`, with the upload control and WebTransport ticket routes.
    transfers: bool,
    /// The upload control and WebTransport ticket routes alone.
    control: bool,
    /// Probe answers advertise the HTTP/3 port.
    pub(crate) bootstrap: bool,
    /// The WebTransport sessions.
    pub(crate) webtransport: bool,
}

const NONE: Topology = Topology {
    spa: false,
    discovery: false,
    latency: false,
    transfers: false,
    control: false,
    bootstrap: false,
    webtransport: false,
};
const UI: Topology = Topology {
    spa: true,
    discovery: true,
    latency: true,
    transfers: true,
    ..NONE
};

impl Topology {
    /// Whether this listener serves `route`: the listeners' only mount decision.
    pub(crate) const fn mounts(self, route: Route) -> bool {
        match route {
            Route::Probe => true,
            Route::Preflight | Route::Servers => self.discovery,
            Route::WsSession | Route::Ping => self.latency,
            Route::Download | Route::Upload => self.transfers,
            Route::UploadSession | Route::UploadCheckpoint | Route::UploadProgress | Route::WtSession => {
                self.transfers || self.control
            }
            Route::WtDownload | Route::WtUpload | Route::WtPing => self.webtransport,
        }
    }

    /// What the authentication policy knows of the listener.
    pub(crate) const fn listener(self) -> Listener {
        Listener {
            ui: self.spa,
            webtransport: self.webtransport,
        }
    }
}

/// A listener as Go's table lists it: its role in the startup log, the protocol it negotiates, what it mounts.
pub(crate) struct ListenerSpec {
    pub(crate) role: &'static str,
    pub(crate) alpn: &'static [u8],
    pub(crate) topology: Topology,
}

/// The TCP listener a native address binds. Under authentication, clear HTTP/1.1 is for a trusted proxy alone.
pub(crate) const fn tcp(kind: NativeKind, authenticated: bool) -> ListenerSpec {
    match kind {
        NativeKind::H1 => ListenerSpec {
            role: if authenticated {
                "HTTP/1.1 clear: trusted proxy upstream only; direct requests are refused, GET / redirects to HTTPS"
            } else {
                "HTTP/1.1 clear: UI, discovery, probe, transfers, WebSockets"
            },
            alpn: b"",
            topology: UI,
        },
        NativeKind::H1Tls => ListenerSpec {
            role: "HTTPS/WSS HTTP/1.1: UI, discovery, probe, transfers, WebSockets",
            alpn: b"http/1.1",
            topology: UI,
        },
        NativeKind::H2 => ListenerSpec {
            role: "HTTPS HTTP/2: measurement probe, transfers, progress only",
            alpn: b"h2",
            topology: Topology {
                transfers: true,
                ..NONE
            },
        },
        NativeKind::H3 => ListenerSpec {
            role: "HTTPS HTTP/1.1 companion: HTTP/3 bootstrap probe, upload and ticket control",
            alpn: b"http/1.1",
            topology: Topology {
                control: true,
                bootstrap: true,
                ..NONE
            },
        },
    }
}

/// The QUIC endpoint the HTTP/3 address binds beside its TCP companion.
pub(crate) const QUIC: ListenerSpec = ListenerSpec {
    role: "HTTP/3: probe, transfers, progress, WebTransport",
    alpn: b"h3",
    topology: Topology {
        transfers: true,
        webtransport: true,
        ..NONE
    },
};

/// What the accepting listener knows of a request's connection, never anything a request claims.
#[derive(Clone, Copy)]
pub(crate) struct Accepted {
    pub(crate) peer: SocketAddr,
    pub(crate) tls: bool,
    pub(crate) topology: Topology,
}

impl Accepted {
    /// A connection the QUIC endpoint accepted from `peer`.
    pub(crate) const fn quic(peer: SocketAddr) -> Self {
        Self {
            peer,
            tls: true,
            topology: QUIC.topology,
        }
    }

    /// The authentication policy's view of the same connection.
    pub(crate) const fn connection(self) -> Connection {
        Connection {
            peer: self.peer,
            tls: self.tls,
            listener: self.topology.listener(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The paths of the routes `topology` mounts, in Go's route order.
    fn mounted(topology: Topology) -> String {
        let paths: Vec<_> = Route::ALL
            .into_iter()
            .filter(|&route| topology.mounts(route))
            .map(Route::path)
            .collect();
        paths.join(" ")
    }

    /// Go's `newMux` mounts these for each listener's topology (go/internal/server/listeners.go and mount.go).
    #[test]
    fn each_listener_mounts_the_routes_of_its_go_topology() {
        let ui = "/preflight /probe /download /upload /upload/session /upload/progress /wt/session /ws/session \
                  /ws/ping /servers /upload/checkpoint";
        let transfers = "/probe /download /upload /upload/session /upload/progress /wt/session";
        for authenticated in [false, true] {
            assert_eq!(mounted(tcp(NativeKind::H1, authenticated).topology), ui);
            assert_eq!(mounted(tcp(NativeKind::H1Tls, authenticated).topology), ui);
            let h2 = mounted(tcp(NativeKind::H2, authenticated).topology);
            assert_eq!(h2, format!("{transfers} /upload/checkpoint"));
            let companion = mounted(tcp(NativeKind::H3, authenticated).topology);
            assert_eq!(
                companion,
                "/probe /upload/session /upload/progress /wt/session /upload/checkpoint"
            );
        }
        let quic = format!("{transfers} /wt/download /wt/upload /wt/ping /upload/checkpoint");
        assert_eq!(mounted(QUIC.topology), quic);
        // Only the UI listeners serve the app and the authentication pages, and only QUIC WebTransport.
        let listeners = NativeKind::ALL.map(|kind| tcp(kind, false).topology.listener());
        assert_eq!(listeners.map(|listener| listener.ui), [true, true, false, false]);
        assert!(listeners.iter().all(|listener| !listener.webtransport));
        assert!(QUIC.topology.listener().webtransport && !QUIC.topology.listener().ui);
        assert!(tcp(NativeKind::H3, false).topology.bootstrap);
    }
}
