use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ring::{
    digest,
    signature::{self, RsaPublicKeyComponents, UnparsedPublicKey},
};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Alg {
    RS256,
    RS384,
    RS512,
    PS256,
    PS384,
    PS512,
    ES256,
    ES384,
    EdDSA,
}

impl Alg {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "RS256" => Self::RS256,
            "RS384" => Self::RS384,
            "RS512" => Self::RS512,
            "PS256" => Self::PS256,
            "PS384" => Self::PS384,
            "PS512" => Self::PS512,
            "ES256" => Self::ES256,
            "ES384" => Self::ES384,
            "EdDSA" => Self::EdDSA,
            _ => return None,
        })
    }

    pub fn allowed(advertised: &[String]) -> Vec<Self> {
        advertised
            .iter()
            .filter_map(|name| Self::parse(name))
            .collect()
    }

    fn digest(self) -> &'static digest::Algorithm {
        match self {
            Self::RS256 | Self::PS256 | Self::ES256 => &digest::SHA256,
            Self::RS384 | Self::PS384 | Self::ES384 => &digest::SHA384,
            Self::RS512 | Self::PS512 | Self::EdDSA => &digest::SHA512,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reject {
    Malformed,
    Algorithm,
    UnknownKey,
    Signature,
    Claims,
}

enum Material {
    Rsa { n: Vec<u8>, e: Vec<u8> },
    P256(Vec<u8>),
    P384(Vec<u8>),
    Ed25519(Vec<u8>),
}

struct Key {
    kid: Option<String>,
    alg: Option<String>,
    material: Material,
}

impl Key {
    fn verifies(&self, alg: Alg, message: &[u8], signature: &[u8]) -> bool {
        let rsa = |parameters: &'static signature::RsaParameters| match &self.material {
            Material::Rsa { n, e } => RsaPublicKeyComponents { n, e }
                .verify(parameters, message, signature)
                .is_ok(),
            _ => false,
        };
        match (alg, &self.material) {
            (Alg::RS256, _) => rsa(&signature::RSA_PKCS1_2048_8192_SHA256),
            (Alg::RS384, _) => rsa(&signature::RSA_PKCS1_2048_8192_SHA384),
            (Alg::RS512, _) => rsa(&signature::RSA_PKCS1_2048_8192_SHA512),
            (Alg::PS256, _) => rsa(&signature::RSA_PSS_2048_8192_SHA256),
            (Alg::PS384, _) => rsa(&signature::RSA_PSS_2048_8192_SHA384),
            (Alg::PS512, _) => rsa(&signature::RSA_PSS_2048_8192_SHA512),
            (Alg::ES256, Material::P256(point)) => {
                UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                    .verify(message, signature)
                    .is_ok()
            }
            (Alg::ES384, Material::P384(point)) => {
                UnparsedPublicKey::new(&signature::ECDSA_P384_SHA384_FIXED, point)
                    .verify(message, signature)
                    .is_ok()
            }
            (Alg::EdDSA, Material::Ed25519(point)) => {
                UnparsedPublicKey::new(&signature::ED25519, point)
                    .verify(message, signature)
                    .is_ok()
            }
            _ => false,
        }
    }

    fn fits(&self, alg: Alg) -> bool {
        let family = matches!(
            (&self.material, alg),
            (Material::Rsa { .. }, Alg::RS256 | Alg::RS384 | Alg::RS512)
                | (Material::Rsa { .. }, Alg::PS256 | Alg::PS384 | Alg::PS512)
                | (Material::P256(_), Alg::ES256)
                | (Material::P384(_), Alg::ES384)
                | (Material::Ed25519(_), Alg::EdDSA)
        );
        family
            && self
                .alg
                .as_deref()
                .is_none_or(|name| Alg::parse(name) == Some(alg))
    }
}

pub(super) struct Jwks(Vec<Key>);

impl Jwks {
    pub fn parse(body: &[u8]) -> Result<Self, Reject> {
        #[derive(Deserialize)]
        struct Set {
            keys: Vec<Value>,
        }
        let set: Set = serde_json::from_slice(body).map_err(|_| Reject::Malformed)?;
        let text =
            |key: &Value, field: &str| key.get(field).and_then(Value::as_str).map(str::to_owned);
        let bytes = |key: &Value, field: &str| {
            key.get(field)
                .and_then(Value::as_str)
                .and_then(|value| B64.decode(value).ok())
        };
        let keys = set
            .keys
            .iter()
            .take(64)
            .filter(|key| {
                key.get("use")
                    .is_none_or(|usage| usage.as_str() == Some("sig"))
            })
            .filter(|key| key.get("alg").is_none_or(Value::is_string))
            .filter(|key| key.get("kid").is_none_or(Value::is_string))
            .filter(|key| {
                key.get("key_ops").is_none_or(|ops| {
                    ops.as_array().is_some_and(|ops| {
                        ops.iter().all(Value::is_string)
                            && ops.iter().any(|op| op.as_str() == Some("verify"))
                    })
                })
            })
            .filter_map(|key| {
                let point = |size| match (bytes(key, "x"), bytes(key, "y")) {
                    (Some(x), Some(y)) if x.len() == size && y.len() == size => {
                        Some([&[4][..], &x, &y].concat())
                    }
                    _ => None,
                };
                let material = match (text(key, "kty")?.as_str(), text(key, "crv").as_deref()) {
                    ("RSA", _) => Material::Rsa {
                        n: strip_zeros(bytes(key, "n")?),
                        e: strip_zeros(bytes(key, "e")?),
                    },
                    ("EC", Some("P-256")) => Material::P256(point(32)?),
                    ("EC", Some("P-384")) => Material::P384(point(48)?),
                    ("OKP", Some("Ed25519")) => {
                        Material::Ed25519(bytes(key, "x").filter(|x| x.len() == 32)?)
                    }
                    _ => return None,
                };
                Some(Key {
                    kid: text(key, "kid"),
                    alg: text(key, "alg"),
                    material,
                })
            })
            .collect();
        Ok(Self(keys))
    }
}

