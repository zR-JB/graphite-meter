//! The stream and datagram limits each role's QUIC transport grants its peer, and the client's windows.
use graphite_meter_net::quic::{client_transport, server_transport};
use graphite_meter_testkit::Identity;
use std::{sync::Arc, time::Duration};

const REQUESTS: u32 = 7;

/// The stream `open` makes at once, as it does within the peer's limit.
async fn open<T>(open: impl Future<Output = Result<T, noq::ConnectionError>>) -> Option<T> {
    tokio::time::timeout(Duration::from_millis(100), open).await.ok()?.ok()
}

#[tokio::test]
async fn each_role_grants_the_streams_and_datagrams_of_its_transport() {
    let identity = Identity::generate().unwrap();
    let mut server = identity.quic_server();
    server.transport_config(Arc::new(server_transport(REQUESTS)));
    let mut client = identity.quic_client();
    client.transport_config(Arc::new(client_transport()));
    let server = noq::Endpoint::server(server, "127.0.0.1:0".parse().unwrap()).unwrap();
    let endpoint = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let connecting = endpoint
        .connect_with(client, server.local_addr().unwrap(), "localhost")
        .unwrap();
    let (client, accepted) = tokio::join!(connecting, async { server.accept().await.unwrap().await });
    let (client, server) = (client.unwrap(), accepted.unwrap());
    assert!(open(server.open_bi()).await.is_none(), "the server opens no request streams");
    let mut held = Vec::new();
    for _ in 0..REQUESTS {
        held.push(
            open(client.open_bi())
                .await
                .expect("a request within the server's grant"),
        );
    }
    assert!(open(client.open_bi()).await.is_none(), "requests past the server's grant wait");
    for (connection, granted) in [(&server, 36), (&client, 3 + 16 + 4)] {
        let mut streams = Vec::new();
        for _ in 0..granted {
            streams.push(
                open(connection.open_uni())
                    .await
                    .expect("a stream within the peer's grant"),
            );
        }
        assert!(open(connection.open_uni()).await.is_none(), "{granted} unidirectional streams");
    }
    assert!(client.max_datagram_size().is_some() && server.max_datagram_size().is_some());
}

#[test]
fn the_client_transport_autotunes_to_48_mib_with_32_mib_streams_and_sends_16_mib() {
    let config = format!("{:?}", client_transport());
    for window in [
        "stream_receive_window: 33554432,",
        ", receive_window: 50331648,",
        "initial_receive_window: Some(786432),",
        "send_window: 16777216,",
    ] {
        assert!(config.contains(window), "{window} in {config}");
    }
}
