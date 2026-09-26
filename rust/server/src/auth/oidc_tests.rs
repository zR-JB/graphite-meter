use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::{
    rand::SystemRandom,
    signature::{self, EcdsaKeyPair, KeyPair, RsaKeyPair},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use serde_json::{Value, json};
use std::{
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};
use tokio_rustls::TlsAcceptor;

use crate::test_identity;

#[derive(Clone, Copy, Default)]
struct Claims {
    wrong_nonce: bool,
    wrong_party: bool,
    wrong_subject: bool,
    denied_group: bool,
}

#[derive(Default)]
struct Twist {
    rotated: bool,
    unknown_kid: bool,
    claims: Option<Value>,
    header: Option<Value>,
    signed_userinfo: Option<Value>,
}

struct Keys {
    rsa: RsaKeyPair,
    ec: EcdsaKeyPair,
    rng: SystemRandom,
}

impl Keys {
    fn new() -> Self {
        let generated = Command::new("openssl")
            .args(["genrsa", "-traditional", "2048"])
            .output()
            .unwrap();
        assert!(generated.status.success());
        let PrivateKeyDer::Pkcs1(der) = PrivateKeyDer::from_pem_slice(&generated.stdout).unwrap()
        else {
            panic!("openssl did not emit PKCS#1");
        };
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .unwrap();
        Self {
            rsa: RsaKeyPair::from_der(der.secret_pkcs1_der()).unwrap(),
            ec: EcdsaKeyPair::from_pkcs8(
                &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                pkcs8.as_ref(),
                &rng,
            )
            .unwrap(),
            rng,
        }
    }
    fn jwks(&self, rotated: bool) -> Value {
        if rotated {
            let point = self.ec.public_key().as_ref();
            return json!({"keys": [{"kty": "EC", "crv": "P-256", "kid": "rotated", "use": "sig",
                "x": URL_SAFE_NO_PAD.encode(&point[1..33]), "y": URL_SAFE_NO_PAD.encode(&point[33..])}]});
        }
        let public: signature::RsaPublicKeyComponents<Vec<u8>> = self.rsa.public().into();
        json!({"keys": [{"kty": "RSA", "kid": "test-key", "use": "sig", "alg": "RS256",
            "n": URL_SAFE_NO_PAD.encode(&public.n), "e": URL_SAFE_NO_PAD.encode(&public.e)}]})
    }
    fn sign(&self, header: &Value, claims: &Value) -> String {
        let message = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = match header["alg"].as_str() {
            Some("RS256") => {
                let mut signature = vec![0; self.rsa.public().modulus_len()];
                self.rsa
                    .sign(
                        &signature::RSA_PKCS1_SHA256,
                        &self.rng,
                        message.as_bytes(),
                        &mut signature,
                    )
                    .unwrap();
                signature
            }
            Some("ES256") => self
                .ec
                .sign(&self.rng, message.as_bytes())
                .unwrap()
                .as_ref()
                .to_vec(),
            Some("HS256") => ring::hmac::sign(
                &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"secret"),
                message.as_bytes(),
            )
            .as_ref()
            .to_vec(),
            _ => Vec::new(),
        };
        format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature))
    }
}

fn at_hash(token: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
    URL_SAFE_NO_PAD.encode(&digest.as_ref()[..16])
}

struct ProviderDouble {
    oidc: Oidc,
    issuer: String,
    claims: Arc<Mutex<Claims>>,
    nonces: Arc<Mutex<HashMap<String, String>>>,
    twist: Arc<Mutex<Twist>>,
    jwks_requests: Arc<AtomicUsize>,
    stop: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
}

