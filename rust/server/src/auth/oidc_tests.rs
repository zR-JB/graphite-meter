use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
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

use crate::{config::AuthMode, test_identity};

#[derive(Clone, Copy, Default)]
struct Claims {
    wrong_nonce: bool,
    wrong_subject: bool,
    denied_group: bool,
}

#[derive(Default)]
struct Twist {
    unavailable: bool,
    rotated: bool,
    unknown_kid: bool,
    claims: Option<Value>,
    header: Option<Value>,
    signed_userinfo: Option<Value>,
    name: Option<String>,
    /// Discovery members that replace the double's own.
    metadata: Option<Value>,
    /// Token response members that replace the double's own.
    tokens: Option<Value>,
    /// User information members that replace the double's own.
    userinfo: Option<Value>,
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
        let PrivateKeyDer::Pkcs1(der) = PrivateKeyDer::from_pem_slice(&generated.stdout).unwrap() else {
            panic!("openssl did not emit PKCS#1");
        };
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        Self {
            rsa: RsaKeyPair::from_der(der.secret_pkcs1_der()).unwrap(),
            ec: EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap(),
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
            Some("ES256") => self.ec.sign(&self.rng, message.as_bytes()).unwrap().as_ref().to_vec(),
            Some("HS256") => ring::hmac::sign(
                &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"s3cret~*"),
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

/// A provider that answers as its twist says, and the client that signs in with it.
struct Double {
    oidc: Oidc,
    issuer: String,
    algorithms: Vec<String>,
    keys: Keys,
    claims: Mutex<Claims>,
    nonces: Mutex<HashMap<String, String>>,
    twist: Mutex<Twist>,
    jwks_requests: AtomicUsize,
    server: Mutex<Option<(tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>)>>,
}

impl Double {
    async fn answer(self: Arc<Self>, request: Request<Incoming>) -> http::Result<Response<String>> {
        let (request, mut body) = request.into_parts();
        let mut bytes = Vec::new();
        while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
            bytes.extend_from_slice(&frame.unwrap().into_data().unwrap());
        }
        let headers = &request.headers;
        assert!(!headers.contains_key(header::PROXY_AUTHORIZATION));
        // As Go's client asks: naming itself, and with no Accept.
        assert_eq!(
            headers[header::USER_AGENT],
            format!("graphite-meter/{}", crate::config::ENGINE_VERSION)
        );
        assert!(!headers.contains_key(header::ACCEPT));
        let (claims, twist, issuer) = (*self.claims.lock().unwrap(), self.twist.lock().unwrap(), &self.issuer);
        let with = |mut value: Value, twist: &Option<Value>| {
            for (key, member) in twist.iter().filter_map(Value::as_object).flatten() {
                value[key] = member.clone();
            }
            value
        };
        let (content_type, body) = match request.uri.path() {
            "/.well-known/openid-configuration" if twist.unavailable => ("application/json", "{}".into()),
            "/.well-known/openid-configuration" => {
                let metadata = json!({"issuer": issuer, "authorization_endpoint": format!("{issuer}/authorize"),
                    "token_endpoint": format!("{issuer}/token"), "userinfo_endpoint": format!("{issuer}/userinfo"),
                    "jwks_uri": format!("{issuer}/jwks"), "response_types_supported": ["code"],
                    "subject_types_supported": ["public"], "id_token_signing_alg_values_supported": self.algorithms,
                    "authorization_response_iss_parameter_supported": true});
                ("application/json", with(metadata, &twist.metadata).to_string())
            }
            "/jwks" => {
                assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
                self.jwks_requests.fetch_add(1, Ordering::SeqCst);
                ("application/json", self.keys.jwks(twist.rotated).to_string())
            }
            "/token" => {
                // Sorted, and escaped as Go's url.Values.Encode escapes them.
                let names: Vec<_> = form_urlencoded::parse(&bytes).map(|(name, _)| name).collect();
                assert_eq!(names, ["code", "code_verifier", "grant_type", "redirect_uri"]);
                assert!(bytes.starts_with(b"code=valid~code%2A&"));
                let form: HashMap<_, _> = form_urlencoded::parse(&bytes).into_owned().collect();
                assert_eq!(form["redirect_uri"], "https://meter.example/auth/oidc/callback");
                let challenge = ring::digest::digest(&ring::digest::SHA256, form["code_verifier"].as_bytes());
                let nonce = self.nonces.lock().unwrap()[&URL_SAFE_NO_PAD.encode(challenge)].clone();
                let basic = format!("Basic {}", STANDARD.encode("meter:s3cret~%2A"));
                assert_eq!(headers[header::AUTHORIZATION], basic);
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                let nonce = if claims.wrong_nonce { "invalid" } else { &nonce };
                let token_claims = json!({"iss": issuer, "aud": "meter", "sub": "operator", "iat": now,
                    "exp": now + 300, "nonce": nonce, "at_hash": at_hash("access")});
                let header = match (&twist.header, twist.rotated, twist.unknown_kid) {
                    (Some(header), ..) => header.clone(),
                    (_, _, true) => json!({"alg": "ES256", "kid": "gone"}),
                    (_, true, _) => json!({"alg": "ES256", "kid": "rotated", "typ": "JWT"}),
                    _ => json!({"alg": "RS256", "kid": "test-key"}),
                };
                let id_token = self.keys.sign(&header, &with(token_claims, &twist.claims));
                let tokens = json!({"access_token": "access", "token_type": "Bearer", "id_token": id_token});
                ("application/json", with(tokens, &twist.tokens).to_string())
            }
            "/userinfo" => {
                assert_eq!(headers[header::AUTHORIZATION], "Bearer access");
                let name = twist.name.as_deref().unwrap_or("Example Operator");
                let groups = [if claims.denied_group { "outsiders" } else { "operators" }];
                let subject = if claims.wrong_subject { "other" } else { "operator" };
                let info = json!({"sub": subject, "name": name, "groups": groups});
                let info = with(with(info, &twist.userinfo), &twist.signed_userinfo);
                let header = json!({"alg": "RS256", "kid": "test-key"});
                match twist.signed_userinfo {
                    Some(_) => ("application/jwt", self.keys.sign(&header, &info)),
                    None => ("application/json", info.to_string()),
                }
            }
            path => panic!("unexpected provider endpoint {path}"),
        };
        Response::builder()
            .header(header::CONTENT_TYPE, content_type)
            .body(body)
    }
    async fn login(&self, claims: Claims) -> Result<Identity, Reason> {
        let tx = self.begin(claims).await?;
        self.oidc.complete(&tx, "valid~code*").await
    }
    async fn begin(&self, claims: Claims) -> Result<Transaction, Reason> {
        assert!(self.oidc.ready().is_some() || self.oidc.discover().await.is_ok());
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
        let tx = self
            .oidc
            .take(&fields["state"], &started.browser, Some(&self.issuer))
            .map_err(|(reason, _)| reason)?;
        assert_eq!(tx.challenge, "challenge");
        Ok(tx)
    }
    /// Stops the server, failing the test where the double's own checks failed.
    async fn stop(&self) {
        let (stop, server) = self.server.lock().unwrap().take().unwrap();
        stop.send(()).unwrap();
        server.await.unwrap();
    }
}

async fn provider_double(host: &str, algorithms: &[&str], proxy: Proxy) -> Arc<Double> {
    let (certificate, key) = test_identity::generate_identity(host).unwrap();
    let certificates = CertificateDer::pem_slice_iter(certificate.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certificates.clone(), key)
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("https://{host}:{}", listener.local_addr().unwrap().port());
    let mut oidc = Oidc::new(
        &AuthConfig {
            mode: AuthMode::Oidc,
            public_url: "https://meter.example".into(),
            oidc_issuer: issuer.clone(),
            oidc_client_id: "meter".into(),
            oidc_client_secret: "s3cret~*".into(),
            oidc_allowed_groups: vec!["operators".into()],
            ..AuthConfig::default()
        },
        Arc::new(crate::auth::logging::SecurityLog::default()),
    )
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(certificates);
    let client_tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    oidc.http = ProviderHttp {
        tls: TlsConnector::from(Arc::new(client_tls)),
        proxy,
    };
    let double = Arc::new(Double {
        oidc,
        issuer,
        algorithms: algorithms.iter().map(|alg| (*alg).to_owned()).collect(),
        keys: Keys::new(),
        claims: Mutex::default(),
        nonces: Mutex::default(),
        twist: Mutex::default(),
        jwks_requests: AtomicUsize::new(0),
        server: Mutex::default(),
    });
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let (acceptor, answering) = (TlsAcceptor::from(Arc::new(tls)), double.clone());
    let server = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let socket = tokio::select! {
                _ = &mut stopped => break,
                accepted = listener.accept() => accepted.unwrap().0,
            };
            let (acceptor, double) = (acceptor.clone(), answering.clone());
            connections.spawn(async move {
                let socket = TokioIo::new(acceptor.accept(socket).await.unwrap());
                let service = service_fn(move |request| double.clone().answer(request));
                http1::Builder::new().serve_connection(socket, service).await.unwrap();
            });
        }
        while let Some(result) = connections.join_next().await {
            result.unwrap();
        }
    });
    *double.server.lock().unwrap() = Some((stop, server));
    double
}

