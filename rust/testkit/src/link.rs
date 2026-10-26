//! A loopback relay that stalls on demand; over UDP it also delays each direction, optionally through a bottleneck.
use bytes::Bytes;
use std::{
    collections::HashMap,
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket, tcp},
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};

/// A drop-tail bottleneck in each direction of a UDP link: its rate, and how much sending its queue holds.
#[derive(Clone, Copy, Debug)]
pub struct Bottleneck {
    pub bits_per_second: u64,
    pub queue: Duration,
}

struct Pace {
    bottleneck: Option<Bottleneck>,
    free: Instant,
}

impl Pace {
    fn new(bottleneck: Option<Bottleneck>) -> Self {
        Self {
            bottleneck,
            free: Instant::now(),
        }
    }

    /// When a packet of `bytes` leaves the bottleneck, or `None` when its queue is full.
    fn departure(&mut self, bytes: usize) -> Option<Instant> {
        let now = Instant::now();
        let Some(bottleneck) = self.bottleneck else {
            return Some(now);
        };
        let start = self.free.max(now);
        if start - now > bottleneck.queue {
            return None;
        }
        let nanos = bytes as u64 * 8 * 1_000_000_000 / bottleneck.bits_per_second;
        self.free = start + Duration::from_nanos(nanos);
        Some(self.free)
    }
}

pub struct Link {
    pub address: SocketAddr,
    stalled: watch::Sender<bool>,
    _tasks: JoinSet<()>,
}

impl Link {
    /// From now on nothing passes, in either direction, until resumed.
    pub fn stall(&self) {
        self.stalled.send_replace(true);
    }

    pub fn resume(&self) {
        self.stalled.send_replace(false);
    }

    pub async fn tcp(target: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let (stalled, stalls) = watch::channel(false);
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let mut relays = JoinSet::new();
            loop {
                let accepted = tokio::select! {
                    Some(_) = relays.join_next() => continue,
                    accepted = listener.accept() => accepted,
                };
                let Ok((client, _)) = accepted else { break };
                let Ok(server) = TcpStream::connect(target).await else {
                    let _ = client.set_zero_linger();
                    continue;
                };
                relays.spawn(relay(client, server, stalls.clone()));
            }
        });
        Ok(Self {
            address,
            stalled,
            _tasks: tasks,
        })
    }

    /// Relays through `bottleneck`, if any, each way before the delay; each client address gets its own socket, as behind a NAT.
    pub async fn udp(
        target: SocketAddr,
        one_way: Duration,
        bottleneck: Option<Bottleneck>,
    ) -> io::Result<Self> {
        if bottleneck.is_some_and(|bottleneck| bottleneck.bits_per_second == 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a bottleneck passes at least 1 bit/s",
            ));
        }
        let front = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?);
        buffers(&front)?;
        let address = front.local_addr()?;
        let (stalled, stalls) = watch::channel(false);
        let (up, mut ups) = mpsc::channel::<(Instant, Arc<UdpSocket>, Bytes)>(16384);
        let (down, mut downs) = mpsc::channel::<(Instant, SocketAddr, Bytes)>(16384);
        let mut tasks = JoinSet::new();
        let (reader, open) = (front.clone(), stalls.clone());
        tasks.spawn(async move {
            let (mut buffer, mut pace) = (vec![0; 65536], Pace::new(bottleneck));
            let down_pace = Arc::new(Mutex::new(Pace::new(bottleneck)));
            let (mut backs, mut readers) = (HashMap::new(), JoinSet::new());
            loop {
                let (count, from) = match reader.recv_from(&mut buffer).await {
                    Ok(received) => received,
                    Err(error) if transient(&error) => continue,
                    Err(_) => break,
                };
                let back = match backs.get(&from) {
                    Some(back) => Arc::clone(back),
                    None => {
                        let Ok(back) = connected(target).await else {
                            continue;
                        };
                        let (down, open, pace) = (down.clone(), open.clone(), down_pace.clone());
                        readers.spawn(back_to(back.clone(), from, down, open, pace, one_way));
                        backs.insert(from, back.clone());
                        back
                    }
                };
                if !*open.borrow()
                    && let Some(departure) = pace.departure(count)
                {
                    let packet = Bytes::copy_from_slice(&buffer[..count]);
                    let _ = up.send((departure + one_way, back, packet)).await;
                }
            }
        });
        let open = stalls.clone();
        tasks.spawn(async move {
            while let Some((at, back, packet)) = ups.recv().await {
                tokio::time::sleep_until(at).await;
                if !*open.borrow() {
                    let _ = back.send(&packet).await;
                }
            }
        });
        tasks.spawn(async move {
            while let Some((at, client, packet)) = downs.recv().await {
                tokio::time::sleep_until(at).await;
                if !*stalls.borrow() {
                    let _ = front.send_to(&packet, client).await;
                }
            }
        });
        Ok(Self {
            address,
            stalled,
            _tasks: tasks,
        })
    }
}

fn buffers(socket: &UdpSocket) -> io::Result<()> {
    let socket = socket2::SockRef::from(socket);
    socket.set_recv_buffer_size(16 << 20)?;
    socket.set_send_buffer_size(16 << 20)
}

async fn connected(target: SocketAddr) -> io::Result<Arc<UdpSocket>> {
    let back = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    back.connect(target).await?;
    buffers(&back)?;
    Ok(Arc::new(back))
}

async fn back_to(
    back: Arc<UdpSocket>,
    client: SocketAddr,
    down: mpsc::Sender<(Instant, SocketAddr, Bytes)>,
    open: watch::Receiver<bool>,
    pace: Arc<Mutex<Pace>>,
    one_way: Duration,
) {
    let mut buffer = vec![0; 65536];
    loop {
        let count = match back.recv(&mut buffer).await {
            Ok(count) => count,
            Err(error) if transient(&error) => continue,
            Err(_) => break,
        };
        let departure = (!*open.borrow())
            .then(|| {
                pace.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .departure(count)
            })
            .flatten();
        if let Some(departure) = departure {
            let packet = Bytes::copy_from_slice(&buffer[..count]);
            let _ = down.send((departure + one_way, client, packet)).await;
        }
    }
}

/// A receive error an earlier send's ICMP reply raised, such as an unbound target; the socket still works.
fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
    )
}

async fn relay(mut client: TcpStream, mut server: TcpStream, stalls: watch::Receiver<bool>) {
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);
    let (client_read, client_write) = client.split();
    let (server_read, server_write) = server.split();
    tokio::join!(
        pipe(client_read, server_write, stalls.clone()),
        pipe(server_read, client_write, stalls),
    );
}

/// Forwards what it reads, holding everything once stalled.
async fn pipe(
    mut from: tcp::ReadHalf<'_>,
    mut to: tcp::WriteHalf<'_>,
    mut stalls: watch::Receiver<bool>,
) {
    let mut buffer = vec![0; 64 * 1024];
    while stalls.wait_for(|stalled| !*stalled).await.is_ok() {
        let Ok(count @ 1..) = from.read(&mut buffer).await else {
            let _ = to.shutdown().await;
            return;
        };
        let open = stalls.wait_for(|stalled| !*stalled).await.is_ok();
        if !open || to.write_all(&buffer[..count]).await.is_err() {
            return;
        }
    }
}
