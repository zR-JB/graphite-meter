//! A loopback relay that delays each direction and injects faults on demand.
use bytes::Bytes;
use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
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
    /// TCP connections end with a reset; UDP drops everything, as with `Stall`.
    Reset,
}

/// The long-header form bits that a QUIC Retry packet's first byte carries.
const RETRY: u8 = 0xf0;

/// A relay to one target; dropping it stops relaying.
pub struct Link {
    pub address: SocketAddr,
    fault: watch::Sender<Fault>,
    retries: Arc<AtomicUsize>,
    _tasks: JoinSet<()>,
}

impl Link {
    pub fn inject(&self, fault: Fault) {
        self.fault.send_replace(fault);
    }

    /// QUIC Retry packets the target has sent.
    pub fn retries(&self) -> usize {
        self.retries.load(Ordering::Relaxed)
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
                if *faults.borrow() == Fault::Reset {
                    let _ = client.set_zero_linger();
                    continue;
                }
                let Ok(server) = TcpStream::connect(target).await else {
                    let _ = client.set_zero_linger();
                    continue;
                };
                relays.spawn(relay(client, server, one_way, faults.clone()));
            }
        });
        Ok(Self { address, fault, retries: Arc::default(), _tasks: tasks })
    }

    pub async fn udp(target: SocketAddr, one_way: Duration) -> io::Result<Self> {
        Self::udp_from(Ipv4Addr::LOCALHOST.into(), target, one_way).await
    }

    /// Relays from `source`, which the target sees as the client's address.
    pub async fn udp_from(source: IpAddr, target: SocketAddr, one_way: Duration) -> io::Result<Self> {
        let front = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let back = UdpSocket::bind((source, 0)).await?;
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
        let retries = Arc::new(AtomicUsize::new(0));
        let (up, mut ups) = mpsc::channel::<(Instant, Bytes)>(16384);
        let (down, mut downs) = mpsc::channel::<(Instant, Bytes)>(16384);
        let mut tasks = JoinSet::new();
        let (reader, open) = (front.clone(), faults.clone());
        tasks.spawn(async move {
            let mut buffer = vec![0; 65536];
            while let Ok((count, from)) = reader.recv_from(&mut buffer).await {
                client.send_replace(Some(from));
                if *open.borrow() == Fault::None {
                    let _ = up
                        .send((Instant::now() + one_way, Bytes::copy_from_slice(&buffer[..count])))
                        .await;
                }
            }
        });
        let (reader, open, counted) = (back.clone(), faults.clone(), retries.clone());
        tasks.spawn(async move {
            let mut buffer = vec![0; 65536];
            while let Ok(count) = reader.recv(&mut buffer).await {
                if buffer[0] & RETRY == RETRY {
                    counted.fetch_add(1, Ordering::Relaxed);
                }
                if *open.borrow() == Fault::None {
                    let _ = down
                        .send((Instant::now() + one_way, Bytes::copy_from_slice(&buffer[..count])))
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
        Ok(Self { address, fault, retries, _tasks: tasks })
    }
}

async fn relay(mut client: TcpStream, mut server: TcpStream, delay: Duration, mut faults: watch::Receiver<Fault>) {
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);
    let (up, down) = (faults.clone(), faults.clone());
    let reset = {
        let (client_read, client_write) = client.split();
        let (server_read, server_write) = server.split();
        tokio::select! {
            _ = async {
                tokio::join!(
                    pipe(client_read, server_write, delay, up),
                    pipe(server_read, client_write, delay, down),
                )
            } => false,
            _ = faults.wait_for(|fault| *fault == Fault::Reset) => true,
        }
    };
    if reset {
        let _ = client.set_zero_linger();
        let _ = server.set_zero_linger();
    }
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
