//! The running server: certificate, listeners and connections; shutdown closes listeners, ends lanes, drains.

use crate::{
    app::{App, Endpoint},
    auth::{COUNTERS, Security},
    config::{Config, ENGINE_VERSION, ListenAddress, ListenerKind, Methods, could_not},
    engine::download::BLOCK_BYTES,
    log,
    transport::{
        accept::{self, Listen},
        http1::Http1,
        http2::{self, Http2},
        quic::{self, Binding, Endpoints},
        tls::{self, Certificates},
    },
};
use futures_util::{StreamExt, future::OptionFuture, stream::FuturesUnordered};
use graphite_meter_net::Pool;
use std::{
    future::Future,
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};
use tokio::{
    net::TcpListener,
    runtime::Handle,
    time::{MissedTickBehavior, interval, timeout},
};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

pub struct Server {
    app: Arc<App>,
    pool: Pool,
    listeners: Vec<Listening>,
    certificates: Option<Arc<Certificates>>,
}

struct Listening {
    endpoint: Endpoint,
    address: ListenAddress,
    socket: Socket,
}

/// A listener's socket and what its connections speak, over TLS with the acceptor's ALPN.
enum Socket {
    Http1(TcpListener, Option<TlsAcceptor>),
    Http2(TcpListener, TlsAcceptor),
    Quic(Box<Endpoints>),
}

impl Socket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Http1(socket, _) | Self::Http2(socket, _) => socket.local_addr(),
            Self::Quic(quic) => quic.listeners[0].local_addr(),
        }
    }
}

type Accepting<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

impl Server {
    /// Loads the certificate and binds every listener, HTTP/3 UDP on its TCP port; a failure closes those bound.
    pub async fn bind(mut config: Config, pool: Pool) -> Result<Self, String> {
        log!(Info, "server", "graphite-meter {ENGINE_VERSION} starting");
        let others = match config.catalog.servers.len() - 1 {
            0 => "no other servers".to_owned(),
            1 => "1 other server".to_owned(),
            count => format!("{count} other servers"),
        };
        let location = Some(config.location()).filter(|location| !location.is_empty());
        let location = location
            .map(|location| format!(" ({location})"))
            .unwrap_or_default();
        log!(
            Info,
            "config",
            "serving as {}{location}; the server catalog lists {others}",
            config.name()
        );
        let (tls, hosts) = (config.tls.clone(), tls::covered_hosts(&config));
        let oidc_only = matches!(
            config.auth.as_ref().map(|auth| &auth.methods),
            Some(Methods { password: None, .. })
        );
        let terms = Terms::of(&config)?;
        let endpoint_bytes = Arc::new(AtomicUsize::new(configured_endpoint(&config)?));
        let endpoint = endpoint_bytes.clone();
        let fits = move |handshake| terms.check(handshake, endpoint.load(Ordering::Relaxed));
        let certificates = match tls {
            Some(files) => {
                let load = move || Certificates::load(files, hosts, SystemTime::now(), fits);
                let load = tokio::task::spawn_blocking(load);
                Some(Arc::new(load.await.map_err(|error| {
                    format!("certificate loading failed: {error}")
                })??))
            }
            None => None,
        };
        let mut listeners = bind_all(&mut config, certificates.as_ref()).await?;
        let app = Arc::new(App::new(config, CancellationToken::new())?);
        if oidc_only {
            let discovery = app.auth().discover();
            discovery
                .await
                .map_err(|error| format!("OIDC discovery failed: {error}"))?;
        }
        let companion = listeners
            .iter()
            .find(|listening| listening.endpoint == Endpoint::H3Companion);
        if let (Some(companion), Some(certificates)) = (companion, &certificates) {
            let fits = |bytes| terms.check(certificates.handshake_bytes(), bytes);
            let binding = Binding {
                app: &app,
                certificates,
                fits,
            };
            let local = companion.socket.local_addr();
            let local =
                local.map_err(|error| format!("could not read the HTTP/3 port: {error}"))?;
            let bound = binding.bind(local, &pool)?;
            endpoint_bytes.store(bound.bytes, Ordering::Relaxed);
            let (endpoint, address) = (Endpoint::Quic, companion.address.clone());
            listeners.push(Listening {
                endpoint,
                address,
                socket: Socket::Quic(Box::new(bound)),
            });
        }
        Ok(Self {
            app,
            pool,
            listeners,
            certificates,
        })
    }

