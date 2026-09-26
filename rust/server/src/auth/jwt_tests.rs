use super::*;
use ring::{
    rand::SystemRandom,
    signature::{EcdsaKeyPair, Ed25519KeyPair, KeyPair, RsaKeyPair},
};
use rustls::pki_types::{PrivateKeyDer, pem::PemObject};
use serde_json::json;

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

struct Signers {
    rsa: RsaKeyPair,
    p256: EcdsaKeyPair,
    p384: EcdsaKeyPair,
    ed: Ed25519KeyPair,
    rng: SystemRandom,
}

impl Signers {
    fn new() -> Self {
        let rng = SystemRandom::new();
        let ec = |alg| {
            EcdsaKeyPair::from_pkcs8(
                alg,
                EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap().as_ref(),
                &rng,
            )
            .unwrap()
        };
        Self {
            rsa: rsa("2048"),
            p256: ec(&signature::ECDSA_P256_SHA256_FIXED_SIGNING),
            p384: ec(&signature::ECDSA_P384_SHA384_FIXED_SIGNING),
            ed: Ed25519KeyPair::from_pkcs8(Ed25519KeyPair::generate_pkcs8(&rng).unwrap().as_ref())
                .unwrap(),
            rng,
        }
    }
    fn jwks(&self, extra: &[Value]) -> Jwks {
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
        let mut keys = vec![
            rsa(&self.rsa, "rsa"),
            point(&self.p256, "P-256", "p256"),
            point(&self.p384, "P-384", "p384"),
            json!({"kty": "OKP", "crv": "Ed25519", "kid": "ed", "x": B64.encode(self.ed.public_key().as_ref())}),
            json!({"kty": "RSA", "kid": "enc", "use": "enc", "n": "AQAB", "e": "AQAB"}),
            json!({"kty": "oct", "kid": "mac", "k": "c2VjcmV0"}),
        ];
        keys.extend_from_slice(extra);
        Jwks::parse(json!({"keys": keys}).to_string().as_bytes()).unwrap()
    }
    fn sign(&self, header: Value, claims: &Value) -> String {
        let message = format!(
            "{}.{}",
            B64.encode(header.to_string()),
            B64.encode(claims.to_string())
        );
        let rsa = |key: &RsaKeyPair, padding: &'static dyn signature::RsaEncoding| {
            let mut out = vec![0; key.public().modulus_len()];
            key.sign(padding, &self.rng, message.as_bytes(), &mut out)
                .unwrap();
            out
        };
        let signature = match header["alg"].as_str().unwrap_or_default() {
            "RS256" => rsa(&self.rsa, &signature::RSA_PKCS1_SHA256),
            "RS384" => rsa(&self.rsa, &signature::RSA_PKCS1_SHA384),
            "PS384" => rsa(&self.rsa, &signature::RSA_PSS_SHA384),
            "PS512" => rsa(&self.rsa, &signature::RSA_PSS_SHA512),
            "RS512" => rsa(&self.rsa, &signature::RSA_PKCS1_SHA512),
            "PS256" => rsa(&self.rsa, &signature::RSA_PSS_SHA256),
            "ES256" => self
                .p256
                .sign(&self.rng, message.as_bytes())
                .unwrap()
                .as_ref()
                .to_vec(),
            "ES384" => self
                .p384
                .sign(&self.rng, message.as_bytes())
                .unwrap()
                .as_ref()
                .to_vec(),
            "EdDSA" => self.ed.sign(message.as_bytes()).as_ref().to_vec(),
            "HS256" => ring::hmac::sign(
                &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"c2VjcmV0"),
                message.as_bytes(),
            )
            .as_ref()
            .to_vec(),
            _ => Vec::new(),
        };
        format!("{message}.{}", B64.encode(signature))
    }
}

const ALL: [Alg; 9] = [
    Alg::RS256,
    Alg::RS384,
    Alg::RS512,
    Alg::PS256,
    Alg::PS384,
    Alg::PS512,
    Alg::ES256,
    Alg::ES384,
    Alg::EdDSA,
];