#[tokio::test]
async fn signed_provider_exchange_checks_nonce_subject_group_and_pkce() {
    let provider = provider_double("localhost", &["RS256"], Proxy::default()).await;
    // Go's CleanText blanks the control, keeps 63 characters and an ellipsis, then trims.
    let display_name = format!("{}…", "é".repeat(61));
    provider.twist.lock().unwrap().name = Some(format!(
        "\u{009b} {}\u{202e}🙂{}",
        "é".repeat(127),
        "x".repeat(256 * 1024)
    ));
    for scenario in 0..4 {
        let result = provider
            .login(Claims {
                wrong_nonce: scenario == 1,
                wrong_subject: scenario == 2,
                denied_group: scenario == 3,
            })
            .await;
        if scenario == 0 {
            let identity = result.unwrap();
            assert_eq!(identity.subject, "oidc:operator");
            assert_eq!(identity.name, display_name);
            assert!(identity.name.capacity() <= 256);
        } else if scenario == 3 {
            assert!(matches!(result, Err(Reason::GroupDenied)));
        } else {
            assert!(result.is_err(), "scenario {scenario} authenticated");
        }
    }
    provider.stop().await;
}

#[tokio::test]
async fn callbacks_past_the_concurrent_exchanges_wait_within_gos_deadline() {
    let provider = provider_double("localhost", &["RS256"], Proxy::default()).await;
    let busy = provider
        .oidc
        .exchanges
        .acquire_many(MAX_EXCHANGES as u32)
        .await
        .unwrap();
    let tx = provider.begin(Claims::default()).await.unwrap();
    let mut callback = Box::pin(provider.oidc.complete(&tx, "valid~code*"));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut callback)
            .await
            .is_err(),
        "a callback past the concurrent exchanges was refused"
    );
    drop(busy);
    assert_eq!(callback.await.unwrap().subject, "oidc:operator");

    let busy = provider
        .oidc
        .exchanges
        .acquire_many(MAX_EXCHANGES as u32)
        .await
        .unwrap();
    let tx = provider.begin(Claims::default()).await.unwrap();
    tokio::time::pause();
    let waited = Instant::now();
    assert!(matches!(
        provider.oidc.complete(&tx, "valid~code*").await,
        Err(Reason::TokenExchange)
    ));
    // Tokio's timer rounds up to its next millisecond.
    assert!((CALLBACK_DEADLINE..=CALLBACK_DEADLINE + Duration::from_millis(1)).contains(&waited.elapsed()));
    tokio::time::resume();
    drop(busy);
    provider.stop().await;
}