    pub fn local_addr(&self, endpoint: Endpoint) -> Option<SocketAddr> {
        let listening = self
            .listeners
            .iter()
            .find(|listening| listening.endpoint == endpoint)?;
        listening.socket.local_addr().ok()
    }

    pub fn app(&self) -> Arc<App> {
        self.app.clone()
    }

    /// Serves until `stop`, then closes every listener at once, ends running lanes and drains connections.
    pub async fn serve(self, stop: impl Future<Output = ()>) -> Result<(), String> {
        let Self {
            app,
            pool,
            listeners,
            certificates,
        } = self;
        let shutdown = app.shutdown().clone();
        let (pool, stopping, auth) = (&pool, &shutdown, app.auth().enabled());
        let mut accepting = FuturesUnordered::<Accepting<'_>>::new();
        for Listening {
            endpoint,
            address,
            socket,
        } in listeners
        {
            let role = endpoint.protocol();
            let (app, next) = (app.clone(), || pool.next());
            let protocol = match socket {
                Socket::Http1(socket, tls) => {
                    let http1 = Http1 {
                        app: app.clone(),
                        endpoint,
                        tls,
                        buses: Handle::current(),
                    };
                    let serve = move |socket, peer| http1.connection(socket, peer);
                    accepting.push(listen(app, socket, next, stopping, role, serve));
                    "tcp"
                }
                Socket::Http2(socket, tls) => {
                    let http2 = Http2 {
                        app: app.clone(),
                        tls,
                    };
                    let serve = move |socket, peer| http2.connection(socket, peer);
                    accepting.push(listen(app, socket, next, stopping, role, serve));
                    "tcp"
                }
                Socket::Quic(quic) => {
                    // Each endpoint accepts, answers Retry and runs its connections on its own runtime.
                    for listener in quic.listeners {
                        let (runtime, http3) = (listener.runtime(), quic.http3.clone());
                        let (app, stopping, next) =
                            (app.clone(), shutdown.clone(), runtime.clone());
                        let logged = app.clone();
                        let serve = move |incoming, peer: SocketAddr| {
                            let accepted = logged.quic_accepted(peer);
                            let serving = http3.connection(incoming, peer);
                            async move {
                                let _accepted = accepted;
                                serving.await;
                            }
                        };
                        let serving = runtime.spawn(async move {
                            let next = move || next.clone();
                            listen(app, listener, next, &stopping, role, serve).await
                        });
                        accepting.push(Box::pin(async move {
                            serving.await.map_err(|error| format!("{role}: {error}"))?
                        }));
                    }
                    "udp"
                }
            };
            let place = format!("{address}/{protocol}");
            log!(
                Info,
                "listen",
                "{place:<21} {role:<9} {}",
                endpoint.roles(auth)
            );
        }
        let tasks = async {
            tokio::join!(
                OptionFuture::from(certificates.map(Certificates::watch)),
                release_when_idle(&app, || release_memory(pool)),
                OptionFuture::from(app.auth().security().map(log_security)),
                OptionFuture::from(app.auth().background_discovery()),
                OptionFuture::from(app.config().verbose.then(|| log_verbose(&app))),
            )
        };
        let background = async {
            tokio::select! {
                () = stopping.cancelled() => {}
                _ = tasks => {}
            }
        };
        let serving = async {
            log!(Info, "server", "ready");
            let mut result = tokio::select! {
                () = stop => Ok(()),
                Some(ended) = accepting.next() => {
                    ended.and(Err("a listener stopped accepting; stopping the server".into()))
                }
            };
            log!(
                Info,
                "server",
                "stop requested; closing listeners and draining connections"
            );
            shutdown.cancel();
            while let Some(ended) = accepting.next().await {
                result = result.and(ended);
            }
            result
        };
        let (result, ()) = tokio::join!(serving, background);
        if result.is_ok() {
            log!(Info, "server", "stopped");
        }
        result
    }
}

