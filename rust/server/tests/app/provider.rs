//! A fake OIDC provider over TLS for the sign-in tests, which run in a child process trusting it.

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD as B64},
};
use bytes::Bytes;
use graphite_meter_proto::approval::challenge;
use graphite_meter_server::app::query;
use graphite_meter_testkit::{Identity, Scratch};
use http::{Request, Response, header};
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use ring::{
    rand::SystemRandom,
    signature::{self, EcdsaKeyPair, Ed25519KeyPair, KeyPair, RsaKeyPair},
};
use rustls::pki_types::{PrivateKeyDer, pem::PemObject};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
};
use tokio_rustls::TlsAcceptor;

pub(super) const CLIENT_ID: &str = "meter";
pub(super) const SECRET: &str = "s3cret~*";
/// Set in a child process; with the provider's certificate and key files beside `SSL_CERT_FILE`.
const CHILD: &str = "GRAPHITE_METER_TEST_CHILD";
const CERTIFICATE: &str = "GRAPHITE_METER_TEST_CERTIFICATE";
const KEY: &str = "GRAPHITE_METER_TEST_KEY";

/// Whether this is the child process running `test`; otherwise runs it in one, trusting a fresh provider identity,
/// and expects it to pass.
pub(super) fn child(test: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let (scratch, identity) = (Scratch::new().unwrap(), Identity::generate().unwrap());
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([test, "--exact", "--nocapture"]).env(CHILD, "1");
    for proxy in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        command.env_remove(proxy);
    }
    command
        .env("SSL_CERT_FILE", scratch.file("ca.pem", &identity.ca).unwrap())
        .env("SSL_CERT_DIR", scratch.dir("none").unwrap())
        .env(CERTIFICATE, scratch.file("leaf.pem", &identity.certificate).unwrap())
        .env(KEY, scratch.file("leaf.key", &identity.key).unwrap());
    assert!(command.status().unwrap().success(), "{test} failed in its child process");
    false
}

/// What the provider changes in its answers: members merged into its metadata, the ID token's header and claims and
/// the user information, where `null` removes one; and `=` padding on its keys' members.
#[derive(Default)]
pub(super) struct Twist {
    pub padded_keys: bool,
    pub metadata: Value,
    pub header: Value,
    pub claims: Value,
    pub userinfo: Value,
}

struct Shared {
    issuer: String,
    keys: Signers,
    /// Each code's PKCE challenge and nonce.
    codes: Mutex<HashMap<String, (String, String)>>,
    twist: Mutex<Twist>,
    ready: AtomicBool,
    key_sets: AtomicUsize,
    /// Where token and user information requests announce themselves while held.
    held: Mutex<Option<Held>>,
}

/// A held request's path, and the sender that lets its answer go.
type Held = mpsc::UnboundedSender<(String, oneshot::Sender<()>)>;

pub(super) struct Provider(Arc<Shared>);

