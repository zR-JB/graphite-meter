//! Loopback peers for the layer tests: our client against our server, and raw peers that send what a
//! conforming peer never would. Deadlines run on tokio's paused clock.
#![allow(dead_code)]

use graphite_meter_http3::{Code, Error, RequestStream, client, server, webtransport::Session};
use graphite_meter_testkit::Identity;
use std::{
    future::Future,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};
use tokio::{
    sync::{Notify, mpsc},
    task::JoinHandle,
};

pub type TestError = Box<dyn std::error::Error + Send + Sync>;
pub type Task = JoinHandle<Result<(), Error>>;

/// The application budget the server's layer charges, with a limit tests move.
#[derive(Debug)]
pub struct Budget {
    pub used: AtomicUsize,
    pub limit: AtomicUsize,
}

impl noq::SharedBudget for Budget {
    fn try_charge(&self, bytes: usize) -> bool {
        let fits = |used: usize| {
            used.checked_add(bytes)
                .filter(|&used| used <= self.limit.load(Ordering::Relaxed))
        };
        self.used.try_update(Ordering::Relaxed, Ordering::Relaxed, fits).is_ok()
    }

    fn refund(&self, bytes: usize) {
        self.used.fetch_sub(bytes, Ordering::Relaxed);
    }
}

/// How the peers connect: the server budget's limit; the client's receive windows, unidirectional stream
/// limit and reliable reset offer; and the SETTINGS a raw client sends, if it is raw.
#[derive(Clone, Copy)]
pub struct Setup {
    pub limit: usize,
    pub window: Option<u32>,
    pub connection_window: Option<u32>,
    pub uni_streams: Option<u32>,
    pub reliable_reset: bool,
    pub settings: Option<&'static [(u64, u64)]>,
}

pub const PLAIN: Setup = Setup {
    limit: usize::MAX,
    window: None,
    connection_window: None,
    uni_streams: None,
    reliable_reset: true,
    settings: None,
};

pub const fn raw(settings: &'static [(u64, u64)]) -> Setup {
    Setup { settings: Some(settings), ..PLAIN }
}

pub struct Peers {
    pub server: noq::Connection,
    pub client: noq::Connection,
    pub budget: Arc<Budget>,
    _held: (Option<noq::SendStream>, [noq::Endpoint; 2]),
}

fn identity() -> &'static Identity {
    static IDENTITY: OnceLock<Identity> = OnceLock::new();
    IDENTITY.get_or_init(|| Identity::generate().expect("openssl"))
}

fn transport(setup: &Setup) -> Arc<noq::TransportConfig> {
    let mut transport = noq::TransportConfig::default();
    // Clock jumps stay inside the idle timeout.
    transport.max_idle_timeout(Some(Duration::from_secs(120).try_into().expect("idle timeout")));
    if let Some(window) = setup.window {
        transport.stream_receive_window(window.into());
    }
    if let Some(window) = setup.connection_window {
        transport.receive_window(window.into());
    }
    if let Some(streams) = setup.uni_streams {
        transport.max_concurrent_uni_streams(streams.into());
    }
    Arc::new(transport)
}

pub async fn pair(setup: Setup) -> Result<Peers, TestError> {
    let mut server = identity().quic_server();
    server.transport_config(transport(&PLAIN));
    let mut client = identity().quic_client();
    client.transport_config(transport(&setup));
    let server = noq::Endpoint::server(server, "127.0.0.1:0".parse()?)?;
    let mut endpoint = noq::EndpointConfig::default();
    endpoint.reliable_stream_reset(setup.reliable_reset);
    let socket = std::net::UdpSocket::bind("127.0.0.1:0")?;
    let client_endpoint = noq::Endpoint::new(endpoint, None, socket, noq::default_runtime().ok_or("runtime")?)?;
    let connecting = client_endpoint.connect_with(client, server.local_addr()?, "localhost")?;
    let (client, accepted) = tokio::join!(connecting, async { server.accept().await.expect("incoming").await });
    let client = client?;
    let control = match setup.settings {
        Some(pairs) => Some(uni(&client, &control(pairs)).await?),
        None => None,
    };
    let budget = Arc::new(Budget {
        used: AtomicUsize::new(0),
        limit: AtomicUsize::new(setup.limit),
    });
    Ok(Peers {
        server: accepted?,
        client,
        budget,
        _held: (control, [server, client_endpoint]),
    })
}

/// Peers whose server runs the layer; `stop` shuts it down, and `outcomes` carries what handlers report.
pub struct Served<T = ()> {
    pub peers: Peers,
    pub serving: Task,
    pub stop: Arc<Notify>,
    pub outcomes: mpsc::UnboundedReceiver<T>,
}

