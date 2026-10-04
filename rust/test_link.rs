use bytes::Bytes;
use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpListener, TcpStream, UdpSocket,
        tcp::{ReadHalf, WriteHalf},
    },
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    Stall,
    Reset,
}

const QUIC_RETRY_HEADER: u8 = 0xF0;

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

    pub fn retries(&self) -> usize {
        self.retries.load(Ordering::Relaxed)
    }

    pub async fn tcp(target: SocketAddr, one_way: Duration) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
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
        Self::udp_from(std::net::Ipv4Addr::LOCALHOST.into(), target, one_way).await
    }

    /// Relays from `source`, which the target sees as the client's address.
    pub async fn udp_from(source: std::net::IpAddr, target: SocketAddr, one_way: Duration) -> io::Result<Self> {
        let front = UdpSocket::bind("127.0.0.1:0").await?;
        let back = UdpSocket::bind(SocketAddr::new(source, 0)).await?;
        back.connect(target).await?;
        for socket in [&front, &back] {
            let socket = socket2::SockRef::from(socket);
            socket.set_recv_buffer_size(16 << 20)?;
            socket.set_send_buffer_size(16 << 20)?;
        }
        let address = front.local_addr()?;
        let (front, back) = (Arc::new(front), Arc::new(back));
        let (fault, faults) = watch::channel(Fault::None);
        let (client_tx, client) = watch::channel(None::<SocketAddr>);
        let (up_tx, mut up) = mpsc::channel::<(Instant, Bytes)>(16384);
        let (down_tx, mut down) = mpsc::channel::<(Instant, Bytes)>(16384);
        let mut tasks = JoinSet::new();
        let (reader, open) = (front.clone(), faults.clone());
        tasks.spawn(async move {
            let mut buffer = vec![0; 65536];
            while let Ok((count, from)) = reader.recv_from(&mut buffer).await {
                client_tx.send_replace(Some(from));
                let forward = *open.borrow() == Fault::None;
                if forward {
                    let packet = Bytes::copy_from_slice(&buffer[..count]);
                    let _ = up_tx.send((Instant::now() + one_way, packet)).await;
                }
            }
        });
        let retries = Arc::new(AtomicUsize::new(0));
        let (reader, open, counted) = (back.clone(), faults.clone(), retries.clone());
        tasks.spawn(async move {
            let mut buffer = vec![0; 65536];
            while let Ok(count) = reader.recv(&mut buffer).await {
                if buffer[0] & QUIC_RETRY_HEADER == QUIC_RETRY_HEADER {
                    counted.fetch_add(1, Ordering::Relaxed);
                }
                let forward = *open.borrow() == Fault::None;
                if forward {
                    let packet = Bytes::copy_from_slice(&buffer[..count]);
                    let _ = down_tx.send((Instant::now() + one_way, packet)).await;
                }
            }
        });
        let open = faults.clone();
        tasks.spawn(async move {
            while let Some((at, packet)) = up.recv().await {
                tokio::time::sleep_until(at).await;
                let forward = *open.borrow() == Fault::None;
                if forward {
                    let _ = back.send(&packet).await;
                }
            }
        });
        tasks.spawn(async move {
            while let Some((at, packet)) = down.recv().await {
                tokio::time::sleep_until(at).await;
                let (client, open) = (*client.borrow(), *faults.borrow() == Fault::None);
                if let (Some(client), true) = (client, open) {
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

async fn pipe(mut from: ReadHalf<'_>, mut to: WriteHalf<'_>, delay: Duration, faults: watch::Receiver<Fault>) {
    let (sender, mut receiver) = mpsc::channel::<(Instant, Bytes)>(256);
    let mut open = faults.clone();
    let read = async move {
        let mut buffer = vec![0; 64 * 1024];
        loop {
            if open.wait_for(|fault| *fault == Fault::None).await.is_err() {
                return;
            }
            let Ok(count @ 1..) = from.read(&mut buffer).await else {
                return;
            };
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
