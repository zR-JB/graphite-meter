//! HTTP/3 requests over the pinned Noq transport, with an explicitly owned driver.
use std::{net::SocketAddr, sync::Arc, time::Duration};

use bytes::Bytes;
use graphite_meter_http3::{self as http3, RecvHalf, SendHalf};
use http::{Request, Response, Uri};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
    time::timeout,
};

use crate::Error;

const MAX_REQUESTS: usize = 256;
/// Go's receive ceilings (noq's credit is fixed; less caps 100 ms below 1 Gbit/s) and quic-go's in-flight cap.
const STREAM_RECEIVE_BYTES: u32 = 32 * 1024 * 1024;
const CONNECTION_RECEIVE_BYTES: u32 = 48 * 1024 * 1024;
const SEND_BYTES: u64 = 16 * 1024 * 1024;
const DATAGRAM_BYTES: usize = 256 * 1024;

/// Close even when cancellation occurs before QUIC or HTTP/3 setup finishes.
struct Endpoint(quinn::Endpoint);
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.0.close(0_u32.into(), b"client endpoint dropped");
    }
}

/// One QUIC connection and its HTTP/3 driver, for requests or a WebTransport session; dropping it closes both.
pub(crate) struct Connection {
    endpoint: Endpoint,
    quic: quinn::Connection,
    driver: JoinSet<()>,
}

impl Connection {
    /// Tries each address in turn; a silent one gets 3 s, so it cannot spend the whole attempt.
    pub(crate) async fn dial(origin: &Origin, insecure: bool) -> Result<(Self, http3::client::SendRequest), Error> {
        let mut config = quinn::ClientConfig::new(crate::tls::quic(insecure).await?);
        let mut transport = quinn::TransportConfig::default();
        transport.max_concurrent_bidi_streams(0_u32.into());
        transport.max_concurrent_uni_streams(36_u32.into());
        transport.stream_receive_window(STREAM_RECEIVE_BYTES.into());
        transport.receive_window(CONNECTION_RECEIVE_BYTES.into());
        transport.send_window(SEND_BYTES);
        transport.datagram_receive_buffer_size(Some(DATAGRAM_BYTES));
        transport.max_idle_timeout(Some(Duration::from_secs(60).try_into()?));
        config.transport_config(Arc::new(transport));
        let mut last_error: Option<Error> = None;
        for address in ipv4_first(graphite_meter_net::resolve(&origin.host, origin.port).await?) {
            let bind: SocketAddr = if address.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" }.parse()?;
            let (socket, warning) = graphite_meter_core::socket::udp_socket(bind)?;
            if let Some(warning) = warning {
                eprintln!("{warning}");
            }
            let endpoint = Endpoint(quinn::Endpoint::new(
                quinn::EndpointConfig::default(),
                None,
                socket,
                quinn::default_runtime().ok_or("no async runtime for QUIC")?,
            )?);
            let connecting = endpoint.0.connect_with(config.clone(), address, &origin.host)?;
            let connected = timeout(Duration::from_secs(3), connecting).await.map_err(Error::from);
            let quic = match connected.and_then(|quic| Ok(quic?)) {
                Ok(quic) => quic,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            let (mut driver, requests) = http3::client::new(quic.clone());
            let mut tasks = JoinSet::new();
            tasks.spawn(async move {
                let _ = driver.drive().await;
            });
            return Ok((
                Self {
                    endpoint,
                    quic,
                    driver: tasks,
                },
                requests,
            ));
        }
        Err(last_error.unwrap_or_else(|| "QUIC hostname resolved to no addresses".into()))
    }

    pub(crate) fn close_reason(&self) -> Option<quinn::ConnectionError> {
        self.quic.close_reason()
    }

    pub(crate) async fn close(mut self, reason: &[u8]) {
        self.quic.close(0_u32.into(), reason);
        self.endpoint.0.close(0_u32.into(), reason);
        self.driver.shutdown().await;
        let _ = timeout(Duration::from_secs(1), self.endpoint.0.wait_idle()).await;
    }
}

/// quic-go dials the first IPv4 address, or the first address when none is, as an IPv6 literal
/// resolves to itself alone (quic-go http3/ip_addr.go:23-48, client.go:42-50); the rest follow.
fn ipv4_first(mut addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
    addresses.sort_by_key(|address| !address.ip().to_canonical().is_ipv4());
    addresses
}

/// HTTP/3 or QUIC that this side found the peer breaking, which no retry mends. A stream or
/// connection the peer ended, with whatever code, or a lost connection is retried, as in Go.
pub(crate) fn violation(error: &(dyn std::error::Error + 'static)) -> bool {
    let quic = match error.downcast_ref() {
        Some(http3::Error::Protocol(_) | http3::Error::Connection { local: true, .. }) => return true,
        Some(http3::Error::Transport(error)) => Some(error),
        _ => error.downcast_ref::<quinn::ConnectionError>(),
    };
    matches!(quic, Some(quinn::ConnectionError::TransportError(_)))
}

/// Holds the sole driver task. Request streams retain this owner, so connection
/// work cannot outlive them. Requests never follow redirects or change authority.
pub struct Http3Client {
    connection: Connection,
    requests: http3::client::SendRequest,
    permits: Arc<Semaphore>,
    origin: Origin,
}

impl Http3Client {
    pub async fn connect(uri: &Uri, insecure: bool, deadline: Duration) -> Result<Self, Error> {
        let origin = Origin::from_uri(uri)?;
        let (connection, requests) = timeout(deadline, Connection::dial(&origin, insecure)).await??;
        Ok(Self {
            connection,
            requests,
            permits: Arc::new(Semaphore::new(MAX_REQUESTS)),
            origin,
        })
    }

    /// Closed, or going away since the server's GOAWAY: either takes no new request.
    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some() || self.requests.going_away()
    }

    pub async fn open(self: &Arc<Self>, request: Request<()>) -> Result<Http3Stream, Error> {
        if Origin::from_uri(request.uri())? != self.origin {
            return Err("HTTP/3 request authority differs from its connection".into());
        }
        let permit = self.permits.clone().acquire_owned().await?;
        let (send, recv) = self.requests.send_request(request).await?.split();
        Ok(Http3Stream {
            send,
            recv,
            _permit: permit,
            _owner: self.clone(),
        })
    }
}

pub struct Http3Stream {
    send: SendHalf,
    recv: RecvHalf,
    _permit: OwnedSemaphorePermit,
    _owner: Arc<Http3Client>,
}

impl Http3Stream {
    pub async fn send_data(&mut self, bytes: Bytes) -> Result<(), Error> {
        Ok(self.send.send_data(bytes).await?)
    }