impl Peers {
    /// Serves each request on its own task until the connection closes.
    pub fn serve<F, H>(self, handler: H) -> Served
    where
        H: Fn(http::Request<()>, RequestStream) -> F + Send + Sync + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        let connection = server::Connection::new(self.server.clone(), Some(self.budget.clone()));
        let stop = Arc::new(Notify::new());
        let serving = tokio::spawn(serve(connection, Arc::new(handler), stop.clone()));
        Served {
            peers: self,
            serving,
            stop,
            outcomes: mpsc::unbounded_channel().1,
        }
    }

    /// Accepts every CONNECT as a session and runs `scenario` on it; other requests get an empty 200.
    pub fn sessions<F, H>(self, scenario: H) -> Served<F::Output>
    where
        H: Fn(Session) -> F + Send + Sync + 'static,
        F: Future<Output: Send> + Send + 'static,
    {
        let (scenario, (outcomes, outcome)) = (Arc::new(scenario), mpsc::unbounded_channel());
        let Served { peers, serving, stop, .. } = self.serve(move |request, stream| {
            let (scenario, outcomes) = (scenario.clone(), outcomes.clone());
            async move {
                if request.method() != http::Method::CONNECT {
                    respond(request, stream).await;
                } else if let Ok(session) = Session::accept(stream, http::HeaderMap::new()).await {
                    let _ = outcomes.send(scenario(session).await);
                }
            }
        });
        Served { peers, serving, stop, outcomes: outcome }
    }

    /// A raw CONNECT; a task reads its response stream so stream credit keeps flowing.
    pub async fn connect(&self) -> Result<(noq::SendStream, JoinHandle<(Vec<u8>, Result<(), Code>)>), TestError> {
        let (send, mut recv) = self.bi(&connect_head()).await?;
        Ok((send, tokio::spawn(async move { raw_stream(&mut recv).await })))
    }

    /// A raw client request stream that has sent `bytes`.
    pub async fn bi(&self, bytes: &[u8]) -> Result<(noq::SendStream, noq::RecvStream), TestError> {
        let (mut send, recv) = self.client.open_bi().await?;
        send.write_all(bytes).await?;
        Ok((send, recv))
    }

    /// A raw GET answered in full.
    pub async fn get(&self) -> Result<(), TestError> {
        let (mut send, mut recv) = self.bi(&request_head(&[])).await?;
        send.finish()?;
        assert_eq!(raw_stream(&mut recv).await.1, Ok(()));
        Ok(())
    }
}

async fn serve<F, H>(mut connection: server::Connection, handler: Arc<H>, stop: Arc<Notify>) -> Result<(), Error>
where
    H: Fn(http::Request<()>, RequestStream) -> F + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    loop {
        let request = tokio::select! {
            request = connection.next() => request?,
            () = stop.notified() => {
                connection.shutdown(4, "shutdown");
                continue;
            }
        };
        let Some(request) = request else { return Ok(()) };
        let handler = handler.clone();
        tokio::spawn(async move {
            if let Ok((request, stream)) = request.resolve().await {
                handler(request, stream).await;
            }
        });
    }
}

/// Answers with an empty 200.
pub async fn respond(_: http::Request<()>, stream: RequestStream) {
    let mut send = stream.split().0;
    send.send_response(http::Response::new(())).await.unwrap();
    send.finish().await.unwrap();
}

pub fn client(peers: &Peers) -> (Task, client::SendRequest) {
    let (mut driver, requests) = client::new(peers.client.clone());
    (tokio::spawn(async move { driver.drive().await }), requests)
}

/// Our client's session at /wt, which the server accepts.
pub async fn accepted(requests: &client::SendRequest) -> Result<Session, TestError> {
    Ok(Session::connect(requests, get("/wt")).await?.expect("accepted").0)
}

pub async fn until_closed(session: Session) -> Result<(u32, String), Error> {
    session.closed().await
}

pub fn get(path: &str) -> http::Request<()> {
    http::Request::get(format!("https://localhost{path}")).body(()).unwrap()
}