/// Accepts on `socket` until `stopping`, each connection holding its share on a runtime `runtime` names, then drains.
fn listen<'a, L, F>(
    app: Arc<App>,
    socket: L,
    runtime: impl Fn() -> Handle + Send + 'a,
    stopping: &'a CancellationToken,
    role: &'static str,
    serve: impl Fn(L::Connection, SocketAddr) -> F + Send + 'a,
) -> Accepting<'a>
where
    L: Listen + Send + 'a,
    F: Future<Output = ()> + Send + 'static,
{
    Box::pin(async move {
        let hold = |peer: SocketAddr| app.connection(peer.ip());
        let serving = accept::serve(socket, runtime, stopping, hold, serve);
        serving.await.map_err(|error| format!("{role}: {error}"))
    })
}

/// Completes at SIGINT or SIGTERM, listening from its call, so a signal during startup stops the server once it serves.
pub fn stop_signal() -> io::Result<impl Future<Output = ()>> {
    cfg_select! {
        unix => {
            use tokio::signal::unix::{SignalKind, signal};
            let (mut interrupt, mut terminate) = (signal(SignalKind::interrupt())?, signal(SignalKind::terminate())?);
            Ok(async move {
                tokio::select! {
                    _ = interrupt.recv() => {}
                    _ = terminate.recv() => {}
                }
            })
        }
        _ => Ok(async {
            let _ = tokio::signal::ctrl_c().await;
        }),
    }
}

/// The server returns freed memory once it has had no connection for this long.
const IDLE_RELEASE: Duration = Duration::from_secs(2);
/// Verbose logs report transfer rates every second and the admission counters every thirty.
const TRANSFER_LOG: Duration = Duration::from_secs(1);
const ADMISSION_LOG: Duration = Duration::from_secs(30);
const SECURITY_LOG: Duration = Duration::from_mins(1);

/// Calls `release` once per idle period, `IDLE_RELEASE` after the last connection closed.
async fn release_when_idle(app: &App, release: impl Fn()) {
    loop {
        app.quotas().idle().await;
        // Each close that empties the server again restarts the wait.
        while timeout(IDLE_RELEASE, app.quotas().idle()).await.is_ok() {}
        if app.quotas().connections() == 0 {
            release();
        }
    }
}

/// Returns freed pages to the OS from every pool thread's heap: mimalloc does so only while freeing threads allocate.
fn release_memory(pool: &Pool) {
    collect();
    for runtime in pool.runtimes() {
        runtime.spawn(async { collect() });
    }
}

fn collect() {
    #[cfg(target_env = "musl")]
    rustfs_mimalloc::heap::Heap::main().collect(true);
}

async fn log_security(security: &Security) {
    let (mut ticks, mut last) = (interval(SECURITY_LOG), [0; COUNTERS]);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticks.tick().await;
    loop {
        ticks.tick().await;
        if let Some(line) = security.line(&mut last) {
            log!(Info, "auth", "{line}");
        }
    }
}

async fn log_verbose(app: &App) {
    let (mut transfers, mut admission) = (interval(TRANSFER_LOG), interval(ADMISSION_LOG));
    transfers.set_missed_tick_behavior(MissedTickBehavior::Skip);
    admission.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last = transfers.tick().await;
    admission.tick().await;
    loop {
        tokio::select! {
            now = transfers.tick() => {
                for (direction, line) in app.transfer_lines(now - last) {
                    log!(Info, direction, "{line}");
                }
                last = now;
            }
            _ = admission.tick() => log!(Info, "admission", "{}", app.quotas().admission()),
        }
    }
}