    pub async fn finish(&mut self) -> Result<(), Error> {
        Ok(self.send.finish().await?)
    }

    pub async fn response(&mut self) -> Result<Response<()>, Error> {
        Ok(self.recv.response().await?)
    }

    pub async fn recv_data(&mut self) -> Result<Option<Bytes>, Error> {
        Ok(self.recv.data().await?)
    }
}

#[derive(PartialEq, Eq)]
pub(crate) struct Origin {
    host: String,
    port: u16,
}
impl Origin {
    pub(crate) fn from_uri(uri: &Uri) -> Result<Self, Error> {
        if uri.scheme_str() != Some("https") {
            return Err("HTTP/3 requires an absolute HTTPS URI".into());
        }
        let authority = uri.authority().ok_or("HTTP/3 URI has no authority")?;
        if authority.as_str().contains('@') {
            return Err("HTTP/3 URI must not contain credentials".into());
        }
        let suffix = &authority.as_str()[authority.host().len()..];
        let port = if suffix.is_empty() {
            443
        } else {
            uri.port_u16().ok_or("HTTP/3 URI has an invalid port")?
        };
        let host = uri
            .host()
            .ok_or("HTTP/3 URI has no hostname")?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        if host.is_empty() {
            return Err("HTTP/3 URI has no hostname".into());
        }
        Ok(Self { host, port })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http3::{Code, WtCode};
    #[test]
    fn request_authority_is_fixed_and_never_accepts_credentials() {
        let origin = Origin::from_uri(&"https://METER.example:443/path".parse().unwrap()).unwrap();
        assert!(origin == Origin::from_uri(&"https://meter.example/other".parse().unwrap()).unwrap());
        assert!(origin != Origin::from_uri(&"https://meter.example:8443/".parse().unwrap()).unwrap());
        for uri in [
            "/path",
            "http://meter.example/",
            "https://user:secret@meter.example/",
            "https://meter.example:65536/",
            "https://meter.example:invalid/",
        ] {
            assert!(Origin::from_uri(&uri.parse().unwrap()).is_err());
        }
    }

    #[test]
    fn quic_dials_ipv4_first_as_quic_go() {
        let [v6, v4, other]: [SocketAddr; 3] =
            ["[2001:db8::1]:443", "192.0.2.1:443", "[2001:db8::2]:443"].map(|address| address.parse().unwrap());
        assert_eq!(ipv4_first(vec![v6, v4, other]), [v4, v6, other]);
        assert_eq!(ipv4_first(vec![other, v6]), [other, v6]);
    }

    #[test]
    fn only_a_violation_found_here_ends_a_transfer() {
        let closed = |local, code| http3::Error::Connection {
            local,
            code,
            reason: Bytes::new(),
        };
        let retried: Vec<Error> = vec![
            Box::new(http3::Error::Reset(WtCode(42).to_http())),
            Box::new(http3::Error::Stopped(Code::WT_SESSION_GONE)),
            Box::new(http3::Error::Reset(Code(0x102))),
            Box::new(http3::Error::Refused),
            Box::new(closed(false, Code(0x102))),
            Box::new(closed(false, Code::H3_FRAME_ERROR)),
            Box::new(quinn::ConnectionError::TimedOut),
            Box::new(quinn::ConnectionError::VersionMismatch),
            Box::new(quinn::ConnectionError::LocallyClosed),
        ];
        for error in retried {
            assert!(!violation(error.as_ref()), "{error}");
        }
        let violations: Vec<Error> = vec![
            Box::new(http3::Error::Protocol(Code::H3_MESSAGE_ERROR)),
            Box::new(closed(true, Code::H3_FRAME_ERROR)),
        ];
        for error in violations {
            assert!(violation(error.as_ref()), "{error}");
        }
    }
}