impl Provider {
    /// Serves at `https://localhost:<port>` until the test ends.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("https://localhost:{}", listener.local_addr().unwrap().port());
        Self::serve(listener, issuer)
    }

    /// Serves on `listener` as `issuer`.
    pub fn serve(listener: TcpListener, issuer: String) -> Self {
        let read = |name| std::fs::read_to_string(std::env::var_os(name).unwrap()).unwrap();
        let identity = Identity {
            ca: String::new(),
            certificate: read(CERTIFICATE),
            key: read(KEY),
        };
        let acceptor = TlsAcceptor::from(Arc::new(identity.server(&[])));
        let shared = Arc::new(Shared {
            issuer,
            keys: Signers::new(),
            codes: Mutex::default(),
            twist: Mutex::default(),
            ready: AtomicBool::new(true),
            key_sets: AtomicUsize::new(0),
            held: Mutex::default(),
        });
        let serving = shared.clone();
        tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let (acceptor, shared) = (acceptor.clone(), serving.clone());
                tokio::spawn(async move {
                    let Ok(socket) = acceptor.accept(socket).await else { return };
                    let service = service_fn(move |request| shared.clone().answer(request));
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .await;
                });
            }
        });
        Self(shared)
    }

    pub fn issuer(&self) -> &str {
        &self.0.issuer
    }

    /// Answers discovery, or 503 until set ready.
    pub fn set_ready(&self, ready: bool) {
        self.0.ready.store(ready, Ordering::Relaxed);
    }

    pub fn twist(&self, twist: Twist) {
        *self.0.twist.lock().unwrap() = twist;
    }

    /// From now on holds each token and user information answer until the test releases it.
    pub fn hold(&self) -> mpsc::UnboundedReceiver<(String, oneshot::Sender<()>)> {
        let (announce, held) = mpsc::unbounded_channel();
        *self.0.held.lock().unwrap() = Some(announce);
        held
    }

    /// How many times the server fetched the key set.
    pub fn key_sets(&self) -> usize {
        self.0.key_sets.load(Ordering::Relaxed)
    }

    /// Signs in at the sign-in URL `location` sent the browser to, returning the callback's code and state.
    pub fn authorize(&self, location: &str) -> (String, String) {
        let (endpoint, query) = location.split_once('?').unwrap();
        assert_eq!(endpoint, format!("{}/authorize", self.0.issuer));
        let fields = query::form(query).unwrap();
        let names: Vec<_> = fields.iter().map(|(name, _)| name.as_str()).collect();
        let sorted = ["client_id", "code_challenge", "code_challenge_method", "nonce", "redirect_uri"];
        assert_eq!(names, [&sorted[..], &["response_type", "scope", "state"]].concat());
        let fields: HashMap<_, _> = fields.into_iter().collect();
        assert_eq!(fields["client_id"], CLIENT_ID);
        assert_eq!(fields["code_challenge_method"], "S256");
        assert_eq!(fields["redirect_uri"], "https://meter.example/auth/oidc/callback");
        assert_eq!(
            (fields["response_type"].as_str(), fields["scope"].as_str()),
            ("code", "openid profile groups")
        );
        let code = format!("code-{}", fields["state"]);
        let bound = (fields["code_challenge"].clone(), fields["nonce"].clone());
        self.0.codes.lock().unwrap().insert(code.clone(), bound);
        (code, fields["state"].clone())
    }
}