/// Binds every enabled listener's TCP socket, HTTP/3's for its companion, and names the ports they bound in `config`.
async fn bind_all(
    config: &mut Config,
    certificates: Option<&Arc<Certificates>>,
) -> Result<Vec<Listening>, String> {
    let mut listeners = Vec::new();
    for listener in &mut config.listeners {
        let acceptor = |alpn: &[u8]| {
            certificates
                .expect("validation requires TLS files")
                .acceptor(alpn)
        };
        let (setting, address) = (listener.kind.env(), &mut listener.address);
        let socket = bind(address).await.map_err(|error| {
            let failed = could_not(&format!("listen on {address}/tcp ({setting}_ADDR)"), &error);
            match error.kind() {
                io::ErrorKind::PermissionDenied if (1..1024).contains(&address.port) => format!(
                    "{failed}; use a port from 1024, or allow lower ones with CAP_NET_BIND_SERVICE or \
                     net.ipv4.ip_unprivileged_port_start"
                ),
                _ => failed,
            }
        })?;
        if let Ok(local) = socket.local_addr() {
            address.port = local.port();
        }
        let (endpoint, socket) = match listener.kind {
            ListenerKind::H1 => (Endpoint::H1, Socket::Http1(socket, None)),
            ListenerKind::H1Tls => (
                Endpoint::H1Tls,
                Socket::Http1(socket, Some(acceptor(b"http/1.1"))),
            ),
            ListenerKind::H2 => (Endpoint::H2, Socket::Http2(socket, acceptor(b"h2"))),
            ListenerKind::H3 => (
                Endpoint::H3Companion,
                Socket::Http1(socket, Some(acceptor(b"http/1.1"))),
            ),
        };
        listeners.push(Listening {
            endpoint,
            address: address.clone(),
            socket,
        });
    }
    Ok(listeners)
}

/// Binds `address`; without a host it takes every IPv6 and IPv4 address, or every IPv4 one on a host without IPv6.
async fn bind(address: &ListenAddress) -> io::Result<TcpListener> {
    let port = address.port;
    if address.host.is_some() {
        return TcpListener::bind(address.to_string()).await;
    }
    let ipv4 = || TcpListener::bind((Ipv4Addr::UNSPECIFIED, port));
    let dual = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::STREAM, None).and_then(
        |socket| {
            socket.set_only_v6(false)?;
            Ok(socket)
        },
    );
    let Ok(socket) = dual else {
        return ipv4().await;
    };
    #[cfg(unix)]
    socket.set_reuse_address(true)?;
    match socket.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)).into()) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::AddrNotAvailable | io::ErrorKind::Unsupported
            ) =>
        {
            ipv4().await
        }
        Err(error) => Err(error),
        Ok(()) => {
            socket.listen(1024)?;
            socket.set_nonblocking(true)?;
            TcpListener::from_std(socket.into())
        }
    }
}

/// Refuses a buffer budget below every connection's floor, the QUIC endpoint's pre-socket buffers and downloads.
pub fn check_budget(config: &Config) -> Result<(), String> {
    Terms::of(config)?.check(0, configured_endpoint(config)?)
}

/// The QUIC endpoint's buffers without its socket's, or none without HTTP/3.
fn configured_endpoint(config: &Config) -> Result<usize, String> {
    match config.listener(ListenerKind::H3) {
        Some(_) => quic::endpoint_bytes(
            &noq::EndpointConfig::default(),
            1,
            config.limits.connections.get(),
            0,
            1,
        )
        .ok_or_else(|| quic::OVERFLOW.into()),
        None => Ok(0),
    }
}

/// What the buffer budget must cover: every connection's floor, the QUIC endpoint's buffers and the download block.
#[derive(Debug, Clone, Copy)]
struct Terms {
    limit: usize,
    connections: usize,
    h2: bool,
    /// noq's own floor per connection, with HTTP/3 enabled.
    quic: Option<usize>,
}

impl Terms {
    fn of(config: &Config) -> Result<Self, String> {
        let h3 = config.listener(ListenerKind::H3);
        Ok(Self {
            limit: config.max_buffer_bytes,
            connections: config.limits.connections.get(),
            h2: config.listener(ListenerKind::H2).is_some(),
            quic: h3.map(|_| quic::noq_floor(&config.limits)).transpose()?,
        })
    }

