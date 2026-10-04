use graphite_meter_server::{
    config::{Config, NativeKind},
    tls::{Certificates, handshake_bytes},
};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, ServerName, pem::PemObject},
};
use std::{
    error::Error,
    fs,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

mod support;
type TestError = Box<dyn Error + Send + Sync>;

fn config(identity: &support::Identity) -> Config {
    let mut config = Config {
        tls_cert: identity.directory().join("identity.pem").to_str().unwrap().into(),
        tls_key: identity.directory().join("identity.key").to_str().unwrap().into(),
        ..Config::default()
    };
    config.native[NativeKind::H1Tls as usize].address = ":8443".into();
    config.native[NativeKind::H1Tls as usize].public_origin = "https://localhost:8443".into();
    config
}

async fn handshake(
    config: Arc<rustls::ServerConfig>,
    roots: &[CertificateDer<'static>],
) -> Result<CertificateDer<'static>, TestError> {
    let mut trust = RootCertStore::empty();
    for certificate in roots {
        trust.add(certificate.clone())?;
    }
    let client = ClientConfig::builder()
        .with_root_certificates(trust)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(client));
    let acceptor = TlsAcceptor::from(config);
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let name = ServerName::try_from("localhost")?;
    let handshakes = async { tokio::try_join!(connector.connect(name, client_io), acceptor.accept(server_io)) };
    let (client, server) = tokio::time::timeout(Duration::from_secs(5), handshakes).await??;
    let version = server.get_ref().1.protocol_version();
    assert_eq!(version, Some(rustls::ProtocolVersion::TLSv1_3));
    Ok(client.get_ref().1.peer_certificates().unwrap()[0].clone())
}

#[tokio::test]
async fn renewal_is_atomic_and_failed_reloads_keep_the_previous_identity() -> Result<(), TestError> {
    let first = support::Identity::generate();
    let second = support::Identity::generate();
    let config = config(&first);
    let original = CertificateDer::from_pem_file(&config.tls_cert)?;
    let replacement = CertificateDer::from_pem_file(second.directory().join("identity.pem"))?;
    let roots = [original.clone(), replacement.clone()];
    let budget = 2 * handshake_bytes(std::slice::from_ref(&original));
    let manager = Certificates::load(&config, SystemTime::now(), move |bytes| {
        (bytes <= budget)
            .then_some(())
            .ok_or_else(|| format!("{bytes} handshake bytes exceed the budget").into())
    })?;
    let tls = manager.config()?;
    assert_eq!(handshake(tls.clone(), &roots).await?, original);
    assert!(!manager.reload(SystemTime::now())?);

    assert!(manager.reload(SystemTime::UNIX_EPOCH).is_err());
    assert!(manager.reload(SystemTime::now() + Duration::from_secs(172800)).is_err());
    fs::copy(second.directory().join("identity.key"), &config.tls_key)?;
    assert_eq!(
        manager.reload(SystemTime::now()).unwrap_err().to_string(),
        "load matching TLS certificate/key: tls: private key does not match public key"
    );
    assert_eq!(handshake(tls.clone(), &roots).await?, original);

    fs::copy(second.directory().join("identity.pem"), &config.tls_cert)?;
    assert!(manager.reload(SystemTime::now())?);
    assert_eq!(handshake(tls.clone(), &roots).await?, replacement);
    fs::write(&config.tls_cert, b"incomplete certificate replacement")?;
    assert!(manager.reload(SystemTime::now()).is_err());
    assert_eq!(handshake(tls.clone(), &roots).await?, replacement);
    let long_chain = fs::read_to_string(second.directory().join("identity.pem"))?.repeat(80);
    fs::write(&config.tls_cert, long_chain)?;
    let refused = manager.reload(SystemTime::now()).unwrap_err().to_string();
    assert!(refused.ends_with("handshake bytes exceed the budget"), "{refused}");
    assert_eq!(handshake(tls, &roots).await?, replacement);
    Certificates::load(&config, SystemTime::now(), |_| Ok(()))?;
    Ok(())
}

#[test]
fn startup_requires_valid_time_and_all_enabled_public_hostnames() -> Result<(), TestError> {
    let identity = support::Identity::generate();
    let mut config = config(&identity);
    assert!(Certificates::load(&config, SystemTime::UNIX_EPOCH, |_| Ok(())).is_err());
    config.native[NativeKind::H2 as usize].address = ":8444".into();
    config.native[NativeKind::H2 as usize].public_origin = "https://other.example".into();
    assert!(Certificates::load(&config, SystemTime::now(), |_| Ok(())).is_err());
    config.native[NativeKind::H2 as usize].address.clear();
    Certificates::load(&config, SystemTime::now(), |_| Ok(()))?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn watcher_retries_invalid_replacement_and_stops_on_shutdown() -> Result<(), TestError> {
    let first = support::Identity::generate();
    let second = support::Identity::generate();
    let config = config(&first);
    let original = CertificateDer::from_pem_file(&config.tls_cert)?;
    let replacement = CertificateDer::from_pem_file(second.directory().join("identity.pem"))?;
    let roots = [original.clone(), replacement.clone()];
    let manager = Certificates::load(&config, SystemTime::now(), |_| Ok(()))?;
    let tls = manager.config()?;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let (reports, mut reported) = tokio::sync::mpsc::channel(4);
    let watch = tokio::spawn(manager.watch(
        async {
            let _ = stopped.await;
        },
        move |result| reports.try_send(result).unwrap(),
    ));
    tokio::task::yield_now().await;
    assert!(reported.try_recv().is_err(), "startup must not reread files");

    fs::copy(second.directory().join("identity.key"), &config.tls_key)?;
    tokio::time::advance(Duration::from_secs(60)).await;
    assert!(reported.recv().await.unwrap().is_err());
    assert_eq!(handshake(tls.clone(), &roots).await?, original);

    fs::copy(second.directory().join("identity.pem"), &config.tls_cert)?;
    tokio::time::advance(Duration::from_secs(60)).await;
    assert!(reported.recv().await.unwrap()?);
    assert_eq!(handshake(tls.clone(), &roots).await?, replacement);

    tokio::time::advance(Duration::from_secs(60)).await;
    assert!(!reported.recv().await.unwrap()?);
    stop.send(()).unwrap();
    watch.await??;
    tokio::time::advance(Duration::from_secs(120)).await;
    assert!(reported.recv().await.is_none());
    Ok(())
}