pub async fn body(stream: &mut graphite_meter_http3::RecvHalf) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    while let Some(chunk) = stream.data().await? {
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Yields until the layer holds more than `above` bytes of the budget, and returns how many.
pub async fn charged(budget: &Budget, above: usize) -> usize {
    loop {
        let used = budget.used.load(Ordering::Relaxed);
        if used > above {
            return used;
        }
        tokio::task::yield_now().await;
    }
}

/// Waits for every layer charge to be refunded.
pub async fn settled(budget: &Budget) {
    let refunded = async {
        while budget.used.load(Ordering::Relaxed) != 0 {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), refunded)
        .await
        .expect("layer charges return to zero");
}

pub async fn yields() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

/// Moves tokio's clock, and noq's with it, by `deadline`.
pub async fn jump(deadline: Duration) {
    tokio::time::pause();
    tokio::time::advance(deadline).await;
    tokio::time::resume();
}

pub fn varint(value: u64) -> Vec<u8> {
    let size = [1, 2, 4, 8]
        .into_iter()
        .find(|size| value >> (8 * size - 2) == 0)
        .expect("a varint");
    let mut bytes = value.to_be_bytes()[8 - size..].to_vec();
    bytes[0] |= (size.trailing_zeros() as u8) << 6;
    bytes
}

pub fn frame(kind: u64, payload: &[u8]) -> Vec<u8> {
    [varint(kind), varint(payload.len() as u64), payload.to_vec()].concat()
}

/// A static-only QPACK section of literal names and values, as an independent encoder writes it.
pub fn section(fields: &[(&str, &str)]) -> Vec<u8> {
    fn integer(value: usize, bits: u32, pattern: u8, output: &mut Vec<u8>) {
        let max = (1 << bits) - 1;
        if value < max {
            return output.push(pattern | value as u8);
        }
        output.push(pattern | max as u8);
        let mut rest = value - max;
        while rest >= 0x80 {
            output.push(0x80 | rest as u8);
            rest >>= 7;
        }
        output.push(rest as u8);
    }
    let mut section = vec![0, 0];
    for (name, value) in fields {
        integer(name.len(), 3, 0x20, &mut section);
        section.extend_from_slice(name.as_bytes());
        integer(value.len(), 7, 0x00, &mut section);
        section.extend_from_slice(value.as_bytes());
    }
    section
}

/// A GET of / with `fields` in place of or after its own.
pub fn request_head(fields: &[(&str, &str)]) -> Vec<u8> {
    let head = [(":method", "GET"), (":scheme", "https"), (":authority", "localhost"), (":path", "/")];
    let mut all: Vec<_> = head
        .into_iter()
        .filter(|(name, _)| !fields.iter().any(|(field, _)| field == name))
        .collect();
    all.extend(fields);
    frame(0x01, &section(&all))
}

pub fn connect_head() -> Vec<u8> {
    request_head(&[(":method", "CONNECT"), (":protocol", "webtransport"), (":path", "/wt")])
}

/// A CLOSE_WEBTRANSPORT_SESSION capsule in a DATA frame; capsules are framed as frames are.
pub fn close_capsule(code: u32, reason: &str) -> Vec<u8> {
    frame(0x00, &frame(0x2843, &[&code.to_be_bytes()[..], reason.as_bytes()].concat()))
}

/// A control stream's type, then SETTINGS of `pairs`.
pub fn control(pairs: &[(u64, u64)]) -> Vec<u8> {
    let payload: Vec<u8> = pairs
        .iter()
        .flat_map(|&(id, value)| [varint(id), varint(value)].concat())
        .collect();
    [vec![0x00], frame(0x04, &payload)].concat()
}

pub async fn uni(quic: &noq::Connection, bytes: &[u8]) -> Result<noq::SendStream, TestError> {
    let mut stream = quic.open_uni().await?;
    stream.write_all(bytes).await?;
    Ok(stream)
}

/// A raw stream's bytes, and how it ended: FIN, a reset code, or the connection's end as code 0.
pub async fn raw_stream(recv: &mut noq::RecvStream) -> (Vec<u8>, Result<(), Code>) {
    let mut bytes = Vec::new();
    loop {
        match recv.read_chunk(usize::MAX).await {
            Ok(Some(chunk)) => bytes.extend_from_slice(&chunk),
            Ok(None) => return (bytes, Ok(())),
            Err(noq::ReadError::Reset(code)) => return (bytes, Err(Code(code.into_inner()))),
            Err(_) => return (bytes, Err(Code(0))),
        }
    }
}

/// Reads a raw stream's first frame, whose type and length take a byte each.
pub async fn first_frame(recv: &mut noq::RecvStream) -> Result<Vec<u8>, TestError> {
    let mut frame = vec![0; 2];
    recv.read_exact(&mut frame).await?;
    frame.resize(2 + usize::from(frame[1]), 0);
    recv.read_exact(&mut frame[2..]).await?;
    Ok(frame)
}

pub async fn stopped(send: &noq::SendStream) -> Option<Code> {
    send.stopped()
        .await
        .expect("stream outcome")
        .map(|code| Code(code.into_inner()))
}

/// Whether STOP_SENDING has already arrived for a raw stream.
pub async fn stopped_yet(send: &noq::SendStream) -> bool {
    yields().await;
    std::future::poll_fn(|cx| Poll::Ready(std::pin::pin!(send.stopped()).poll(cx).is_ready())).await
}

/// The error this side reports for a connection it closed with `code`.
pub fn closed(code: Code) -> Error {
    Error::Connection { local: true, code, reason: bytes::Bytes::new() }
}

/// The code a connection closed with, within 5 s.
pub async fn closed_with(quic: &noq::Connection) -> Code {
    match tokio::time::timeout(Duration::from_secs(5), quic.closed()).await {
        Ok(noq::ConnectionError::ApplicationClosed(close)) => Code(close.error_code.into_inner()),
        error => panic!("closed without an application code: {error:?}"),
    }
}