async fn provider_double(host: &str, algorithms: &[&str], proxy: Proxy) -> ProviderDouble {
    let (certificate, key) = test_identity::generate_identity(host).unwrap();
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
    let issuer = format!("https://{host}:{}", listener.local_addr().unwrap().port());
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
    oidc.http = ProviderHttp {
        tls: TlsConnector::from(Arc::new(client_tls)),
        proxy,
    };
    let claims = Arc::new(Mutex::new(Claims::default()));
    let twist = Arc::new(Mutex::new(Twist::default()));
    let nonces = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let jwks_requests = Arc::new(AtomicUsize::new(0));
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let (server_claims, server_twist, server_jwks, server_issuer, server_nonces) = (
        claims.clone(),
        twist.clone(),
        jwks_requests.clone(),
        issuer.clone(),
        nonces.clone(),
    );
    let algorithms: Vec<String> = algorithms.iter().map(|alg| (*alg).to_owned()).collect();
    let server = tokio::spawn(async move {
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let keys = Arc::new(Keys::new());
        let mut connections = JoinSet::new();
        loop {
            let socket = tokio::select! {
                _ = &mut stopped => break,
                accepted = listener.accept() => accepted.unwrap().0,
                Some(result) = connections.join_next(), if !connections.is_empty() => { result.unwrap(); continue; },
            };
            let (acceptor, claims, twist, jwks, issuer, keys, algorithms, nonces) = (
                acceptor.clone(),
                server_claims.clone(),
                server_twist.clone(),
                server_jwks.clone(),
                server_issuer.clone(),
                keys.clone(),
                algorithms.clone(),
                server_nonces.clone(),
            );
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
                assert!(!headers.to_ascii_lowercase().contains("proxy-authorization:"));
                let length = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").map(|length| length.parse::<usize>().unwrap())).unwrap_or(0);
                assert!(length <= 4096);
                while raw.len() < header_end + length {
                    let mut buffer = [0; 2048];
                    let read = socket.read(&mut buffer).await.unwrap();
                    assert!(read > 0);
                    raw.extend_from_slice(&buffer[..read]);
                }
                let path = headers.split_whitespace().nth(1).unwrap();
                let (content_type, body) = {
                    let claims = *claims.lock().unwrap();
                    let twist = twist.lock().unwrap();
                    match path {
                        "/.well-known/openid-configuration" => ("application/json", json!({"issuer": issuer, "authorization_endpoint": format!("{issuer}/authorize"), "token_endpoint": format!("{issuer}/token"), "userinfo_endpoint": format!("{issuer}/userinfo"), "jwks_uri": format!("{issuer}/jwks"), "response_types_supported": ["code"], "subject_types_supported": ["public"], "id_token_signing_alg_values_supported": algorithms, "authorization_response_iss_parameter_supported": true}).to_string()),
                        "/jwks" => {
                            jwks.fetch_add(1, Ordering::SeqCst);
                            ("application/json", keys.jwks(twist.rotated).to_string())
                        }
                        "/token" => {
                            let form: HashMap<_, _> = form_urlencoded::parse(&raw[header_end..]).into_owned().collect();
                            assert_eq!(form["code"], "valid-code");
                            assert_eq!(form["redirect_uri"], "https://meter.example/auth/oidc/callback");
                            let challenge = URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, form["code_verifier"].as_bytes()));
                            let nonce = nonces.lock().unwrap().get(&challenge).cloned().expect("PKCE verifier matches a started transaction");
                            assert!(headers.contains(&format!("authorization: Basic {}\r\n", STANDARD.encode("meter:secret"))), "{headers}");
                            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                            let mut token_claims = json!({"iss": issuer, "aud": "meter", "azp": if claims.wrong_party {"other-client"} else {"meter"}, "sub": "operator", "iat": now, "exp": now + 300, "nonce": if claims.wrong_nonce { "invalid" } else { nonce.as_str() }, "at_hash": at_hash("access")});
                            for (key, value) in twist.claims.as_ref().and_then(Value::as_object).into_iter().flatten() {
                                token_claims[key] = value.clone();
                            }
                            let header = twist.header.clone().unwrap_or_else(|| match (twist.rotated, twist.unknown_kid) {
                                (_, true) => json!({"alg": "ES256", "kid": "gone"}),
                                (true, _) => json!({"alg": "ES256", "kid": "rotated", "typ": "JWT"}),
                                _ => json!({"alg": "RS256", "kid": "test-key"}),
                            });
                            ("application/json", json!({"access_token": "access", "token_type": "Bearer", "id_token": keys.sign(&header, &token_claims)}).to_string())
                        }
                        "/userinfo" => {
                            assert!(headers.contains("authorization: Bearer access\r\n"), "{headers}");
                            let info = json!({"sub": if claims.wrong_subject { "other" } else { "operator" }, "name": "Example Operator", "groups": if claims.denied_group { vec!["outsiders"] } else { vec!["operators"] }});
                            match &twist.signed_userinfo {
                                Some(extra) => {
                                    let mut info = info;
                                    for (key, value) in extra.as_object().unwrap() {
                                        info[key] = value.clone();
                                    }
                                    ("application/jwt", keys.sign(&json!({"alg": "RS256", "kid": "test-key"}), &info))
                                }
                                None => ("application/json", info.to_string()),
                            }
                        }
                        _ => panic!("unexpected provider endpoint {path}"),
                    }
                };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            });
        }
        while let Some(result) = connections.join_next().await {
            result.unwrap();
        }
    });
    ProviderDouble {
        oidc,
        issuer,
        claims,
        nonces,
        twist,
        jwks_requests,
        stop,
        server,
    }
}

impl ProviderDouble {
    async fn login(&self, claims: Claims) -> Result<Identity, ConfigError> {
        let started = self
            .oidc
            .start("192.0.2.1".parse().unwrap(), "challenge".into(), None)
            .await
            .unwrap();
        let fields = tests::query_fields(&started.url);
        self.nonces
            .lock()
            .unwrap()
            .insert(fields["code_challenge"].clone(), fields["nonce"].clone());
        *self.claims.lock().unwrap() = claims;
        self.oidc
            .finish(
                &fields["state"],
                &started.browser,
                "valid-code",
                Some(&self.issuer),
            )
            .await
    }
    async fn stop(self) {
        self.stop.send(()).unwrap();
        self.server.await.unwrap();
    }
}

