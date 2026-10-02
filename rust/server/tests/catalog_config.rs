use graphite_meter_server::catalog::{load, parse};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};

struct TempFile(std::path::PathBuf);
impl TempFile {
    fn new(data: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "gm-catalog-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, data).unwrap();
        Self(path)
    }
}
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[test]
fn source_presence_and_inline_file_equivalence() {
    assert_eq!(load(None, None).unwrap().servers[0].id, "self");
    assert!(load(Some(""), Some("")).is_err());
    assert!(load(Some("{}"), Some("/unused")).is_err());
    assert!(load(Some(""), None).is_err());
    assert!(load(None, Some("")).is_err());
    let raw = r#"["https://EXAMPLE.net:443/"]"#;
    let file = TempFile::new(raw.as_bytes());
    let inline = load(Some(raw), None).unwrap();
    assert_eq!(inline, load(None, Some(file.0.to_str().unwrap())).unwrap());
    assert_eq!(inline.servers[1].url, "https://example.net");
}

#[test]
fn null_and_missing_defaults_match_go() {
    for raw in [
        "null",
        "{}",
        r#"{"servers":null,"defaultSelection":null}"#,
        r#"{"servers":[]}"#,
        "[]",
    ] {
        assert_eq!(parse(raw.as_bytes()).unwrap(), load(None, None).unwrap(), "{raw}");
    }
    for raw in [
        r#"{"defaultSelection":[]}"#,
        r#"{"defaultSelection":[null]}"#,
        r#"{"servers":[null]}"#,
        "[null]",
    ] {
        assert!(parse(raw.as_bytes()).is_err(), "{raw}");
    }
}

#[test]
fn rejects_unsafe_ambiguous_and_duplicate_json() {
    for raw in [
        r#"{"defaultSelection":["missing"]}"#,
        r#"{"servers":[{"id":"self","url":"https://example.net"}]}"#,
        r#"{"servers":[{"id":"a","url":"https://example.net"},{"id":"b","url":"https://EXAMPLE.net:443"}]}"#,
        r#"{"servers":[],"typo":true}"#,
        r#"{"servers":[{"id":"a","url":"https://example.net","typo":true}]}"#,
        r#"{"servers":[],"servers":[]}"#,
        r#"{"defaultSelection":null,"defaultSelection":["self"]}"#,
        r#"{"servers":[{"id":null,"id":"a","url":"https://example.net"}]}"#,
        "[42]",
        "[{}]",
        "[",
        "{} {}",
        "42",
        "true",
    ] {
        assert!(parse(raw.as_bytes()).is_err(), "{raw}");
    }
    for origin in [
        "https://user:pass@example.net",
        "https://example.net/path",
        "https://*.example.net",
        "https://example.net;evil",
        "https://example.net?x=1",
        "https://example.net#x",
        "https://example.net//",
    ] {
        assert!(
            parse(serde_json::to_string(&[origin]).unwrap().as_bytes()).is_err(),
            "{origin}"
        );
    }
}

#[test]
fn bounds_raw_and_normalized_sizes() {
    let mut exact = b"{}".to_vec();
    exact.resize(64 << 10, b' ');
    parse(&exact).unwrap();
    exact.push(b' ');
    assert!(parse(&exact).is_err());
    let file = TempFile::new(&exact);
    assert!(load(None, Some(file.0.to_str().unwrap())).is_err());
    // Short input grows beyond 48KiB through derived IDs and authority names.
    let origins: Vec<_> = (0..31)
        .map(|i| format!("https://s{i}.{}.example", "a".repeat(900)))
        .collect();
    let bytes = serde_json::to_vec(&origins).unwrap();
    assert!(bytes.len() < 64 << 10);
    assert!(parse(&bytes).unwrap_err().to_string().contains("normalized catalogue"));
}