#[tokio::test]
async fn forged_or_misbound_tokens_are_refused_and_rotation_refetches_keys_once() {
    let provider = provider_double("localhost", &["RS256", "ES256", "HS256", "none"], Proxy::default()).await;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let refused: [(&str, Twist); 9] = [
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
        assert!(provider.login(Claims::default()).await.is_err(), "{name} authenticated");
    }
    *provider.twist.lock().unwrap() = Twist {
        signed_userinfo: Some(
            json!({"iss": provider.issuer, "aud": "meter", "name": "\u{009b}München \u{202e}العربية\u{2069} 👩\u{200d}💻"}),
        ),
        ..Twist::default()
    };
    let identity = provider.login(Claims::default()).await.unwrap();
    assert_eq!(identity.subject, "oidc:operator");
    assert_eq!(identity.name, "München  العربية  👩\u{200d}💻");
    let before = provider.jwks_requests.load(Ordering::SeqCst);
    assert_eq!(before, 2);
    *provider.twist.lock().unwrap() = Twist {
        rotated: true,
        ..Twist::default()
    };
    let (first, second) = tokio::join!(provider.login(Claims::default()), provider.login(Claims::default()));
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
                let mut upstream = TcpStream::connect(format!("127.0.0.1:{target}")).await.unwrap();
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
async fn rs256_signs_in_only_where_go_oidc_supports_no_advertised_algorithm() {
    for (algorithms, signs_in) in [(vec![], true), (vec!["HS256"], true), (vec!["HS256", "ES512"], false)] {
        let provider = provider_double("localhost", &algorithms, Proxy::default()).await;
        assert_eq!(
            provider.login(Claims::default()).await.is_ok(),
            signs_in,
            "{algorithms:?}"
        );
        provider.stop().await;
    }
}

/// Token, ID token and user information members as x/oauth2, go-oidc and Go's decoder read them.
#[tokio::test]
async fn provider_members_read_as_go_reads_them() {
    let provider = provider_double("localhost", &["RS256"], Proxy::default()).await;
    let go_signs_in = json!({"SUB": "operator", "sub": null, "email_verified": "true", "groups": ["operators", null]});
    for (tokens, claims, userinfo, signs_in) in [
        (
            json!({"error": null, "expires_in": "300"}),
            json!({"at_hash": null}),
            go_signs_in,
            true,
        ),
        (json!({"expires_in": 300.5}), json!({}), json!({}), false),
        (json!({"token_type": 7}), json!({}), json!({}), false),
        (json!({}), json!({"_claim_names": {"groups": "a"}}), json!({}), false),
        (json!({}), json!({}), json!({"email": 123}), false),
        (json!({}), json!({}), json!({"email_verified": "yes"}), false),
    ] {
        let case = format!("{tokens} {claims} {userinfo}");
        *provider.twist.lock().unwrap() = Twist {
            tokens: Some(tokens),
            claims: Some(claims),
            userinfo: Some(userinfo),
            ..Twist::default()
        };
        assert_eq!(provider.login(Claims::default()).await.is_ok(), signs_in, "{case}");
    }
    provider.stop().await;
}

#[tokio::test]
async fn discovery_refuses_only_an_authorization_endpoint_that_breaks_sign_in() {
    let provider = provider_double("localhost", &["RS256"], Proxy::default()).await;
    let issuer = provider.issuer.clone();
    for (metadata, discovered) in [
        // Sign-in pages name the authorization origin in their form-action; port 0 is no origin a browser can post to.
        (
            json!({"authorization_endpoint": "https://idp.example:0/authorize"}),
            false,
        ),
        (
            json!({"authorization_endpoint": format!("{issuer}/authorize#x")}),
            false,
        ),
        // Go's TestOIDCDiscoveryToleratesMistypedOptionalMetadata, and fragments Go's client leaves out.
        (
            json!({"authorization_response_iss_parameter_supported": "yes", "token_endpoint": format!("{issuer}/token#x"),
                "userinfo_endpoint": format!("{issuer}/userinfo#"), "jwks_uri": format!("{issuer}/jwks#x")}),
            true,
        ),
        // go-oidc's providerJSON, as Go's decoder fills it: a member in any case, the last winning, where null
        // leaves a string as it was and a null element is empty; one of another type refuses the document.
        (json!({"Issuer": issuer, "issuer": null}), true),
        (json!({"id_token_signing_alg_values_supported": [null, "RS256"]}), true),
        (json!({"device_authorization_endpoint": 5}), false),
    ] {
        provider.twist.lock().unwrap().metadata = Some(metadata);
        assert_eq!(provider.oidc.discover().await.is_ok(), discovered);
    }
    assert!(!provider.oidc.ready().unwrap().issuer_parameter);
    assert_eq!(
        provider.login(Claims::default()).await.unwrap().subject,
        "oidc:operator"
    );
    provider.stop().await;
}

#[tokio::test]
async fn unavailable_provider_refuses_logins_until_discovery_recovers_once() {
    let provider = provider_double("localhost", &["RS256"], Proxy::default()).await;
    provider.twist.lock().unwrap().unavailable = true;
    assert!(provider.oidc.discover().await.is_err());
    let address = "192.0.2.1".parse().unwrap();
    assert!(matches!(
        provider.oidc.start(address, String::new(), None).await,
        Err(Reason::ProviderNotReady)
    ));
    provider.twist.lock().unwrap().unavailable = false;
    provider.oidc.retry_discovery().await;
    let ready = provider.oidc.ready().unwrap().clone();
    provider.oidc.retry_discovery().await;
    assert!(Arc::ptr_eq(&ready, provider.oidc.ready().unwrap()));
    assert_eq!(
        provider.jwks_requests.load(Ordering::SeqCst),
        0,
        "keys wait for the first token"
    );
    assert!(provider.oidc.start(address, String::new(), None).await.is_ok());
    provider.stop().await;
}

pub(in crate::auth) fn ready() -> Oidc {
    ready_with(true)
}

fn ready_with(issuer_parameter: bool) -> Oidc {
    let oidc = discovered("https://identity.example/authorize", issuer_parameter);
    assert!(oidc.ready().is_some());
    oidc
}

/// A client that discovered metadata naming `authorization_endpoint`, ready if discovery accepted it.
pub(in crate::auth) fn discovered(authorization_endpoint: &str, issuer_parameter: bool) -> Oidc {
    let oidc = Oidc::new(
        &AuthConfig {
            mode: AuthMode::Oidc,
            public_url: "https://meter.example".into(),
            oidc_issuer: "https://identity.example".into(),
            oidc_client_id: "meter~*".into(),
            oidc_client_secret: "secret".into(),
            oidc_allowed_groups: vec!["operators".into()],
            ..AuthConfig::default()
        },
        Arc::new(crate::auth::logging::SecurityLog::default()),
    )
    .unwrap();
    let metadata = serde_json::from_value(serde_json::json!({
        "issuer": "https://identity.example",
        "authorization_endpoint": authorization_endpoint,
        "token_endpoint": "https://identity.example/token",
        "userinfo_endpoint": "https://identity.example/userinfo",
        "jwks_uri": "https://identity.example/jwks",
        "id_token_signing_alg_values_supported": ["RS256"],
        "authorization_response_iss_parameter_supported": issuer_parameter
    }))
    .unwrap();
    if let Ok(provider) = Provider::new(&metadata, "https://identity.example") {
        assert!(oidc.provider.set(Arc::new(provider)).is_ok());
    }
    oidc
}

pub(in crate::auth) fn query_fields(url: &str) -> HashMap<String, String> {
    form_urlencoded::parse(url.split_once('?').unwrap().1.as_bytes())
        .into_owned()
        .collect()
}

#[tokio::test]
async fn authorization_is_pkce_bound_bounded_and_consumed_before_browser_validation() {
    let oidc = ready();
    let address = "192.0.2.1".parse().unwrap();
    let started = oidc.start(address, String::new(), None).await.unwrap();
    let names: Vec<_> = form_urlencoded::parse(started.url.split_once('?').unwrap().1.as_bytes()).collect();
    assert!(names.is_sorted_by_key(|(name, _)| name.clone()), "{names:?}");
    // Go's url.QueryEscape keeps '~' and escapes '*'.
    assert!(started.url.contains("?client_id=meter~%2A&"));
    let fields = query_fields(&started.url);
    assert_eq!(fields["code_challenge_method"], "S256");
    assert_eq!(fields["response_type"], "code");
    assert_eq!(fields["redirect_uri"], "https://meter.example/auth/oidc/callback");
    let issuer = Some("https://identity.example");
    let refused = oidc.take(&fields["state"], "wrong-browser", issuer).err();
    assert_eq!(refused, Some((Reason::TransactionReplay, String::new())));
    assert!(oidc.transactions.lock().unwrap().is_empty());
    assert!(oidc.take(&fields["state"], &started.browser, issuer).is_err());
    for _ in 0..8 {
        oidc.start(address, String::new(), None).await.unwrap();
    }
    assert!(oidc.start(address, String::new(), None).await.is_err());
    assert_eq!(oidc.transactions.lock().unwrap().len(), 8);
}

#[tokio::test]
async fn transactions_charge_wider_ipv6_shares_before_global_capacity() {
    let oidc = ready();
    for subnet in 0..2 {
        for host in 1..=8 {
            let address = format!("2001:db8:1:{subnet:x}::{host}").parse().unwrap();
            oidc.start(address, String::new(), None).await.unwrap();
        }
    }
    assert!(
        oidc.start("2001:db8:1:2::1".parse().unwrap(), String::new(), None)
            .await
            .is_err()
    );
    assert!(
        oidc.start("2001:db8:2::1".parse().unwrap(), String::new(), None)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn mismatched_response_issuer_cannot_redeem_a_code() {
    let oidc = ready();
    let started = oidc
        .start("192.0.2.2".parse().unwrap(), "challenge".into(), None)
        .await
        .unwrap();
    let state = query_fields(&started.url)["state"].clone();
    let refused = oidc.take(&state, &started.browser, Some("https://other.example")).err();
    assert_eq!(refused, Some((Reason::ResponseIssuer, "challenge".into())));
    assert!(oidc.transactions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_empty_response_issuer_is_absent_like_go() {
    for advertised in [false, true] {
        let oidc = ready_with(advertised);
        let started = oidc
            .start("192.0.2.3".parse().unwrap(), "challenge".into(), None)
            .await
            .unwrap();
        let state = query_fields(&started.url)["state"].clone();
        let taken = oidc
            .take(&state, &started.browser, Some(""))
            .map(|_| ())
            .map_err(|(reason, _)| reason);
        assert_eq!(
            taken,
            if advertised {
                Err(Reason::ResponseIssuer)
            } else {
                Ok(())
            }
        );
    }
}

#[test]
fn provider_endpoints_require_https_without_embedded_credentials() {
    for endpoint in [
        "http://identity.example/token",
        "https://user@identity.example/token",
        "https://identity.example/token#fragment",
    ] {
        assert!(valid_url(endpoint).is_err());
    }
    assert!(valid_url("https://identity.example/token").is_ok());
}
