//! Real TLS provider exchange; JWT signing uses the same reviewed library as verification.
use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinSet,
};
use tokio_rustls::TlsAcceptor;

use crate::test_identity;

#[derive(Default)]
struct Claims {
    nonce: String,
    pkce: String,
    wrong_nonce: bool,
    wrong_party: bool,
    wrong_subject: bool,
    denied_group: bool,
}

#[tokio::test]
async fn signed_provider_exchange_checks_nonce_subject_group_and_pkce() {
    let (certificate, key) = test_identity::generate_identity().unwrap();
    let generated = Command::new("openssl")
        .args(["genrsa", "-traditional", "2048"])
        .output()
        .unwrap();
    assert!(generated.status.success());
    let signer = CoreRsaPrivateSigningKey::from_pem(
        std::str::from_utf8(&generated.stdout).unwrap(),
        Some(JsonWebKeyId::new("test-key".into())),
    )
    .unwrap();
    let certificates = CertificateDer::pem_slice_iter(certificate.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(crate::crypto::provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certificates.clone(), key)
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let mut oidc = Oidc::new(&AuthConfig {
        mode: crate::config::AuthMode::Oidc,
        public_url: "https://meter.example".into(),
        oidc_issuer: issuer.clone(),
        oidc_client_id: "meter".into(),
        oidc_client_secret: "secret".into(),
        oidc_allowed_groups: vec!["operators".into()],
        ..AuthConfig::default()
    })
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    for cert in certificates {
        roots.add(cert).unwrap();
    }
    let client_tls =
        rustls::ClientConfig::builder_with_provider(Arc::new(crate::crypto::provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
    oidc.http = ProviderHttp(
        reqwest::Client::builder()
            .use_preconfigured_tls(client_tls)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
    );
    let claims = Arc::new(Mutex::new(Claims::default()));
    let server_claims = claims.clone();
    let server_issuer = issuer.clone();
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let signer = Arc::new(signer);
        let mut connections = JoinSet::new();
        loop {
            let socket = tokio::select! {
                _ = &mut stopped => break,
                accepted = listener.accept() => accepted.unwrap().0,
                Some(result) = connections.join_next(), if !connections.is_empty() => { result.unwrap(); continue; },
            };
            let acceptor = acceptor.clone();
            let claims = server_claims.clone();
            let issuer = server_issuer.clone();
            let signer = signer.clone();
            connections.spawn(async move {
                let mut socket = acceptor.accept(socket).await.unwrap();
                let mut raw = Vec::new();
                let header_end = loop {
                    let mut buffer = [0; 2048];
                    let read = socket.read(&mut buffer).await.unwrap();
                    assert!(read > 0);
                    raw.extend_from_slice(&buffer[..read]);
                    if let Some(end) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") { break end + 4; }
                    assert!(raw.len() <= 16384);
                };
                let headers = String::from_utf8(raw[..header_end].to_vec()).unwrap();
                let length = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").map(|length| length.parse::<usize>().unwrap())).unwrap_or(0);
                assert!(length <= 4096);
                while raw.len() < header_end + length {
                    let mut buffer = [0; 2048];
                    let read = socket.read(&mut buffer).await.unwrap();
                    assert!(read > 0);
                    raw.extend_from_slice(&buffer[..read]);
                }
                let path = headers.split_whitespace().nth(1).unwrap();
                let body = {
                    let claims = claims.lock().unwrap();
                    match path {
                        "/.well-known/openid-configuration" => json!({"issuer": issuer, "authorization_endpoint": format!("{issuer}/authorize"), "token_endpoint": format!("{issuer}/token"), "userinfo_endpoint": format!("{issuer}/userinfo"), "jwks_uri": format!("{issuer}/jwks"), "response_types_supported": ["code"], "subject_types_supported": ["public"], "id_token_signing_alg_values_supported": ["RS256"], "authorization_response_iss_parameter_supported": true}),
                        "/jwks" => json!({"keys": [signer.as_verification_key()]}),
                        "/token" => {
                            let form: HashMap<_, _> = url::form_urlencoded::parse(&raw[header_end..]).into_owned().collect();
                            assert_eq!(form["code"], "valid-code");
                            assert_eq!(form["redirect_uri"], "https://meter.example/auth/oidc/callback");
                            assert_eq!(URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())), claims.pkce);
                            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                            let token_claims: CoreIdTokenClaims = serde_json::from_value(json!({"iss": issuer, "aud": "meter", "azp": if claims.wrong_party {"other-client"} else {"meter"}, "sub": "operator", "iat": now, "exp": now + 300, "nonce": if claims.wrong_nonce { "invalid" } else { &claims.nonce }})).unwrap();
                            let token = CoreIdToken::new(token_claims, signer.as_ref(), CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256, Some(&AccessToken::new("access".into())), None).unwrap();
                            json!({"access_token": "access", "token_type": "Bearer", "id_token": token.to_string()})
                        }
                        "/userinfo" => json!({"sub": if claims.wrong_subject { "other" } else { "operator" }, "name": "Example Operator", "groups": if claims.denied_group { vec!["outsiders"] } else { vec!["operators"] }}),
                        _ => panic!("unexpected provider endpoint {path}"),
                    }
                };
                let body = serde_json::to_vec(&body).unwrap();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
                socket.shutdown().await.unwrap();
            });
        }
        while let Some(result) = connections.join_next().await {
            result.unwrap();
        }
    });
    for scenario in 0..5 {
        let started = oidc
            .start("192.0.2.1".parse().unwrap(), "challenge".into(), None)
            .await
            .unwrap();
        let fields: HashMap<_, _> = url::Url::parse(&started.url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        *claims.lock().unwrap() = Claims {
            nonce: fields["nonce"].clone(),
            pkce: fields["code_challenge"].clone(),
            wrong_nonce: scenario == 1,
            wrong_party: scenario == 4,
            wrong_subject: scenario == 2,
            denied_group: scenario == 3,
        };
        let result = oidc
            .finish(
                &fields["state"],
                &started.browser,
                "valid-code",
                Some(&issuer),
            )
            .await;
        if scenario == 0 {
            let identity = result.unwrap();
            assert_eq!(identity.subject, "oidc:operator");
            assert_eq!(identity.name, "Example Operator");
            assert_eq!(identity.challenge, "challenge");
        } else {
            assert!(result.is_err(), "scenario {scenario} authenticated");
        }
    }
    stop.send(()).unwrap();
    server.await.unwrap();
}
