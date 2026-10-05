//! Signed JSON Web Tokens as an OIDC provider issues them: its key set, signatures and an ID token's claims.

use super::security::Reason;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ring::{
    digest::{self, SHA256, SHA384, SHA512},
    signature::{self, RsaParameters, RsaPublicKeyComponents, UnparsedPublicKey, VerificationAlgorithm},
};
use serde::Deserialize;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;

/// The longest token read.
const MAX_TOKEN: usize = 16 * 1024;
/// A key set keeps at most this many usable keys.
const MAX_KEYS: usize = 64;
/// A token may name a start this many seconds in the future.
const SKEW: f64 = 300.0;

/// The signature algorithms tokens may use; each name is its JOSE name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// RSA parameters for keys of 2048 to 8192 bits, else a curve's algorithm.
type Scheme = Result<&'static RsaParameters, &'static dyn VerificationAlgorithm>;

impl Alg {
    fn parse(name: &str) -> Option<Self> {
        use Alg::*;
        let all = [RS256, RS384, RS512, PS256, PS384, PS512, ES256, ES384, EdDSA];
        all.into_iter().find(|alg| format!("{alg:?}") == name)
    }

    /// The advertised algorithms this server verifies; RS256 when a provider advertises none an OIDC client knows.
    pub fn allowed(advertised: &[String]) -> Vec<Self> {
        let known = |name: &String| name == "ES512" || Self::parse(name).is_some();
        match advertised.iter().any(known) {
            true => advertised.iter().filter_map(|name| Self::parse(name)).collect(),
            false => vec![Self::RS256],
        }
    }

    /// How a signature verifies, and the digest whose left half `at_hash` holds.
    fn scheme(self) -> (Scheme, &'static digest::Algorithm) {
        use signature::*;
        match self {
            Self::RS256 => (Ok(&RSA_PKCS1_2048_8192_SHA256), &SHA256),
            Self::RS384 => (Ok(&RSA_PKCS1_2048_8192_SHA384), &SHA384),
            Self::RS512 => (Ok(&RSA_PKCS1_2048_8192_SHA512), &SHA512),
            Self::PS256 => (Ok(&RSA_PSS_2048_8192_SHA256), &SHA256),
            Self::PS384 => (Ok(&RSA_PSS_2048_8192_SHA384), &SHA384),
            Self::PS512 => (Ok(&RSA_PSS_2048_8192_SHA512), &SHA512),
            Self::ES256 => (Err(&ECDSA_P256_SHA256_FIXED), &SHA256),
            Self::ES384 => (Err(&ECDSA_P384_SHA384_FIXED), &SHA384),
            Self::EdDSA => (Err(&ED25519), &SHA512),
        }
    }
}

/// A signing key: an RSA modulus and exponent, or an uncompressed curve point or Ed25519 key.
#[derive(Clone)]
struct Key {
    kid: Option<String>,
    alg: Option<String>,
    public: Result<(Vec<u8>, Vec<u8>), Vec<u8>>,
}

impl Key {
    fn verifies(&self, alg: Alg, message: &[u8], signature: &[u8]) -> bool {
        if self.alg.as_deref().is_some_and(|name| Alg::parse(name) != Some(alg)) {
            return false;
        }
        match (&self.public, alg.scheme().0) {
            (Ok((n, e)), Ok(rsa)) => RsaPublicKeyComponents { n, e }.verify(rsa, message, signature).is_ok(),
            (Err(point), Err(curve)) => UnparsedPublicKey::new(curve, point).verify(message, signature).is_ok(),
            _ => false,
        }
    }
}

/// A provider's signing keys.
#[derive(Default, Clone)]
pub(super) struct Jwks(Vec<Key>);

impl Jwks {
    /// The usable signing keys of a key set; a key not for signatures, or one this server cannot read, is skipped.
    pub fn parse(document: &[u8]) -> Option<Self> {
        #[derive(Deserialize)]
        struct Set {
            keys: Vec<Value>,
        }
        #[derive(Deserialize)]
        struct Jwk {
            kty: String,
            crv: Option<String>,
            kid: Option<String>,
            alg: Option<String>,
            #[serde(rename = "use")]
            usage: Option<String>,
            key_ops: Option<Vec<String>>,
            n: Option<String>,
            e: Option<String>,
            x: Option<String>,
            y: Option<String>,
        }
        let bytes = |value: &Option<String>| B64.decode(value.as_deref()?.trim_end_matches('=')).ok();
        let key = |key: Jwk| {
            let point =
                |size: usize| Some([vec![4], bytes(&key.x)?, bytes(&key.y)?].concat()).filter(|p| p.len() == 1 + size);
            let signs = key.usage.as_deref().is_none_or(|usage| usage == "sig");
            let verifies = key
                .key_ops
                .as_ref()
                .is_none_or(|ops| ops.iter().any(|op| op == "verify"));
            let public = match (key.kty.as_str(), key.crv.as_deref()) {
                _ if !signs || !verifies => return None,
                ("RSA", _) => Ok((unsigned(bytes(&key.n)?), unsigned(bytes(&key.e)?))),
                ("EC", Some("P-256")) => Err(point(64)?),
                ("EC", Some("P-384")) => Err(point(96)?),
                ("OKP", Some("Ed25519")) => Err(bytes(&key.x).filter(|x| x.len() == 32)?),
                _ => return None,
            };
            Some(Key { kid: key.kid, alg: key.alg, public })
        };
        let keys = serde_json::from_slice::<Set>(document).ok()?.keys.into_iter();
        let keys = keys.filter_map(|value| serde_json::from_value(value).ok().and_then(key));
        Some(Self(keys.take(MAX_KEYS).collect()))
    }
}

