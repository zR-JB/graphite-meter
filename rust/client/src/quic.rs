//! HTTP/3 requests over the pinned Noq transport, with an explicitly owned driver.
use std::{net::SocketAddr, sync::Arc, time::Duration};

use crate::tls::config as tls_config;
use bytes::{Buf, Bytes};
use h3::error::Code;
use http::{Request, Response, Uri};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
    time::{Instant, timeout, timeout_at},
};

use crate::Error;

type Sender = h3::client::SendRequest<h3_noq::OpenStreams, Bytes>;
type Stream = h3::client::RequestStream<h3_noq::BidiStream<Bytes>, Bytes>;
const MAX_REQUESTS: usize = 256;

#[derive(Clone, Copy, Debug)]
pub struct RequestLimits {
    pub timeout: Duration,
    pub max_send_bytes: u64,
    pub max_receive_bytes: u64,
}

impl Default for RequestLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_send_bytes: 1024 * 1024,
            max_receive_bytes: 1024 * 1024,
        }
    }
}

/// Holds the sole driver task. Request streams borrow this owner, so connection
/// work cannot outlive it. Requests never follow redirects or change authority.
pub struct Http3Client {
    endpoint: EndpointOwner,
    connection: quinn::Connection,
    sender: Option<Sender>,
    driver: JoinSet<()>,
    permits: Arc<Semaphore>,
    origin: Origin,
}

/// Close even when cancellation occurs before QUIC or HTTP/3 setup finishes.
struct EndpointOwner(quinn::Endpoint);
impl Drop for EndpointOwner {
    fn drop(&mut self) {
        self.0.close(0_u32.into(), b"client endpoint dropped");
    }
}

impl Http3Client {
    pub async fn connect(uri: &Uri, insecure: bool, deadline: Duration) -> Result<Self, Error> {
        let origin = Origin::from_uri(uri)?;
        timeout(deadline, Self::connect_inner(origin, insecure)).await?
    }

    async fn connect_inner(origin: Origin, insecure: bool) -> Result<Self, Error> {
        let tls = tls_config(insecure)?;
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)?;
        let mut config = quinn::ClientConfig::new(Arc::new(crypto));
        let mut transport = quinn::TransportConfig::default();
        transport.max_concurrent_bidi_streams(0_u32.into());
        transport.max_concurrent_uni_streams(16_u32.into());
        transport.stream_receive_window((1024 * 1024_u32).into());
        transport.receive_window((4 * 1024 * 1024_u32).into());
        transport.send_window(4 * 1024 * 1024);
        transport.max_idle_timeout(Some(Duration::from_secs(60).try_into()?));
        config.transport_config(Arc::new(transport));

        let addresses: Vec<_> = tokio::net::lookup_host((origin.host.as_str(), origin.port))
            .await?
            .collect();
        let mut last_error: Option<Error> = None;
        for address in addresses {
            let bind: SocketAddr = if address.is_ipv6() {
                "[::]:0"
            } else {
                "0.0.0.0:0"
            }
            .parse()?;
            let endpoint = EndpointOwner(quinn::Endpoint::client(bind)?);
            let connecting = endpoint
                .0
                .connect_with(config.clone(), address, &origin.host)?;
            // A silent address must not consume the whole multi-address attempt.
            let connection = match timeout(Duration::from_secs(3), connecting).await {
                Ok(Ok(connection)) => connection,
                Ok(Err(error)) => {
                    last_error = Some(error.into());
                    continue;
                }
                Err(error) => {
                    last_error = Some(error.into());
                    continue;
                }
            };
            let mut owner = Self {
                endpoint,
                connection,
                sender: None,
                driver: JoinSet::new(),
                permits: Arc::new(Semaphore::new(MAX_REQUESTS)),
                origin,
            };
            // Construct the owner before HTTP/3 setup can suspend, ensuring that
            // cancellation closes QUIC even when peer stream credit is withheld.
            let (mut driver, sender) = h3::client::builder()
                .max_field_section_size(32 * 1024)
                .build(h3_noq::Connection::new(owner.connection.clone()))
                .await?;
            owner.sender = Some(sender);
            owner.driver.spawn(async move {
                let _ = driver.wait_idle().await;
            });
            return Ok(owner);
        }
        Err(last_error.unwrap_or_else(|| "HTTP/3 hostname resolved to no addresses".into()))
    }

    pub async fn open(
        &self,
        request: Request<()>,
        limits: RequestLimits,
    ) -> Result<Http3Stream<'_>, Error> {
        if Origin::from_uri(request.uri())? != self.origin {
            return Err("HTTP/3 request authority differs from its connection".into());
        }
        let deadline = Instant::now()
            .checked_add(limits.timeout)
            .ok_or("HTTP/3 timeout is too large")?;
        let permit = timeout_at(deadline, self.permits.clone().acquire_owned()).await??;
        let mut sender = self
            .sender
            .as_ref()
            .expect("connected client has sender")
            .clone();
        let stream = timeout_at(deadline, sender.send_request(request)).await??;
        Ok(Http3Stream {
            stream,
            deadline,
            limits,
            sent: 0,
            received: 0,
            send_finished: false,
            receive_finished: false,
            response_received: false,
            _permit: permit,
            _owner: self,
        })
    }

    pub async fn close(mut self) {
        self.connection.close(0_u32.into(), b"client complete");
        self.endpoint.0.close(0_u32.into(), b"client complete");
        self.driver.shutdown().await;
        let _ = timeout(Duration::from_secs(1), self.endpoint.0.wait_idle()).await;
    }
}