    /// Refuses a budget that a `handshake`-byte QUIC handshake and `endpoint`-byte endpoint buffers leave short.
    fn check(&self, handshake: usize, endpoint: usize) -> Result<(), String> {
        let quic = self
            .quic
            .map_or(0, |noq| quic::floor_bytes(handshake).saturating_add(noq));
        let floor = quic.max(if self.h2 { http2::FLOOR_BYTES } else { 0 });
        let (limit, connections) = (self.limit, self.connections);
        let minimum = floor as u128 * connections as u128 + endpoint as u128 + BLOCK_BYTES as u128;
        if minimum > limit as u128 {
            // The kernel may double endpoint buffers and handshakes grow floors, so the advice keeps a tenth spare.
            const GIB: u128 = 1 << 30;
            let fits = (limit as u128).saturating_sub(endpoint as u128 + BLOCK_BYTES as u128)
                / (floor as u128).max(1);
            let fits = match fits * 9 / 10 {
                spare @ 32.. => spare / 32 * 32,
                spare => spare.max(1),
            };
            let raise = (minimum * 11 / 10).div_ceil(GIB) * GIB;
            return Err(format!(
                "GM_MAX_BUFFER_BYTES {limit} is short of {minimum} for {connections} connections\n\
                 per connection {}, QUIC endpoints {}, download block {}\n\
                 raise GM_MAX_BUFFER_BYTES to {raise} or lower GM_MAX_CONNECTIONS to {fits}",
                binary(floor as u128),
                binary(endpoint as u128),
                binary(BLOCK_BYTES as u128),
            ));
        }
        Ok(())
    }
}

fn binary(bytes: u128) -> String {
    let (mut value, mut unit) = (bytes as f64, "B");
    for next in ["KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 {
            break;
        }
        (value, unit) = (value / 1024.0, next);
    }
    match unit {
        "B" => format!("{bytes} B"),
        _ if value.fract() == 0.0 => format!("{value} {unit}"),
        _ => format!("{value:.1} {unit}"),
    }
}

#[cfg(test)]
mod background_tests {
    use super::*;
    use crate::config::{self, Loaded};
    use std::ffi::OsString;
    use tokio::time::advance;

    fn app() -> App {
        let none = |_: &str| None::<OsString>;
        let Ok(Loaded::Config(config)) =
            config::load(none, Vec::<OsString>::new(), &mut Vec::new())
        else {
            panic!("the default configuration loads");
        };
        App::new(*config, CancellationToken::new()).unwrap()
    }

    /// Lets the release loop run up to its next wait.
    async fn settle() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn freed_memory_is_released_two_seconds_after_the_last_connection_closes() {
        let app = app();
        let releases = AtomicUsize::new(0);
        let peer = "192.0.2.1".parse().unwrap();
        let first = app.connection(peer).unwrap();
        let second = app.connection(peer).unwrap();
        let released = || releases.load(Ordering::Relaxed);
        let releasing = release_when_idle(&app, || {
            releases.fetch_add(1, Ordering::Relaxed);
        });
        let test = async {
            drop(first);
            settle().await;
            advance(IDLE_RELEASE * 2).await;
            settle().await;
            assert_eq!(released(), 0, "a connection remains");
            drop(second);
            settle().await;
            advance(IDLE_RELEASE - Duration::from_millis(1)).await;
            settle().await;
            assert_eq!(released(), 0);
            advance(Duration::from_millis(1)).await;
            settle().await;
            assert_eq!(
                released(),
                1,
                "two seconds after the last connection closed"
            );

            let again = app.connection(peer).unwrap();
            advance(IDLE_RELEASE * 3).await;
            settle().await;
            assert_eq!(
                released(),
                1,
                "nothing is released while a connection is open"
            );
            drop(again);
            settle().await;
            advance(IDLE_RELEASE).await;
            settle().await;
            assert_eq!(released(), 2);
            advance(IDLE_RELEASE * 3).await;
            settle().await;
            assert_eq!(released(), 2, "once per idle period");

            for _ in 0..4 {
                drop(app.connection(peer).unwrap());
                settle().await;
                advance(IDLE_RELEASE - Duration::from_millis(500)).await;
                settle().await;
            }
            assert_eq!(
                released(),
                2,
                "short connections every 1.5 s release nothing"
            );
            advance(Duration::from_millis(500)).await;
            settle().await;
            assert_eq!(released(), 3, "two seconds after the last of them");
        };
        tokio::select! {
            () = releasing => unreachable!("the release loop runs until the server stops"),
            () = test => {}
        }
    }
}

