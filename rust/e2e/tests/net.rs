//! The client's request API against the server.
use graphite_meter_client::net::{Client, Fault, Request};
use graphite_meter_e2e::Server;
use graphite_meter_net::{ConnectError, Pool};
use graphite_meter_proto::{
    discovery::{NegotiatedProtocol, Probe, Protocol},
    origin::Origin,
    route::Route,
};
use graphite_meter_testkit::{self as testkit, Link};
use http::Method;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn client() -> Client {
    Client::new(true, Arc::new(Pool::inline()))
}

async fn probe(client: &Client, via: Protocol, origin: &Origin) -> Result<Probe, Fault> {
    client
        .json(via, Request::new(Method::GET, origin, Route::Probe), Probe::decode)
        .await
}

#[tokio::test]
async fn json_travels_over_http1_http2_and_http3() {
    let (server, client) = (Server::start().await, client());
    for (via, origin, negotiated) in [
        (Protocol::Http1, &server.http1, NegotiatedProtocol::Http1),
        (Protocol::Http2, &server.http2, NegotiatedProtocol::Http2),
        (Protocol::Http3, &server.http3, NegotiatedProtocol::Http3),
    ] {
        assert_eq!(probe(&client, via, origin).await.unwrap().protocol_negotiated, negotiated);
    }
}

#[tokio::test]
async fn a_silent_quic_address_fails_after_3_s_while_a_delayed_answering_one_finishes() {
    let (server, client) = (Server::start().await, client());
    let silent = Link::udp(server.quic, Duration::ZERO).await.unwrap();
    silent.inject(testkit::Fault::Stall);
    let delayed = Link::udp(server.quic, Duration::from_secs(1)).await.unwrap();
    let timed = async |link: &Link| {
        let (started, origin) = (Instant::now(), Origin::parse(&format!("https://{}", link.address)).unwrap());
        (probe(&client, Protocol::Http3, &origin).await, started.elapsed())
    };
    let ((unanswered, silence), (answered, delay)) = tokio::join!(timed(&silent), timed(&delayed));
    assert!(matches!(unanswered, Err(Fault::Connect(ConnectError::Unreachable(_)))), "{unanswered:?}");
    assert!((3.0..4.0).contains(&silence.as_secs_f64()), "{silence:?}");
    answered.unwrap();
    assert!(delay > Duration::from_secs(3), "{delay:?} outlasts the silent address's bound");
}
