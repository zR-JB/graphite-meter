//! Strict JSON objects, read as Go's `encoding/json/v2` reads control responses and progress records.

use serde::de::{DeserializeOwned, Error as _};
use std::collections::HashSet;

/// Decodes a JSON object, refusing invalid UTF-8, an escape that is no character and a repeated member name
/// at any depth; unknown members are skipped unconverted.
pub fn decode<T: DeserializeOwned>(data: &[u8]) -> Result<T, serde_json::Error> {
    let text = std::str::from_utf8(data).map_err(serde_json::Error::custom)?;
    check(text)?;
    serde_json::from_str(text)
}

/// Refuses what serde_json would accept from valid JSON and Go would not; serde_json refuses the rest.
fn check(text: &str) -> Result<(), serde_json::Error> {
    if !text.trim_start().starts_with('{') {
        return Err(serde_json::Error::custom("expected a JSON object"));
    }
    // Each open container: an object's member names, or `None` for an array.
    let mut open: Vec<Option<HashSet<String>>> = Vec::new();
    let (mut at, mut name_next) = (0, false);
    while let Some(&byte) = text.as_bytes().get(at) {
        match byte {
            b'{' => {
                open.push(Some(HashSet::new()));
                name_next = true;
            }
            b'[' => open.push(None),
            b'}' | b']' => drop(open.pop()),
            b',' => name_next = matches!(open.last(), Some(Some(_))),
            b'"' => {
                let mut strings = serde_json::Deserializer::from_str(&text[at..]).into_iter::<String>();
                let Some(string) = strings.next().transpose()? else { break };
                at += strings.byte_offset();
                if name_next
                    && let Some(Some(names)) = open.last_mut()
                    && !names.insert(string)
                {
                    return Err(serde_json::Error::custom("duplicate JSON member"));
                }
                name_next = false;
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    Ok(())
}
