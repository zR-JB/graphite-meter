//! Fresh signed JWT fixtures shared by cryptographic and provider-boundary tests.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ring::{
    rand::SystemRandom,
    signature::{self, EcdsaKeyPair, Ed25519KeyPair, KeyPair, RsaKeyPair},
};
use rustls::pki_types::{PrivateKeyDer, pem::PemObject};
use serde_json::{Value, json};

fn rsa(bits: &str) -> RsaKeyPair {
    let pem = std::process::Command::new("openssl")
        .args(["genrsa", "-traditional", bits])
        .output()
        .unwrap();
    let PrivateKeyDer::Pkcs1(der) = PrivateKeyDer::from_pem_slice(&pem.stdout).unwrap() else {
        panic!("openssl did not emit PKCS#1");
    };
    RsaKeyPair::from_der(der.secret_pkcs1_der()).unwrap()
}

pub(super) struct Signers {
    pub(super) rsa: RsaKeyPair,
    secret: Vec<u8>,
    p256: EcdsaKeyPair,
    p384: EcdsaKeyPair,
    ed: Ed25519KeyPair,
    rng: SystemRandom,
}

impl Signers {
    pub(super) fn new(secret: &[u8]) -> Self {
        let rng = SystemRandom::new();
        let ec = |alg| {
            EcdsaKeyPair::from_pkcs8(alg, EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap().as_ref(), &rng).unwrap()
        };
        Self {
            rsa: rsa("2048"),
            secret: secret.to_vec(),
            p256: ec(&signature::ECDSA_P256_SHA256_FIXED_SIGNING),
            p384: ec(&signature::ECDSA_P384_SHA384_FIXED_SIGNING),
            ed: Ed25519KeyPair::from_pkcs8(Ed25519KeyPair::generate_pkcs8(&rng).unwrap().as_ref()).unwrap(),
            rng,
        }
    }
    pub(super) fn jwks(&self) -> Value {
        let rsa = |key: &RsaKeyPair, kid: &str| {
            let public: signature::RsaPublicKeyComponents<Vec<u8>> = key.public().into();
            let n = [&[0][..], &public.n].concat();
            json!({"kty": "RSA", "kid": kid, "n": B64.encode(n), "e": B64.encode(public.e)})
        };
        let point = |key: &EcdsaKeyPair, crv: &str, kid: &str| {
            let point = key.public_key().as_ref();
            let half = (point.len() - 1) / 2;
            json!({"kty": "EC", "crv": crv, "kid": kid, "x": B64.encode(&point[1..=half]), "y": B64.encode(&point[half + 1..])})
        };
        let keys = vec![
            rsa(&self.rsa, "rsa"),
            point(&self.p256, "P-256", "p256"),
            point(&self.p384, "P-384", "p384"),
            json!({"kty": "OKP", "crv": "Ed25519", "kid": "ed", "x": B64.encode(self.ed.public_key().as_ref())}),
            json!({"kty": "RSA", "kid": "enc", "use": "enc", "n": "AQAB", "e": "AQAB"}),
            json!({"kty": "oct", "kid": "mac", "k": "c2VjcmV0"}),
        ];
        json!({"keys": keys})
    }
    pub(super) fn sign(&self, header: Value, claims: &Value) -> String {
        let message = format!("{}.{}", B64.encode(header.to_string()), B64.encode(claims.to_string()));
        let rsa = |key: &RsaKeyPair, padding: &'static dyn signature::RsaEncoding| {
            let mut out = vec![0; key.public().modulus_len()];
            key.sign(padding, &self.rng, message.as_bytes(), &mut out).unwrap();
            out
        };
        let signature = match header["alg"].as_str().unwrap_or_default() {
            "RS256" => rsa(&self.rsa, &signature::RSA_PKCS1_SHA256),
            "RS384" => rsa(&self.rsa, &signature::RSA_PKCS1_SHA384),
            "PS384" => rsa(&self.rsa, &signature::RSA_PSS_SHA384),
            "PS512" => rsa(&self.rsa, &signature::RSA_PSS_SHA512),
            "RS512" => rsa(&self.rsa, &signature::RSA_PKCS1_SHA512),
            "PS256" => rsa(&self.rsa, &signature::RSA_PSS_SHA256),
            "ES256" => self.p256.sign(&self.rng, message.as_bytes()).unwrap().as_ref().to_vec(),
            "ES384" => self.p384.sign(&self.rng, message.as_bytes()).unwrap().as_ref().to_vec(),
            "EdDSA" => self.ed.sign(message.as_bytes()).as_ref().to_vec(),
            "HS256" => {
                ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &self.secret), message.as_bytes())
                    .as_ref()
                    .to_vec()
            }
            _ => Vec::new(),
        };
        format!("{message}.{}", B64.encode(signature))
    }
}
