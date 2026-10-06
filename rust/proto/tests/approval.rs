use graphite_meter_proto::approval::{challenge, verification_code};

#[test]
fn challenges_are_s256_digests_in_unpadded_base64url() {
    // RFC 7636, appendix B.
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    assert_eq!(challenge(verifier), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    assert_eq!(challenge("verifier"), "iMnq5o6zALKXGivsnlom_0F5_WYda32GHkxlV7mq7hQ");
}

#[test]
fn both_pages_show_the_digest_start_in_base32() {
    assert_eq!(verification_code(&challenge("verifier")).as_deref(), Some("RDE6VZUO"));
    let rfc = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    assert_eq!(verification_code(rfc).as_deref(), Some("CPJR5FQ2"));
    assert_eq!(
        verification_code("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cN").as_deref(),
        Some("CPJR5FQ2")
    );
}

#[test]
fn a_challenge_that_is_no_s256_digest_has_no_code() {
    let digest = challenge("verifier");
    for refused in [
        String::new(),
        digest[..42].to_owned(),
        format!("{digest}A"),
        format!("{digest}="),
        digest.replace('_', "/"),
        format!("{digest}{}", "A".repeat(30)),
    ] {
        assert_eq!(verification_code(&refused), None, "{refused}");
    }
}
