use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ring::{
    digest,
    signature::{self, RsaPublicKeyComponents, UnparsedPublicKey},
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;

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

    /// The advertised algorithms ring verifies, or RS256 where go-oidc, which also knows ES512, knows none.
    pub fn allowed(advertised: &[&str]) -> Vec<Self> {
        if !advertised
            .iter()
            .any(|&name| name == "ES512" || Self::parse(name).is_some())
        {
            return vec![Self::RS256];
        }
        advertised.iter().filter_map(|name| Self::parse(name)).collect()
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
    /// The nonce differs from the sign-in's, or a name is mistyped.
    Nonce,
    AccessTokenHash,
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
            (Alg::ES256, Material::P256(point)) => UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                .verify(message, signature)
                .is_ok(),
            (Alg::ES384, Material::P384(point)) => UnparsedPublicKey::new(&signature::ECDSA_P384_SHA384_FIXED, point)
                .verify(message, signature)
                .is_ok(),
            (Alg::EdDSA, Material::Ed25519(point)) => UnparsedPublicKey::new(&signature::ED25519, point)
                .verify(message, signature)
                .is_ok(),
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
        family && self.alg.as_deref().is_none_or(|name| Alg::parse(name) == Some(alg))
    }
}

pub(super) struct Jwks(Vec<Key>);