impl Drop for Http3Client {
    fn drop(&mut self) {
        self.connection.close(0_u32.into(), b"client dropped");
        self.endpoint.0.close(0_u32.into(), b"client dropped");
        self.driver.abort_all();
    }
}

pub struct Http3Stream<'a> {
    stream: Stream,
    deadline: Instant,
    limits: RequestLimits,
    sent: u64,
    received: u64,
    send_finished: bool,
    receive_finished: bool,
    response_received: bool,
    _permit: OwnedSemaphorePermit,
    _owner: &'a Http3Client,
}

impl Http3Stream<'_> {
    pub async fn send_data(&mut self, bytes: Bytes) -> Result<(), Error> {
        if self.send_finished {
            return Err("HTTP/3 request body already finished".into());
        }
        self.sent = self
            .sent
            .checked_add(bytes.len() as u64)
            .filter(|&size| size <= self.limits.max_send_bytes)
            .ok_or("HTTP/3 request body exceeds limit")?;
        timeout_at(self.deadline, self.stream.send_data(bytes)).await??;
        Ok(())
    }

    pub async fn finish(&mut self) -> Result<(), Error> {
        timeout_at(self.deadline, self.stream.finish()).await??;
        self.send_finished = true;
        Ok(())
    }

    pub async fn response(&mut self) -> Result<Response<()>, Error> {
        if self.response_received {
            return Err("HTTP/3 response headers already received".into());
        }
        let response = timeout_at(self.deadline, self.stream.recv_response()).await??;
        self.response_received = true;
        Ok(response)
    }

    pub async fn recv_data(&mut self) -> Result<Option<Bytes>, Error> {
        if !self.response_received {
            return Err("receive HTTP/3 response headers before its body".into());
        }
        let Some(mut data) = timeout_at(self.deadline, self.stream.recv_data()).await?? else {
            self.receive_finished = true;
            return Ok(None);
        };
        self.received = self
            .received
            .checked_add(data.remaining() as u64)
            .filter(|&size| size <= self.limits.max_receive_bytes)
            .ok_or("HTTP/3 response body exceeds limit")?;
        Ok(Some(data.copy_to_bytes(data.remaining())))
    }

    pub async fn recv_body(&mut self) -> Result<Bytes, Error> {
        let mut body = Vec::new();
        while let Some(chunk) = self.recv_data().await? {
            body.try_reserve(chunk.len())?;
            body.extend_from_slice(&chunk);
        }
        Ok(body.into())
    }
}

impl Drop for Http3Stream<'_> {
    fn drop(&mut self) {
        if !self.send_finished {
            self.stream.stop_stream(Code::H3_REQUEST_CANCELLED);
        }
        if !self.receive_finished {
            self.stream.stop_sending(Code::H3_REQUEST_CANCELLED);
        }
    }
}

