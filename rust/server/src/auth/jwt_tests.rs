use super::super::test_keys::Signers;
use super::*;
use serde_json::json;

const ALL: [Alg; 9] = Alg::ALL;

#[test]
fn every_ring_algorithm_verifies_against_the_matching_key_only() {
    let signers = Signers::new(b"c2VjcmV0");
    let keys = Jwks::parse(signers.jwks().to_string().as_bytes()).unwrap();
    let claims = json!({"sub": "operator"});
    let check = |header: Value| verify(&signers.sign(header, &claims), &keys, &ALL);
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
        let verified = check(json!({"alg": alg, "kid": kid})).unwrap();
        let payload: Value = serde_json::from_slice(&verified.payload).unwrap();
        assert_eq!(payload["sub"], "operator", "{alg}");
        for header in [json!({"alg": alg}), json!({"alg": alg, "kid": ""}), json!({"alg": alg, "kid": null})] {
            assert!(check(header.clone()).is_ok(), "{header}");
        }
    }
    let refused = [
        (json!({"alg": "RS256", "kid": "p256"}), Reject::UnknownKey),
        (json!({"alg": "ES256", "kid": "rsa"}), Reject::UnknownKey),
        (json!({"alg": "ES384", "kid": "p256"}), Reject::UnknownKey),
        (json!({"alg": "RS256", "kid": "enc"}), Reject::UnknownKey),
        (json!({"alg": "RS256", "kid": "missing"}), Reject::UnknownKey),
        (json!({"alg": "HS256", "kid": "mac"}), Reject::Algorithm),
        (json!({"alg": "ES512", "kid": "p256"}), Reject::Algorithm),
        (json!({"alg": "none"}), Reject::Algorithm),
        (json!({"kid": "rsa"}), Reject::Algorithm),
        (json!({"alg": "RS256", "kid": "rsa", "crit": ["exp"]}), Reject::Malformed),
        (json!({"alg": "RS256", "kid": "rsa", "cty": "JWT"}), Reject::Malformed),
        (json!({"alg": "RS256", "kid": "rsa", "typ": "at+jwt"}), Reject::Malformed),
        (json!({"alg": "RS256", "kid": 7}), Reject::Malformed),
    ];
    for (header, reject) in refused {
        assert_eq!(check(header.clone()).err(), Some(reject), "{header}");
    }
    let token = signers.sign(json!({"alg": "RS256", "kid": "rsa", "typ": "JWT"}), &claims);
    assert!(verify(&token, &keys, &ALL).is_ok());
    // go-jose strips whitespace, which a provider may wrap the token in.
    assert!(verify(&format!(" {token}\r\n"), &keys, &ALL).is_ok());
    assert_eq!(verify(&token, &keys, &[Alg::ES256]).err(), Some(Reject::Algorithm));
    let (message, _) = token.rsplit_once('.').unwrap();
    let forged = format!("{message}.{}", B64.encode([0; 256]));
    assert_eq!(verify(&forged, &keys, &ALL).err(), Some(Reject::Signature));
    let extra = format!("{token}.extra.parts");
    assert_eq!(verify(&extra, &keys, &ALL).err(), Some(Reject::Malformed));
    let public: signature::RsaPublicKeyComponents<Vec<u8>> = signers.rsa.public().into();
    let rs256 = signers.sign(json!({"alg": "RS256"}), &claims);
    for extra in [
        json!({"alg": "PS256"}),
        json!({"alg": 7}),
        json!({"use": 7}),
        json!({"use": "enc"}),
        json!({"key_ops": ["sign"]}),
        json!({"key_ops": "verify"}),
    ] {
        let mut key = json!({"kty": "RSA", "n": B64.encode(&public.n), "e": B64.encode(&public.e)});
        key.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let keys = Jwks::parse(json!({"keys": [key]}).to_string().as_bytes()).unwrap();
        assert_eq!(verify(&rs256, &keys, &ALL).err(), Some(Reject::UnknownKey));
    }
    // Keys no token can use count nothing towards the cap.
    let mut junk: Vec<_> = (0..64)
        .map(|kid| json!({"kty": "oct", "kid": kid.to_string(), "k": "AAAA"}))
        .collect();
    junk.push(json!({"kty": "RSA", "n": B64.encode(&public.n), "e": B64.encode(&public.e)}));
    let keys = Jwks::parse(json!({"keys": junk}).to_string().as_bytes()).unwrap();
    assert!(verify(&rs256, &keys, &ALL).is_ok());
}