#[tokio::test]
async fn signed_provider_exchange_checks_nonce_subject_group_and_pkce() {
    let provider = provider_double("localhost", &["RS256"], Proxy::default()).await;
    for scenario in 0..5 {
        let result = provider
            .login(Claims {
                wrong_nonce: scenario == 1,
                wrong_party: scenario == 4,
                wrong_subject: scenario == 2,
                denied_group: scenario == 3,
            })
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
    provider.stop().await;
}

#[tokio::test]
async fn forged_or_misbound_tokens_are_refused_and_rotation_refetches_keys_once() {
    let provider = provider_double(
        "localhost",
        &["RS256", "ES256", "HS256", "none"],
        Proxy::default(),
    )
    .await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let refused: [(&str, Twist); 10] = [
        (
            "second audience",
            Twist {
                claims: Some(json!({"aud": ["meter", "other"]})),
                ..Twist::default()
            },
        ),
        (
            "foreign audience",
            Twist {
                claims: Some(json!({"aud": "other"})),
                ..Twist::default()
            },
        ),
        (
            "foreign issuer",
            Twist {
                claims: Some(json!({"iss": "https://evil.example"})),
                ..Twist::default()
            },
        ),
        (
            "expired",
            Twist {
                claims: Some(json!({"exp": now - 1})),
                ..Twist::default()
            },
        ),
        (
            "not yet valid",
            Twist {
                claims: Some(json!({"nbf": now + 3600})),
                ..Twist::default()
            },
        ),
        (
            "access token binding",
            Twist {
                claims: Some(json!({"at_hash": at_hash("other")})),
                ..Twist::default()
            },
        ),
        (
            "unsigned",
            Twist {
                header: Some(json!({"alg": "none"})),
                ..Twist::default()
            },
        ),
        (
            "client-secret MAC",
            Twist {
                header: Some(json!({"alg": "HS256", "kid": "test-key"})),
                ..Twist::default()
            },
        ),
        (
            "unknown kid after refetch",
            Twist {
                unknown_kid: true,
                ..Twist::default()
            },
        ),
        (
            "signed user information for another client",
            Twist {
                signed_userinfo: Some(json!({"iss": provider.issuer, "aud": "other"})),
                ..Twist::default()
            },
        ),
    ];
    for (name, twist) in refused {
        *provider.twist.lock().unwrap() = twist;
        assert!(
            provider.login(Claims::default()).await.is_err(),
            "{name} authenticated"
        );
    }
    *provider.twist.lock().unwrap() = Twist {
        signed_userinfo: Some(json!({"iss": provider.issuer, "aud": "meter"})),
        ..Twist::default()
    };
    assert_eq!(
        provider.login(Claims::default()).await.unwrap().subject,
        "oidc:operator"
    );
    let before = provider.jwks_requests.load(Ordering::SeqCst);
    assert_eq!(before, 2);
    *provider.twist.lock().unwrap() = Twist {
        rotated: true,
        ..Twist::default()
    };
    let (first, second) = tokio::join!(
        provider.login(Claims::default()),
        provider.login(Claims::default())
    );
    assert_eq!(first.unwrap().subject, "oidc:operator");
    assert_eq!(second.unwrap().subject, "oidc:operator");
    assert_eq!(provider.jwks_requests.load(Ordering::SeqCst), before + 1);
    provider.stop().await;
}

#[tokio::test]
async fn provider_traffic_uses_the_https_proxy() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let tunnels = Arc::new(AtomicUsize::new(0));
    let counted = tunnels.clone();
    tokio::spawn(async move {
        loop {
            let (mut client, _) = proxy.accept().await.unwrap();
            let counted = counted.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(client.read_u8().await.unwrap());
                }
                let head = String::from_utf8(head).unwrap();
                assert!(
                    head.to_ascii_lowercase()
                        .contains("proxy-authorization: basic dxnlcjpwyxnz\r\n")
                );
                let target = head
                    .strip_prefix("CONNECT provider.test:")
                    .unwrap()
                    .split(' ')
                    .next()
                    .unwrap()
                    .to_owned();
                counted.fetch_add(1, Ordering::SeqCst);
                let mut upstream = TcpStream::connect(format!("127.0.0.1:{target}"))
                    .await
                    .unwrap();
                client
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    let provider = provider_double(
        "provider.test",
        &["RS256"],
        Proxy::new("", &format!("http://user:pass@{proxy_address}"), ""),
    )
    .await;
    assert_eq!(
        provider.login(Claims::default()).await.unwrap().subject,
        "oidc:operator"
    );
    assert_eq!(tunnels.load(Ordering::SeqCst), 4);
    provider.stop().await;
}

#[tokio::test]
async fn unadvertised_signing_algorithms_are_refused() {
    for algorithms in [vec![], vec!["HS256", "ES512"]] {
        let provider = provider_double("localhost", &algorithms, Proxy::default()).await;
        assert!(provider.login(Claims::default()).await.is_err());
        provider.stop().await;
    }
}