#[cfg(test)]
mod terms_tests {
    use super::*;
    use crate::config::{self, Loaded};
    use std::ffi::OsString;

    fn config(env: &[(&str, &str)]) -> Config {
        let lookup = |name: &str| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.into())
        };
        match config::load(lookup, Vec::<OsString>::new(), &mut Vec::new()) {
            Ok(Loaded::Config(config)) => *config,
            other => panic!("{other:?}"),
        }
    }

    const TLS: [(&str, &str); 2] = [("GM_TLS_CERT", "/cert.pem"), ("GM_TLS_KEY", "/key.pem")];

    #[test]
    fn per_client_stream_budgets_must_fit_a_quic_stream_count() {
        let max = i64::MAX.to_string();
        let huge = [
            ("GM_H3_ADDR", ":7249"),
            ("GM_MAX_ACTIVE_MEASUREMENTS", &max),
            ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", &max),
            ("GM_MAX_ACTIVE_SESSIONS", &max),
            ("GM_MAX_SESSIONS_PER_CLIENT", &max),
        ];
        let refused = check_budget(&config(&[&TLS[..], &huge[..]].concat()));
        assert_eq!(
            refused,
            Err("per-client stream budgets exceed the QUIC stream limit".into())
        );
        assert_eq!(
            check_budget(&config(&huge[1..])),
            Ok(()),
            "only HTTP/3 counts streams"
        );
    }

    #[test]
    fn http3_adds_its_connection_floor_handshake_and_endpoint_buffers() {
        let env = [
            ("GM_H3_ADDR", ":7249"),
            ("GM_MAX_CONNECTIONS", "2"),
            ("GM_MAX_CONNECTIONS_PER_CLIENT", "2"),
        ];
        let config = config(&[&TLS[..], &env[..]].concat());
        let endpoint = configured_endpoint(&config).unwrap();
        let floor = quic::floor_bytes(0) + quic::noq_floor(&config.limits).unwrap();
        let terms = Terms {
            limit: 2 * floor + endpoint + BLOCK_BYTES,
            ..Terms::of(&config).unwrap()
        };
        assert_eq!(terms.check(0, endpoint), Ok(()));
        let refused = terms.check(1, endpoint).unwrap_err();
        let lines: Vec<&str> = refused.lines().collect();
        assert_eq!(lines.len(), 3, "{refused}");
        assert_eq!(
            lines[0],
            format!(
                "GM_MAX_BUFFER_BYTES {} is short of {} for 2 connections",
                terms.limit,
                terms.limit + 2
            )
        );
        assert!(
            lines[2] == "raise GM_MAX_BUFFER_BYTES to 1073741824 or lower GM_MAX_CONNECTIONS to 1",
            "a handshake byte more on each connection: {refused}"
        );
        assert!(
            terms.check(0, endpoint + 1).is_err(),
            "the endpoint's socket buffers count"
        );
    }

    #[test]
    fn noqs_stream_floor_at_per_client_limits_equal_to_the_totals() {
        let totals = [
            ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "256"),
            ("GM_MAX_SESSIONS_PER_CLIENT", "64"),
        ];
        let floor = quic::noq_floor(&config(&totals).limits).unwrap();
        assert_eq!(
            floor >> 10,
            984,
            "noq's stream floor for 324 request streams, without HTTP/3's state"
        );
    }
}