/// A big-endian integer without leading zeros.
fn unsigned(mut bytes: Vec<u8>) -> Vec<u8> {
    let zeros = bytes.iter().take_while(|&&byte| byte == 0).count();
    bytes.drain(..zeros);
    bytes
}

/// A token whose signature verified, and the algorithm that signed it.
pub(super) struct Verified {
    alg: Alg,
    pub payload: Vec<u8>,
}

/// The token's payload when one of `keys` signed it with an `allowed` algorithm.
pub(super) fn verify(token: &str, keys: &Jwks, allowed: &[Alg]) -> Option<Verified> {
    #[derive(Deserialize)]
    struct Header {
        alg: String,
        kid: Option<String>,
        typ: Option<String>,
        cty: Option<Value>,
        crit: Option<Value>,
        enc: Option<Value>,
    }
    let token = token.trim();
    let (message, signature) = token.rsplit_once('.').filter(|_| token.len() <= MAX_TOKEN)?;
    let (header, payload) = message.split_once('.').filter(|(_, payload)| !payload.contains('.'))?;
    let header: Header = serde_json::from_slice(&B64.decode(header).ok()?).ok()?;
    let jose = ["jwt", "application/jwt", "jose", "application/jose"];
    let typed = header
        .typ
        .is_none_or(|typ| jose.contains(&typ.to_ascii_lowercase().as_str()));
    if header.cty.is_some() || header.crit.is_some() || header.enc.is_some() || !typed {
        return None;
    }
    let alg = Alg::parse(&header.alg).filter(|alg| allowed.contains(alg))?;
    let kid = header.kid.filter(|kid| !kid.is_empty());
    let signature = B64.decode(signature).ok()?;
    let mut named = keys.0.iter().filter(|key| kid.is_none() || key.kid == kid);
    named.find(|key| key.verifies(alg, message.as_bytes(), &signature))?;
    Some(Verified { alg, payload: B64.decode(payload).ok()? })
}

/// Whether claims name `issuer` and include `client` in their audience.
pub(super) fn addressed(payload: &[u8], issuer: &str, client: &str) -> bool {
    #[derive(Deserialize)]
    struct Addressed {
        iss: String,
        aud: Value,
    }
    let Ok(claims) = serde_json::from_slice::<Addressed>(payload) else {
        return false;
    };
    let audience = claims.aud == client
        || claims
            .aud
            .as_array()
            .is_some_and(|many| many.iter().any(|one| one == client));
    claims.iss == issuer && audience
}

/// The subject and the offered names of an ID token for this `client` from `issuer`, checking its validity period,
/// the sign-in's `nonce` and its hash of `access_token`.
pub(super) fn id_token(
    verified: &Verified,
    issuer: &str,
    client: &str,
    nonce: &str,
    access_token: &str,
) -> Result<(String, [Option<String>; 2]), Reason> {
    #[derive(Deserialize)]
    struct Period {
        sub: Option<String>,
        exp: f64,
        nbf: Option<f64>,
        #[serde(rename = "iat")]
        _iat: Option<f64>,
    }
    #[derive(Deserialize)]
    struct Named {
        nonce: Option<String>,
        at_hash: Option<String>,
        name: Option<String>,
        preferred_username: Option<String>,
    }
    let payload = &verified.payload;
    let now = SystemTime::now().duration_since(UNIX_EPOCH);
    let now = now.unwrap_or_default().as_secs_f64();
    let current = |period: &Period| now <= period.exp && period.nbf.is_none_or(|nbf| nbf <= now + SKEW);
    let period = serde_json::from_slice::<Period>(payload).ok().filter(current);
    let Some(period) = period.filter(|_| addressed(payload, issuer, client)) else {
        return Err(Reason::IdTokenVerification);
    };
    let named: Named = serde_json::from_slice(payload).map_err(|_| Reason::IdTokenClaimsOrNonce)?;
    let claimed = named.nonce.unwrap_or_default();
    if claimed.is_empty() || !bool::from(claimed.as_bytes().ct_eq(nonce.as_bytes())) {
        return Err(Reason::IdTokenClaimsOrNonce);
    }
    let hash = digest::digest(verified.alg.scheme().1, access_token.as_bytes());
    let hash = B64.encode(&hash.as_ref()[..hash.as_ref().len() / 2]);
    if named
        .at_hash
        .is_some_and(|at_hash| !at_hash.is_empty() && at_hash != hash)
    {
        return Err(Reason::AccessTokenHash);
    }
    Ok((period.sub.unwrap_or_default(), [named.name, named.preferred_username]))
}