#[test]
fn every_ring_algorithm_verifies_against_the_matching_key_only() {
    let signers = Signers::new();
    let keys = signers.jwks(&[]);
    let claims = json!({"sub": "operator"});
    for (alg, kid) in [
        ("RS256", "rsa"),
        ("RS384", "rsa"),
        ("RS512", "rsa"),
        ("PS384", "rsa"),
        ("PS512", "rsa"),
        ("PS256", "rsa"),
        ("ES256", "p256"),
        ("ES384", "p384"),
        ("EdDSA", "ed"),
    ] {
        let token = signers.sign(json!({"alg": alg, "kid": kid}), &claims);
        assert_eq!(
            verify(&token, &keys, &ALL).map(|v| v.claims["sub"].clone()),
            Ok(json!("operator")),
            "{alg}"
        );
        let token = signers.sign(json!({"alg": alg}), &claims);
        assert!(verify(&token, &keys, &ALL).is_ok(), "{alg} without kid");
    }
    let refused = [
        (json!({"alg": "RS256", "kid": "p256"}), Reject::UnknownKey),
        (json!({"alg": "ES256", "kid": "rsa"}), Reject::UnknownKey),
        (json!({"alg": "ES384", "kid": "p256"}), Reject::UnknownKey),
        (json!({"alg": "RS256", "kid": "enc"}), Reject::UnknownKey),
        (
            json!({"alg": "RS256", "kid": "missing"}),
            Reject::UnknownKey,
        ),
        (json!({"alg": "HS256", "kid": "mac"}), Reject::Algorithm),
        (json!({"alg": "ES512", "kid": "p256"}), Reject::Algorithm),
        (json!({"alg": "none"}), Reject::Algorithm),
        (json!({"kid": "rsa"}), Reject::Algorithm),
        (
            json!({"alg": "RS256", "kid": "rsa", "crit": ["exp"]}),
            Reject::Malformed,
        ),
        (
            json!({"alg": "RS256", "kid": "rsa", "cty": "JWT"}),
            Reject::Malformed,
        ),
        (
            json!({"alg": "RS256", "kid": "rsa", "typ": "at+jwt"}),
            Reject::Malformed,
        ),
        (json!({"alg": "RS256", "kid": 7}), Reject::Malformed),
    ];
    for (header, reject) in refused {
        let token = signers.sign(header.clone(), &claims);
        assert_eq!(verify(&token, &keys, &ALL).err(), Some(reject), "{header}");
    }
    let token = signers.sign(json!({"alg": "RS256", "kid": "rsa", "typ": "JWT"}), &claims);
    assert!(verify(&token, &keys, &ALL).is_ok());
    assert_eq!(
        verify(&token, &keys, &[Alg::ES256]).err(),
        Some(Reject::Algorithm)
    );
    let (message, _) = token.rsplit_once('.').unwrap();
    let forged = format!("{message}.{}", B64.encode([0; 256]));
    assert_eq!(verify(&forged, &keys, &ALL).err(), Some(Reject::Signature));
    assert_eq!(
        verify(&format!("{token}.extra.parts"), &keys, &ALL).err(),
        Some(Reject::Malformed)
    );
    let public: signature::RsaPublicKeyComponents<Vec<u8>> = signers.rsa.public().into();
    for extra in [
        json!({"alg": "PS256"}),
        json!({"alg": 7}),
        json!({"use": 7}),
        json!({"use": "enc"}),
        json!({"key_ops": ["sign"]}),
        json!({"key_ops": "verify"}),
    ] {
        let mut key = json!({"kty": "RSA", "n": B64.encode(&public.n), "e": B64.encode(&public.e)});
        key.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let keys = Jwks::parse(json!({"keys": [key]}).to_string().as_bytes()).unwrap();
        assert_eq!(
            verify(&signers.sign(json!({"alg": "RS256"}), &claims), &keys, &ALL).err(),
            Some(Reject::UnknownKey)
        );
    }
}

#[test]
fn id_token_claims_bind_issuer_audience_nonce_time_and_access_token() {
    let signers = Signers::new();
    let keys = signers.jwks(&[]);
    let now = 1_800_000_000u64;
    let expected = Expected {
        issuer: "https://id.example",
        client_id: "meter",
        nonce: "n0nce",
        access_token: "access",
        now,
    };
    let at_hash = |digest: &'static ring::digest::Algorithm| {
        let hash = ring::digest::digest(digest, b"access");
        B64.encode(&hash.as_ref()[..hash.as_ref().len() / 2])
    };
    let base = json!({"iss": "https://id.example", "sub": "operator", "aud": "meter", "exp": now + 300, "iat": now, "nonce": "n0nce", "name": "Operator"});
    let check = |alg: &str, kid: &str, changes: Value| {
        let mut claims = base.clone();
        for (key, value) in changes.as_object().unwrap() {
            if value.is_null() {
                claims.as_object_mut().unwrap().remove(key);
            } else {
                claims[key] = value.clone();
            }
        }
        let verified = verify(
            &signers.sign(json!({"alg": alg, "kid": kid}), &claims),
            &keys,
            &ALL,
        )
        .unwrap();
        id_token(verified, &expected).map(|claims| claims.subject)
    };
    for (alg, kid, digest) in [
        ("RS256", "rsa", &ring::digest::SHA256),
        ("ES384", "p384", &ring::digest::SHA384),
        ("EdDSA", "ed", &ring::digest::SHA512),
    ] {
        assert_eq!(
            check(alg, kid, json!({"at_hash": at_hash(digest)})),
            Ok("operator".into()),
            "{alg}"
        );
    }
    assert!(
        check(
            "RS256",
            "rsa",
            json!({"aud": ["meter"], "azp": "meter", "nbf": now + 299})
        )
        .is_ok()
    );
    for changes in [
        json!({"iss": "https://id.example/"}),
        json!({"aud": "other"}),
        json!({"aud": ["meter", "other"]}),
        json!({"aud": ["meter", "meter"]}),
        json!({"aud": null}),
        json!({"azp": "other"}),
        json!({"exp": now}),
        json!({"exp": null}),
        json!({"iat": null}),
        json!({"nbf": now + 301}),
        json!({"nonce": "other"}),
        json!({"nonce": null}),
        json!({"at_hash": at_hash(&ring::digest::SHA384)}),
        json!({"sub": 7}),
    ] {
        assert_eq!(
            check("RS256", "rsa", changes.clone()).err(),
            Some(Reject::Claims),
            "{changes}"
        );
    }
}
