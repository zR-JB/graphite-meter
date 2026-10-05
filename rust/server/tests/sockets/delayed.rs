//! Delayed links: an HTTP/3 upload beyond the floor window, and the optimized-build gate that QUIC downloads exceed
//! the old window limit.

use super::{
    http3::{H3, read, transport},
    *,
};
use bytes::Bytes;
use graphite_meter_http3::{RecvHalf, webtransport::RecvStream};
use graphite_meter_testkit::Link;
use tokio::time::Instant;

const KIB: usize = 1 << 10;

#[tokio::test]
async fn an_http3_upload_is_not_bound_by_the_floor_window_on_a_delayed_link() {
    let h3 = H3::start(&[]).await;
    let link = Link::udp(h3.server.quic.unwrap(), Duration::from_millis(20))
        .await
        .unwrap();
    let connection = h3.connect_via(link.address, transport(None)).await;
    let id = connection.upload_id().await;
    let size = 4 * KIB * KIB;
    let granted = connection.quic.stats().frame_rx.max_data;
    let (mut upload, mut answer) = connection.open("POST", &format!("/upload?id={id}")).await;
    upload.send_data(Bytes::from(vec![7; size])).await.unwrap();
    upload.finish().await.unwrap();
    assert_eq!(answer.response().await.unwrap().status(), 200);
    assert_eq!(read(&mut answer).await.unwrap(), format!(r#"{{"bytes":{size}}}"#).as_bytes());
    let updates = connection.quic.stats().frame_rx.max_data - granted;
    let floor_bound = ((size - 64 * KIB) / (64 * KIB)) as u64;
    assert!(
        updates < floor_bound,
        "{updates} MAX_DATA round trips; each raises a 64 KiB floor window by 64 KiB at most"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "optimized-build delayed-link throughput gate"]
async fn quic_downloads_exceed_the_old_window_limit() {
    if cfg!(debug_assertions) {
        panic!("run this gate with an optimized profile");
    }
    let gate = async {
        for webtransport in [false, true] {
            let baseline = download_rate(webtransport, Duration::ZERO).await;
            if baseline < 671.0 {
                eprintln!("inconclusive: webtransport={webtransport}, loopback below 671 Mbit/s");
                continue;
            }
            let delayed = download_rate(webtransport, Duration::from_millis(50)).await;
            assert!(
                delayed > 419.0,
                "webtransport={webtransport}: delayed throughput below 419 Mbit/s; see the samples"
            );
        }
    };
    tokio::time::timeout(Duration::from_secs(60), gate).await.unwrap();
}

/// A download's body or WebTransport stream.
enum Lane {
    Http(RecvHalf),
    Stream(RecvStream),
}

impl Lane {
    async fn chunk(&mut self) -> Option<Bytes> {
        match self {
            Self::Http(body) => body.data().await.unwrap(),
            Self::Stream(stream) => stream.read_chunk().await.unwrap(),
        }
    }
}

/// The median rate in Mbit/s of one download's half-second samples after its first two seconds, over a link
/// delaying each direction by `one_way`.
async fn download_rate(webtransport: bool, one_way: Duration) -> f64 {
    let h3 = H3::start(&[]).await;
    let link = Link::udp(h3.server.quic.unwrap(), one_way).await.unwrap();
    let target = if one_way.is_zero() { h3.server.quic.unwrap() } else { link.address };
    let mut wide = transport(Some(64 << 20));
    wide.receive_window((64_u32 << 20).into());
    let connection = h3.connect_via(target, wide).await;
    let (mut lane, _session) = if webtransport {
        let session = connection.session("/wt/download?bytes=4294967296").await;
        (Lane::Stream(session.accept_uni().await.unwrap()), Some(session))
    } else {
        let (answer, body) = connection.send("GET", "/download?bytes=4294967296", b"").await;
        assert_eq!(answer.status(), 200);
        (Lane::Http(body), None)
    };
    let started = Instant::now();
    let (mut marks, mut total, mut next) = (Vec::new(), 0, Duration::from_millis(500));
    while started.elapsed() < Duration::from_secs(4) {
        let Some(chunk) = lane.chunk().await else { break };
        total += chunk.len();
        if started.elapsed() >= next {
            marks.push((started.elapsed(), total));
            next = started.elapsed() + Duration::from_millis(500);
        }
    }
    let mut rates = Vec::new();
    for pair in marks.windows(2) {
        let ((before, a), (at, b)) = (pair[0], pair[1]);
        let rate = (b - a) as f64 * 8.0 / (at - before).as_secs_f64() / 1e6;
        eprintln!("webtransport={webtransport} one_way={one_way:?} at={at:?}: {rate:.0} Mbit/s");
        if at >= Duration::from_secs(2) {
            rates.push(rate);
        }
    }
    assert!(rates.len() >= 3, "too few steady-state samples");
    rates.sort_by(f64::total_cmp);
    connection.quic.close(0_u32.into(), b"done");
    rates[rates.len() / 2]
}