#[test]
fn id_token_claims_bind_issuer_audience_nonce_time_and_access_token() {
    let signers = Signers::new(b"c2VjcmV0");
    let keys = Jwks::parse(signers.jwks().to_string().as_bytes()).unwrap();
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
    let sign_in = |alg: &str, kid: &str, claims: &Value| {
        let verified = verify(&signers.sign(json!({"alg": alg, "kid": kid}), claims), &keys, &ALL).unwrap();
        id_token(verified, &expected).map(|claims| claims.subject)
    };
    // The base claims with `changes` set; a null change sends null.
    let check = |alg: &str, kid: &str, changes: Value| {
        let mut claims = base.clone();
        for (name, value) in changes.as_object().unwrap() {
            claims[name] = value.clone();
        }
        sign_in(alg, kid, &claims)
    };
    for (absent, expected) in [
        ("iat", Ok("operator".to_owned())),
        ("at_hash", Ok("operator".to_owned())),
        ("exp", Err(Reject::Claims)),
        ("nonce", Err(Reject::Nonce)),
        ("aud", Err(Reject::Claims)),
    ] {
        let mut claims = base.clone();
        claims.as_object_mut().unwrap().remove(absent);
        assert_eq!(sign_in("RS256", "rsa", &claims), expected, "{absent}");
    }
    // As in go-oidc, a null subject is empty, which the user information check refuses.
    assert_eq!(check("RS256", "rsa", json!({"sub": null})), Ok(String::new()));
    // Go's decoder reads each byte of invalid UTF-8 as U+FFFD.
    let mut payload = base.to_string().into_bytes();
    payload.pop();
    payload.extend_from_slice(b",\"preferred_username\":\"J\xf6rg\"}");
    let latin1 = id_token(Verified { alg: Alg::RS256, payload }, &expected).map(|claims| claims.preferred_username);
    assert_eq!(latin1, Ok(Some("J\u{fffd}rg".into())));
    for (alg, kid, digest) in [
        ("RS256", "rsa", &ring::digest::SHA256),
        ("ES384", "p384", &ring::digest::SHA384),
        ("EdDSA", "ed", &ring::digest::SHA512),
    ] {
        let subject = check(alg, kid, json!({"at_hash": at_hash(digest)}));
        assert_eq!(subject, Ok("operator".into()), "{alg}");
    }
    for changes in [
        json!({"aud": ["meter"], "azp": "meter", "nbf": now + 299}),
        json!({"aud": ["other", "meter"], "azp": "other"}),
        json!({"aud": ["meter", "meter"]}),
        json!({"aud": ["meter", null]}),
        // go-oidc's int64 seconds, which time.Unix wraps into the past.
        json!({"nbf": 1e300}),
        json!({"nbf": i64::MAX}),
        json!({"iat": now.to_string()}),
        json!({"exp": (now + 300).to_string(), "nbf": format!("{now}.5"), "iat": 1e9}),
        json!({"at_hash": null, "nbf": null, "name": null, "preferred_username": null}),
        json!({"_claim_names": {"groups": "a"}, "_claim_sources": {"a": {"endpoint": "https://x"}, "b": null}}),
    ] {
        assert!(check("RS256", "rsa", changes.clone()).is_ok(), "{changes}");
    }
    for (changes, reject) in [
        (json!({"iss": "https://id.example/"}), Reject::Claims),
        (json!({"iss": null}), Reject::Claims),
        (json!({"aud": "other"}), Reject::Claims),
        (json!({"aud": []}), Reject::Claims),
        (json!({"aud": ["meter", 7]}), Reject::Claims),
        (json!({"aud": null}), Reject::Claims),
        (json!({"exp": now}), Reject::Claims),
        (json!({"exp": i64::MAX}), Reject::Claims),
        (json!({"exp": u64::MAX}), Reject::Claims),
        (json!({"exp": 1e300}), Reject::Claims),
        (json!({"exp": null}), Reject::Claims),
        (json!({"iat": null}), Reject::Claims),
        (json!({"iat": "soon"}), Reject::Claims),
        (json!({"exp": format!(" {}", now + 300)}), Reject::Claims),
        (json!({"exp": now as f64 + 0.5}), Reject::Claims),
        (json!({"nbf": true}), Reject::Claims),
        (json!({"_claim_names": {"groups": "a"}}), Reject::Claims),
        (json!({"_claim_names": {"groups": ""}, "_claim_sources": {"": {}}}), Reject::Claims),
        (json!({"_claim_sources": {"a": {"access_token": 7}}}), Reject::Claims),
        (json!({"nbf": now + 301}), Reject::Claims),
        (json!({"sub": 7}), Reject::Claims),
        (json!({"nonce": "other"}), Reject::Nonce),
        (json!({"nonce": null}), Reject::Nonce),
        (json!({"name": 7}), Reject::Nonce),
        (json!({"at_hash": at_hash(&ring::digest::SHA384)}), Reject::AccessTokenHash),
    ] {
        assert_eq!(check("RS256", "rsa", changes.clone()).err(), Some(reject), "{changes}");
    }
}
