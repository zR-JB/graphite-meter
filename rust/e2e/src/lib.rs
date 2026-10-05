//! End-to-end tests of the client against the in-process server.
use graphite_meter_proto::origin::Origin;
use graphite_meter_server::{
    app::Endpoint,
    config::{self, Loaded},
    runtime,
};
use graphite_meter_testkit::{Identity, Scratch};
use std::{ffi::OsString, net::SocketAddr};
use tokio::sync::oneshot;

/// One loopback address spelled three ways, as listener addresses must differ: discovery advertises every listener
/// on the host it was asked at.
const ADDRESSES: [&str; 3] = ["127.0.0.7:0", "127.0.7:0", "127.7:0"];

/// The server on local ports, serving cleartext HTTP/1.1, HTTP/2 over TLS and HTTP/3 until dropped.
pub struct Server {
    pub http1: Origin,
    pub http2: Origin,
    pub http3: Origin,
    pub quic: SocketAddr,
    _stop: oneshot::Sender<()>,
    _scratch: Scratch,
}

impl Server {
    pub async fn start() -> Self {
        Self::with(&[]).await
    }

    /// The server with `settings` over the test configuration.
    pub async fn with(settings: &[(&str, &str)]) -> Self {
        let (scratch, identity) = (Scratch::new().unwrap(), Identity::generate().unwrap());
        let certificate = scratch.file("cert.pem", &identity.certificate).unwrap();
        let key = scratch.file("key.pem", &identity.key).unwrap();
        let mut env: Vec<(&str, OsString)> = settings.iter().map(|&(name, value)| (name, value.into())).collect();
        env.extend([
            ("GM_H1_ADDR", ADDRESSES[0].into()),
            ("GM_H2_ADDR", ADDRESSES[1].into()),
            ("GM_H3_ADDR", ADDRESSES[2].into()),
            ("GM_TLS_CERT", certificate.into()),
            ("GM_TLS_KEY", key.into()),
        ]);
        let lookup = |name: &str| env.iter().find(|(set, _)| *set == name).map(|(_, value)| value.clone());
        let loaded = config::load(lookup, Vec::<OsString>::new(), &mut Vec::new());
        let Ok(Loaded::Config(config)) = loaded else {
            panic!("the test configuration loads: {:?}", loaded.err());
        };
        let server = runtime::Server::bind(*config).await.unwrap();
        let address = |endpoint| server.local_addr(endpoint).unwrap();
        let origin = |scheme, endpoint| Origin::parse(&format!("{scheme}://{}", address(endpoint))).unwrap();
        let (http1, http2, http3) = (
            origin("http", Endpoint::H1),
            origin("https", Endpoint::H2),
            origin("https", Endpoint::Quic),
        );
        let quic = address(Endpoint::Quic);
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(server.serve(async {
            let _ = stopped.await;
        }));
        Self { http1, http2, http3, quic, _stop: stop, _scratch: scratch }
    }
}