impl Shared {
    async fn answer(self: Arc<Self>, request: Request<Incoming>) -> Result<Response<Full<Bytes>>, http::Error> {
        let (head, body) = request.into_parts();
        let body = body.collect().await.unwrap().to_bytes();
        let agent = head.headers[header::USER_AGENT].to_str().unwrap();
        assert!(agent.starts_with("graphite-meter/"), "{agent}");
        let held = self.held.lock().unwrap().clone();
        if let Some(held) = held.filter(|_| matches!(head.uri.path(), "/token" | "/userinfo")) {
            let (release, released) = oneshot::channel();
            held.send((head.uri.path().to_owned(), release)).unwrap();
            let _ = released.await;
        }
        let issuer = &self.issuer;
        let (status, document) = match head.uri.path() {
            "/.well-known/openid-configuration" if !self.ready.load(Ordering::Relaxed) => (503, json!({})),
            "/.well-known/openid-configuration" => (
                200,
                merge(
                    json!({
                        "issuer": issuer,
                        "authorization_endpoint": format!("{issuer}/authorize"),
                        "token_endpoint": format!("{issuer}/token"),
                        "userinfo_endpoint": format!("{issuer}/userinfo"),
                        "jwks_uri": format!("{issuer}/jwks"),
                        "id_token_signing_alg_values_supported":
                            ["RS256", "RS384", "RS512", "PS256", "PS384", "PS512", "ES256", "ES384", "EdDSA", "HS256"],
                        "authorization_response_iss_parameter_supported": true,
                    }),
                    &self.twist.lock().unwrap().metadata,
                ),
            ),
            "/jwks" => {
                assert_eq!(head.headers[header::CACHE_CONTROL], "no-cache");
                self.key_sets.fetch_add(1, Ordering::Relaxed);
                (200, self.keys.jwks(self.twist.lock().unwrap().padded_keys))
            }
            "/token" => self.token(&head.headers, &body),
            "/userinfo" => {
                assert_eq!(head.headers[header::AUTHORIZATION], "Bearer access");
                let info = json!({"sub": "operator", "name": "Example Operator", "groups": ["operators"]});
                (200, merge(info, &self.twist.lock().unwrap().userinfo))
            }
            path => panic!("unexpected provider request {path}"),
        };
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(document.to_string())))
    }

    /// The code exchange: the client's secret, the redirect URI and the verifier of the code's PKCE challenge.
    fn token(&self, headers: &http::HeaderMap, body: &[u8]) -> (u16, Value) {
        let basic = format!("Basic {}", STANDARD.encode("meter:s3cret~%2A"));
        assert_eq!(headers[header::AUTHORIZATION], basic);
        let form = query::form(std::str::from_utf8(body).unwrap()).unwrap();
        let names: Vec<_> = form.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["code", "code_verifier", "grant_type", "redirect_uri"]);
        let form: HashMap<_, _> = form.into_iter().collect();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["redirect_uri"], "https://meter.example/auth/oidc/callback");
        let bound = self.codes.lock().unwrap().remove(&form["code"]);
        let verified = bound.filter(|(code_challenge, _)| *code_challenge == challenge(&form["code_verifier"]));
        let Some((_, nonce)) = verified else {
            return (400, json!({"error": "invalid_grant"}));
        };
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let twist = self.twist.lock().unwrap();
        let header = merge(json!({"alg": "ES256", "kid": "p256", "typ": "JWT"}), &twist.header);
        let alg = header["alg"].as_str().unwrap_or_default();
        let hash = match alg {
            _ if alg.ends_with("384") => &ring::digest::SHA384,
            _ if alg.ends_with("512") || alg == "EdDSA" => &ring::digest::SHA512,
            _ => &ring::digest::SHA256,
        };
        let digest = ring::digest::digest(hash, b"access");
        let at_hash = B64.encode(&digest.as_ref()[..digest.as_ref().len() / 2]);
        let claims = json!({"iss": self.issuer, "aud": CLIENT_ID, "sub": "operator", "iat": now, "exp": now + 300,
            "nonce": nonce, "at_hash": at_hash});
        let id_token = self.keys.sign(&header, &merge(claims, &twist.claims));
        (200, json!({"access_token": "access", "token_type": "Bearer", "id_token": id_token}))
    }
}

fn merge(mut value: Value, twist: &Value) -> Value {
    for (name, member) in twist.as_object().into_iter().flatten() {
        match member {
            Value::Null => value.as_object_mut().unwrap().remove(name),
            member => value.as_object_mut().unwrap().insert(name.clone(), member.clone()),
        };
    }
    value
}

/// The provider's signing keys; `stranger` signs with a P-256 key its key set leaves out.
struct Signers {
    rsa: RsaKeyPair,
    p256: EcdsaKeyPair,
    p384: EcdsaKeyPair,
    ed: Ed25519KeyPair,
    stranger: EcdsaKeyPair,
    random: SystemRandom,
}