fn strip_zeros(mut value: Vec<u8>) -> Vec<u8> {
    let zeros = value.iter().take_while(|byte| **byte == 0).count();
    value.drain(..zeros);
    value
}

pub(super) struct Verified {
    pub alg: Alg,
    pub claims: Map<String, Value>,
}

pub(super) fn verify(token: &str, keys: &Jwks, allowed: &[Alg]) -> Result<Verified, Reject> {
    if token.len() > 16 * 1024 {
        return Err(Reject::Malformed);
    }
    let (message, signature) = token.rsplit_once('.').ok_or(Reject::Malformed)?;
    let (header, payload) = message.split_once('.').ok_or(Reject::Malformed)?;
    if payload.contains('.') {
        return Err(Reject::Malformed);
    }
    let decode = |part: &str| B64.decode(part).map_err(|_| Reject::Malformed);
    let header: Map<String, Value> =
        serde_json::from_slice(&decode(header)?).map_err(|_| Reject::Malformed)?;
    let typ = header
        .get("typ")
        .map(|typ| typ.as_str().map(str::to_ascii_lowercase));
    if header.contains_key("cty")
        || header.contains_key("crit")
        || header.contains_key("enc")
        || typ.is_some_and(|typ| {
            !matches!(
                typ.as_deref(),
                Some("jwt" | "application/jwt" | "jose" | "application/jose")
            )
        })
    {
        return Err(Reject::Malformed);
    }
    let alg = header
        .get("alg")
        .and_then(Value::as_str)
        .and_then(Alg::parse)
        .filter(|alg| allowed.contains(alg))
        .ok_or(Reject::Algorithm)?;
    let kid = match header.get("kid") {
        None => None,
        Some(kid) => Some(kid.as_str().ok_or(Reject::Malformed)?),
    };
    let signature = decode(signature)?;
    let mut candidates = keys
        .0
        .iter()
        .filter(|key| key.fits(alg) && kid.is_none_or(|kid| key.kid.as_deref() == Some(kid)))
        .peekable();
    if candidates.peek().is_none() {
        return Err(Reject::UnknownKey);
    }
    if !candidates.any(|key| key.verifies(alg, message.as_bytes(), &signature)) {
        return Err(Reject::Signature);
    }
    let claims = serde_json::from_slice(&decode(payload)?).map_err(|_| Reject::Malformed)?;
    Ok(Verified { alg, claims })
}

pub(super) struct IdClaims {
    pub subject: String,
    pub name: Option<String>,
    pub preferred_username: Option<String>,
}

pub(super) struct Expected<'a> {
    pub issuer: &'a str,
    pub client_id: &'a str,
    pub nonce: &'a str,
    pub access_token: &'a str,
    pub now: u64,
}

pub(super) fn audience_and_issuer(
    claims: &Map<String, Value>,
    issuer: &str,
    client_id: &str,
) -> Result<(), Reject> {
    let audiences = match claims.get("aud") {
        Some(Value::String(one)) => vec![one.as_str()],
        Some(Value::Array(many)) => many
            .iter()
            .map(|aud| aud.as_str().ok_or(Reject::Claims))
            .collect::<Result<_, _>>()?,
        _ => return Err(Reject::Claims),
    };
    if claims.get("iss").and_then(Value::as_str) != Some(issuer)
        || audiences.len() != 1
        || !audiences.contains(&client_id)
        || audiences.iter().any(|aud| *aud != client_id)
    {
        return Err(Reject::Claims);
    }
    Ok(())
}

pub(super) fn id_token(verified: Verified, expected: &Expected<'_>) -> Result<IdClaims, Reject> {
    #[derive(Deserialize)]
    struct Claims {
        sub: String,
        exp: f64,
        #[serde(rename = "iat")]
        _iat: f64,
        nbf: Option<f64>,
        nonce: Option<String>,
        azp: Option<String>,
        at_hash: Option<String>,
        name: Option<String>,
        preferred_username: Option<String>,
    }
    audience_and_issuer(&verified.claims, expected.issuer, expected.client_id)?;
    let claims: Claims =
        serde_json::from_value(Value::Object(verified.claims)).map_err(|_| Reject::Claims)?;
    let now = expected.now as f64;
    let digest = |value: &str| digest::digest(&digest::SHA256, value.as_bytes());
    let nonce_matches = claims
        .nonce
        .as_deref()
        .is_some_and(|nonce| digest(nonce).as_ref() == digest(expected.nonce).as_ref());
    let at_hash_matches = claims.at_hash.as_deref().is_none_or(|expected_hash| {
        let hash = digest::digest(verified.alg.digest(), expected.access_token.as_bytes());
        B64.encode(&hash.as_ref()[..hash.as_ref().len() / 2]) == expected_hash
    });
    if now >= claims.exp
        || claims.nbf.is_some_and(|nbf| nbf > now + 300.0)
        || !nonce_matches
        || claims
            .azp
            .as_deref()
            .is_some_and(|azp| azp != expected.client_id)
        || !at_hash_matches
    {
        return Err(Reject::Claims);
    }
    Ok(IdClaims {
        subject: claims.sub,
        name: claims.name,
        preferred_username: claims.preferred_username,
    })
}

#[cfg(test)]
#[path = "jwt_tests.rs"]
mod tests;