#[derive(PartialEq, Eq)]
struct Origin {
    host: String,
    port: u16,
}
impl Origin {
    fn from_uri(uri: &Uri) -> Result<Self, Error> {
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
    use rustls::pki_types::CertificateDer;
    #[test]
    fn request_authority_is_fixed_and_never_accepts_credentials() {
        let origin = Origin::from_uri(&"https://METER.example:443/path".parse().unwrap()).unwrap();
        assert!(
            origin == Origin::from_uri(&"https://meter.example/other".parse().unwrap()).unwrap()
        );
        assert!(
            origin != Origin::from_uri(&"https://meter.example:8443/".parse().unwrap()).unwrap()
        );
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
    fn ipv6_authority_keeps_its_port() {
        let origin = Origin::from_uri(&"https://[::1]:8443/".parse().unwrap()).unwrap();
        assert_eq!(origin.host, "::1");
        assert_eq!(origin.port, 8443);
        assert_eq!(
            Origin::from_uri(&"https://[::1]/".parse().unwrap())
                .unwrap()
                .port,
            443
        );
    }
    #[test]
    fn insecure_tls_configuration_does_not_enable_legacy_protocols() {
        let tls = tls_config(true).unwrap();
        assert_eq!(tls.alpn_protocols, [b"h3".to_vec()]);
        assert!(!tls.enable_early_data);
    }
    #[tokio::test]
    async fn native_streaming_and_body_limits() -> Result<(), Error> {
        use rustls::pki_types::{PrivateKeyDer, pem::PemObject};
        struct Identity(std::path::PathBuf);
        impl Drop for Identity {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let mut nonce = [0; 16];
        rustls::crypto::ring::default_provider()
            .secure_random
            .fill(&mut nonce)
            .map_err(|_| "test randomness unavailable")?;
        let name: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let identity = Identity(std::env::temp_dir().join(format!("gm-h3-client-{name}")));
        std::fs::create_dir(&identity.0)?;
        let cert_path = identity.0.join("cert.pem");
        let key_path = identity.0.join("key.pem");
        let output = std::process::Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost",
                "-keyout",
            ])
            .arg(&key_path)
            .arg("-out")
            .arg(&cert_path)
            .output()?;
        if !output.status.success() {
            return Err("test certificate generation failed".into());
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_file(cert_path)?],
                PrivateKeyDer::from_pem_file(key_path)?,
            )?;
        tls.alpn_protocols = vec![b"h3".to_vec()];
        let config = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls)?,
        ));
        let server = quinn::Endpoint::server(config, "127.0.0.1:0".parse()?)?;
        let uri: Uri = format!("https://{}/echo", server.local_addr()?).parse()?;
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            // First connection rejects this untrusted certificate; the second opts in.
            let mut rejected = JoinSet::new();
            let incoming = server.accept().await.unwrap();
            rejected.spawn(async move { incoming.await });
            let connection = server.accept().await.unwrap().await?;
            let mut h3 = h3::server::builder()
                .build::<_, Bytes>(h3_noq::Connection::new(connection))
                .await?;
            for _ in 0..2 {
                let (request, mut stream) = h3
                    .accept()
                    .await?
                    .ok_or("missing request")?
                    .resolve_request()
                    .await?;
                assert_eq!(request.method(), http::Method::POST);
                let mut body = Vec::new();
                while let Some(mut chunk) = stream.recv_data().await? {
                    body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
                }
                assert_eq!(body, b"native upload");
                stream
                    .send_response(Response::builder().status(200).body(())?)
                    .await?;
                stream.send_data(Bytes::from(body)).await?;
                stream.finish().await?;
            }
            // Keep driving HTTP/3 until the client explicitly closes its owner.
            let _ = h3.accept().await;
            Ok::<_, Error>(())
        });
        assert!(
            Http3Client::connect(&uri, false, Duration::from_secs(5))
                .await
                .is_err()
        );
        let client = Http3Client::connect(&uri, true, Duration::from_secs(5)).await?;
        for max_receive_bytes in [100, 3] {
            let request = Request::post(uri.clone()).body(())?;
            let mut stream = client
                .open(
                    request,
                    RequestLimits {
                        max_receive_bytes,
                        ..RequestLimits::default()
                    },
                )
                .await?;
            stream.send_data(Bytes::from_static(b"native ")).await?;
            stream.send_data(Bytes::from_static(b"upload")).await?;
            stream.finish().await?;
            assert_eq!(stream.response().await?.status(), 200);
            let result = stream.recv_body().await;
            if max_receive_bytes == 100 {
                assert_eq!(result?, b"native upload"[..]);
            } else {
                assert!(result.unwrap_err().to_string().contains("exceeds limit"));
            }
        }
        client.close().await;
        timeout(Duration::from_secs(5), tasks.join_next())
            .await?
            .ok_or("missing server task")???;
        Ok(())
    }
}