impl Signers {
    fn new() -> Self {
        let random = SystemRandom::new();
        let pem = Command::new("openssl")
            .args(["genrsa", "-traditional", "2048"])
            .output()
            .unwrap();
        let PrivateKeyDer::Pkcs1(der) = PrivateKeyDer::from_pem_slice(&pem.stdout).unwrap() else {
            panic!("openssl writes PKCS#1");
        };
        let ec = |curve| {
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(curve, &random).unwrap();
            EcdsaKeyPair::from_pkcs8(curve, pkcs8.as_ref(), &random).unwrap()
        };
        let ed = Ed25519KeyPair::from_pkcs8(Ed25519KeyPair::generate_pkcs8(&random).unwrap().as_ref()).unwrap();
        Self {
            rsa: RsaKeyPair::from_der(der.secret_pkcs1_der()).unwrap(),
            p256: ec(&signature::ECDSA_P256_SHA256_FIXED_SIGNING),
            p384: ec(&signature::ECDSA_P384_SHA384_FIXED_SIGNING),
            ed,
            stranger: ec(&signature::ECDSA_P256_SHA256_FIXED_SIGNING),
            random,
        }
    }

    fn jwks(&self, padded: bool) -> Value {
        let public: signature::RsaPublicKeyComponents<Vec<u8>> = self.rsa.public().into();
        let point = |key: &EcdsaKeyPair, crv, kid| {
            let point = key.public_key().as_ref();
            let half = (point.len() - 1) / 2;
            let (x, y) = (B64.encode(&point[1..=half]), B64.encode(&point[half + 1..]));
            json!({"kty": "EC", "crv": crv, "kid": kid, "x": x, "y": y})
        };
        let ed = B64.encode(self.ed.public_key().as_ref());
        let mut keys = json!({"keys": [
            {"kty": "RSA", "kid": "rsa", "use": "sig", "n": B64.encode([&[0], &public.n[..]].concat()), "e": B64.encode(&public.e)},
            point(&self.p256, "P-256", "p256"),
            point(&self.p384, "P-384", "p384"),
            {"kty": "OKP", "crv": "Ed25519", "kid": "ed", "x": ed},
            {"kty": "EC", "crv": "P-256", "kid": "encrypting", "use": "enc", "x": "AA", "y": "AA"},
        ]});
        let members = keys["keys"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .flat_map(|key| key.as_object_mut().unwrap());
        for (_, value) in members.filter(|(name, _)| padded && ["n", "e", "x", "y"].contains(&name.as_str())) {
            let text = value.as_str().unwrap();
            *value = json!(format!("{text}{}", "=".repeat((4 - text.len() % 4) % 4)));
        }
        keys
    }

    /// A compact JWS of `claims` signed as `header` names; `none` and unknown algorithms sign nothing.
    fn sign(&self, header: &Value, claims: &Value) -> String {
        let message = format!("{}.{}", B64.encode(header.to_string()), B64.encode(claims.to_string()));
        let rsa = |padding: &'static dyn signature::RsaEncoding| {
            let mut signature = vec![0; self.rsa.public().modulus_len()];
            self.rsa
                .sign(padding, &self.random, message.as_bytes(), &mut signature)
                .unwrap();
            signature
        };
        let ec = |key: &EcdsaKeyPair| key.sign(&self.random, message.as_bytes()).unwrap().as_ref().to_vec();
        let signature = match (header["alg"].as_str().unwrap_or_default(), header["kid"].as_str()) {
            (_, Some("stranger")) => ec(&self.stranger),
            ("RS256", _) => rsa(&signature::RSA_PKCS1_SHA256),
            ("RS512", _) => rsa(&signature::RSA_PKCS1_SHA512),
            ("PS256", _) => rsa(&signature::RSA_PSS_SHA256),
            ("PS384", _) => rsa(&signature::RSA_PSS_SHA384),
            ("ES256", _) => ec(&self.p256),
            ("ES384", _) => ec(&self.p384),
            ("EdDSA", _) => self.ed.sign(message.as_bytes()).as_ref().to_vec(),
            ("HS256", _) => {
                let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, SECRET.as_bytes());
                ring::hmac::sign(&key, message.as_bytes()).as_ref().to_vec()
            }
            _ => Vec::new(),
        };
        format!("{message}.{}", B64.encode(signature))
    }
}
