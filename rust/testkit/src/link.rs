//! A loopback relay that delays each direction, optionally through a bottleneck, and injects faults on demand.
use bytes::Bytes;
use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket, tcp},
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    /// Nothing passes, in either direction, until the fault is cleared.
    Stall,
}

/// A drop-tail bottleneck in each direction of a UDP link: its rate, and how much sending its queue holds.
#[derive(Clone, Copy, Debug)]
pub struct Bottleneck {
    pub bits_per_second: u64,
    pub queue: Duration,
}

/// When packets leave a direction's bottleneck.
struct Pace {
    bottleneck: Option<Bottleneck>,
    free: Instant,
}

impl Pace {
    fn new(bottleneck: Option<Bottleneck>) -> Self {
        Self { bottleneck, free: Instant::now() }
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

/// A relay to one target; dropping it stops relaying.
pub struct Link {
    pub address: SocketAddr,
    fault: watch::Sender<Fault>,
    _tasks: JoinSet<()>,
}

impl Link {
    pub fn inject(&self, fault: Fault) {
        self.fault.send_replace(fault);
    }

    pub async fn tcp(target: SocketAddr, one_way: Duration) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let (fault, faults) = watch::channel(Fault::None);
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
                relays.spawn(relay(client, server, one_way, faults.clone()));
            }
        });
        Ok(Self { address, fault, _tasks: tasks })
    }

    pub async fn udp(target: SocketAddr, one_way: Duration) -> io::Result<Self> {
        Self::udp_relay(target, one_way, None).await
    }

    /// Relays through `bottleneck` in each direction before the delay.
    pub async fn udp_through(target: SocketAddr, one_way: Duration, bottleneck: Bottleneck) -> io::Result<Self> {
        Self::udp_relay(target, one_way, Some(bottleneck)).await
    }

    async fn udp_relay(target: SocketAddr, one_way: Duration, bottleneck: Option<Bottleneck>) -> io::Result<Self> {
        let front = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let back = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        back.connect(target).await?;
        for socket in [&front, &back] {
            let socket = socket2::SockRef::from(socket);
            socket.set_recv_buffer_size(16 << 20)?;
            socket.set_send_buffer_size(16 << 20)?;
        }
        let address = front.local_addr()?;
        let (front, back) = (Arc::new(front), Arc::new(back));
        let (fault, faults) = watch::channel(Fault::None);
        let (client, clients) = watch::channel(None::<SocketAddr>);
        let (up, mut ups) = mpsc::channel::<(Instant, Bytes)>(16384);
        let (down, mut downs) = mpsc::channel::<(Instant, Bytes)>(16384);
        let mut tasks = JoinSet::new();
        let (reader, open) = (front.clone(), faults.clone());
        tasks.spawn(async move {
            let (mut buffer, mut pace) = (vec![0; 65536], Pace::new(bottleneck));
            loop {
                let (count, from) = match reader.recv_from(&mut buffer).await {
                    Ok(received) => received,
                    Err(error) if transient(&error) => continue,
                    Err(_) => break,
                };
                client.send_replace(Some(from));
                if *open.borrow() == Fault::None
                    && let Some(departure) = pace.departure(count)
                {
                    let _ = up
                        .send((departure + one_way, Bytes::copy_from_slice(&buffer[..count])))
                        .await;
                }
            }
        });
        let (reader, open) = (back.clone(), faults.clone());
        tasks.spawn(async move {
            let (mut buffer, mut pace) = (vec![0; 65536], Pace::new(bottleneck));
            loop {
                let count = match reader.recv(&mut buffer).await {
                    Ok(count) => count,
                    Err(error) if transient(&error) => continue,
                    Err(_) => break,
                };
                if *open.borrow() == Fault::None
                    && let Some(departure) = pace.departure(count)
                {
                    let _ = down
                        .send((departure + one_way, Bytes::copy_from_slice(&buffer[..count])))
                        .await;
                }
            }
        });
        let open = faults.clone();
        tasks.spawn(async move {
            while let Some((at, packet)) = ups.recv().await {
                tokio::time::sleep_until(at).await;
                if *open.borrow() == Fault::None {
                    let _ = back.send(&packet).await;
                }
            }
        });
        tasks.spawn(async move {
            while let Some((at, packet)) = downs.recv().await {
                tokio::time::sleep_until(at).await;
                let (client, fault) = (*clients.borrow(), *faults.borrow());
                if let (Some(client), Fault::None) = (client, fault) {
                    let _ = front.send_to(&packet, client).await;
                }
            }
        });
        Ok(Self { address, fault, _tasks: tasks })
    }
}

/// A receive error an earlier send's ICMP reply raised, such as an unbound target; the socket still works.
fn transient(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset)
}

async fn relay(mut client: TcpStream, mut server: TcpStream, delay: Duration, faults: watch::Receiver<Fault>) {
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);
    let (client_read, client_write) = client.split();
    let (server_read, server_write) = server.split();
    tokio::join!(
        pipe(client_read, server_write, delay, faults.clone()),
        pipe(server_read, client_write, delay, faults),
    );
}

/// Forwards each chunk `delay` after it was read, holding everything while a fault is set.
async fn pipe(
    mut from: tcp::ReadHalf<'_>,
    mut to: tcp::WriteHalf<'_>,
    delay: Duration,
    faults: watch::Receiver<Fault>,
) {
    let (sender, mut receiver) = mpsc::channel::<(Instant, Bytes)>(256);
    let mut open = faults.clone();
    let read = async move {
        let mut buffer = vec![0; 64 * 1024];
        while open.wait_for(|fault| *fault == Fault::None).await.is_ok() {
            let Ok(count @ 1..) = from.read(&mut buffer).await else { return };
            let chunk = Bytes::copy_from_slice(&buffer[..count]);
            if sender.send((Instant::now() + delay, chunk)).await.is_err() {
                return;
            }
        }
    };
    let mut open = faults;
    let write = async move {
        while let Some((at, chunk)) = receiver.recv().await {
            tokio::time::sleep_until(at).await;
            let open = open.wait_for(|fault| *fault == Fault::None).await.is_ok();
            if !open || to.write_all(&chunk).await.is_err() {
                return;
            }
        }
        let _ = to.shutdown().await;
    };
    tokio::join!(read, write);
}