impl Jwks {
    pub fn parse(body: &[u8]) -> Result<Self, Reject> {
        #[derive(Deserialize)]
        struct Set {
            keys: Vec<Value>,
        }
        let set: Set = go_json(body).map_err(|_| Reject::Malformed)?;
        let text = |key: &Value, field: &str| key.get(field).and_then(Value::as_str).map(str::to_owned);
        let bytes = |key: &Value, field: &str| {
            key.get(field)
                .and_then(Value::as_str)
                .and_then(|value| B64.decode(value).ok())
        };
        let keys = set
            .keys
            .iter()
            .filter(|key| key.get("use").is_none_or(|usage| usage.as_str() == Some("sig")))
            .filter(|key| key.get("alg").is_none_or(Value::is_string))
            .filter(|key| key.get("kid").is_none_or(Value::is_string))
            .filter(|key| {
                key.get("key_ops").is_none_or(|ops| {
                    ops.as_array().is_some_and(|ops| {
                        ops.iter().all(Value::is_string) && ops.iter().any(|op| op.as_str() == Some("verify"))
                    })
                })
            })
            .filter_map(|key| {
                let point = |size| match (bytes(key, "x"), bytes(key, "y")) {
                    (Some(x), Some(y)) if x.len() == size && y.len() == size => Some([&[4][..], &x, &y].concat()),
                    _ => None,
                };
                let material = match (text(key, "kty")?.as_str(), text(key, "crv").as_deref()) {
                    ("RSA", _) => Material::Rsa {
                        n: strip_zeros(bytes(key, "n")?),
                        e: strip_zeros(bytes(key, "e")?),
                    },
                    ("EC", Some("P-256")) => Material::P256(point(32)?),
                    ("EC", Some("P-384")) => Material::P384(point(48)?),
                    ("OKP", Some("Ed25519")) => Material::Ed25519(bytes(key, "x").filter(|x| x.len() == 32)?),
                    _ => return None,
                };
                Some(Key {
                    kid: text(key, "kid"),
                    alg: text(key, "alg"),
                    material,
                })
            })
            // Past the filters, so that keys no token can use cannot push out the signing key.
            .take(64)
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
    pub payload: Vec<u8>,
}

pub(super) fn verify(token: &str, keys: &Jwks, allowed: &[Alg]) -> Result<Verified, Reject> {
    // go-jose drops the whitespace a provider may wrap a token in.
    let token = token.trim();
    if token.len() > 16 * 1024 {
        return Err(Reject::Malformed);
    }
    let (message, signature) = token.rsplit_once('.').ok_or(Reject::Malformed)?;
    let (header, payload) = message.split_once('.').ok_or(Reject::Malformed)?;
    if payload.contains('.') {
        return Err(Reject::Malformed);
    }
    let decode = |part: &str| B64.decode(part).map_err(|_| Reject::Malformed);
    let header: Map<String, Value> = serde_json::from_slice(&decode(header)?).map_err(|_| Reject::Malformed)?;
    let typ = header.get("typ").map(|typ| typ.as_str().map(str::to_ascii_lowercase));
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
    // As go-jose and go-oidc read it, a null or empty kid names no key.
    let kid = match header.get("kid") {
        None | Some(Value::Null) => None,
        Some(kid) => Some(kid.as_str().ok_or(Reject::Malformed)?).filter(|kid| !kid.is_empty()),
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
    Ok(Verified {
        alg,
        payload: decode(payload)?,
    })
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

pub(super) fn audience_and_issuer(claims: &Map<String, Value>, issuer: &str, client_id: &str) -> Result<(), Reject> {
    let audiences = match claims.get("aud") {
        Some(Value::String(one)) => vec![one.as_str()],
        Some(Value::Array(many)) => many.iter().map(go_str).collect::<Option<_>>().ok_or(Reject::Claims)?,
        _ => return Err(Reject::Claims),
    };
    // Like Go's go-oidc: the audience includes this client, whatever else it names, and azp is not read.
    if claims.get("iss").and_then(Value::as_str) != Some(issuer) || !audiences.contains(&client_id) {
        return Err(Reject::Claims);
    }
    Ok(())
}

/// A JSON number, or a string that holds exactly one, as Go's json.Number takes it.
pub(super) fn go_number(value: &Value) -> Option<serde_json::Number> {
    match value {
        Value::String(text) if text.trim() == text => serde_json::from_str(text).ok(),
        value => value.as_number().cloned(),
    }
}

/// A member as Go's decoder fills a string or slice field: absent or null, it keeps its zero value.
pub(super) fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de> + Default>(
    member: D,
) -> Result<T, D::Error> {
    Ok(Option::deserialize(member)?.unwrap_or_default())
}

/// A string as Go's decoder fills one, which null leaves empty.
pub(super) fn go_str(value: &Value) -> Option<&str> {
    value.as_str().or(value.is_null().then_some(""))
}

/// A document as Go's decoder reads it, each byte of invalid UTF-8 as U+FFFD.
pub(super) fn go_json<T: serde::de::DeserializeOwned>(document: &[u8]) -> serde_json::Result<T> {
    let mut text = String::with_capacity(document.len());
    for chunk in document.utf8_chunks() {
        text.push_str(chunk.valid());
        text.extend(chunk.invalid().iter().map(|_| char::REPLACEMENT_CHARACTER));
    }
    serde_json::from_str(&text)
}

pub(super) fn id_token(verified: Verified, expected: &Expected<'_>) -> Result<IdClaims, Reject> {
    #[derive(Deserialize)]
    struct Standard {
        #[serde(default, deserialize_with = "nullable")]
        sub: String,
        #[serde(default, deserialize_with = "nullable")]
        nonce: String,
        #[serde(default, deserialize_with = "nullable")]
        at_hash: String,
        #[serde(default, rename = "_claim_names", deserialize_with = "nullable")]
        names: HashMap<String, Option<String>>,
        #[serde(default, rename = "_claim_sources", deserialize_with = "nullable")]
        sources: HashMap<String, Option<Source>>,
    }
    /// go-oidc reads a distributed claim's source, and so refuses one whose members are mistyped.
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Source {
        endpoint: Option<String>,
        access_token: Option<String>,
    }
    #[derive(Deserialize)]
    struct Names {
        name: Option<String>,
        preferred_username: Option<String>,
    }
    let claims: Map<String, Value> = go_json(&verified.payload).map_err(|_| Reject::Malformed)?;
    audience_and_issuer(&claims, expected.issuer, expected.client_id)?;
    // go-oidc's jsonTime: int64 seconds, where a float past that range reads as the least, as on amd64, and which
    // time.Unix moves to Go's own epoch, year 1, wrapping; nbf alone is a pointer, which null leaves absent.
    let go = |seconds: i64| seconds.wrapping_add(62_135_596_800);
    let time = |name: &str| match claims.get(name) {
        None => Ok(None),
        Some(Value::Null) if name == "nbf" => Ok(None),
        Some(time) => {
            let time = go_number(time).ok_or(Reject::Claims)?;
            let float = time.as_f64().filter(|&time| time < i64::MAX as f64);
            Ok(Some(go(time
                .as_i64()
                .or(float.map(|time| time as i64))
                .unwrap_or(i64::MIN))))
        }
    };
    let (expiry, not_before) = (time("exp")?.ok_or(Reject::Claims)?, time("nbf")?);
    time("iat")?;
    let now = go(expected.now as i64);
    if now >= expiry || not_before.is_some_and(|nbf| nbf > now + 300) {
        return Err(Reject::Claims);
    }
    let claims = Value::Object(claims);
    let standard = Standard::deserialize(&claims).map_err(|_| Reject::Claims)?;
    // go-oidc refuses a distributed claim whose source is empty or not listed.
    let missing = |source: &str| source.is_empty() || !standard.sources.contains_key(source);
    if standard
        .names
        .values()
        .any(|source| source.as_deref().is_none_or(missing))
    {
        return Err(Reject::Claims);
    }
    let names = Names::deserialize(&claims).map_err(|_| Reject::Nonce)?;
    let digest = |value: &str| digest::digest(&digest::SHA256, value.as_bytes());
    if standard.nonce.is_empty() || digest(&standard.nonce).as_ref() != digest(expected.nonce).as_ref() {
        return Err(Reject::Nonce);
    }
    let hash = digest::digest(verified.alg.digest(), expected.access_token.as_bytes());
    if !standard.at_hash.is_empty() && B64.encode(&hash.as_ref()[..hash.as_ref().len() / 2]) != standard.at_hash {
        return Err(Reject::AccessTokenHash);
    }
    Ok(IdClaims {
        subject: standard.sub,
        name: names.name,
        preferred_username: names.preferred_username,
    })
}

#[cfg(test)]
#[path = "jwt_tests.rs"]
mod tests;
