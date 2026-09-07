//! Error-code catalog operations shared by the logger and the Rust CLI.

use super::config::{invalid, resolve_environment_references, ConfigError};
use rand::RngCore;
use serde::{
    de::{MapAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Write,
    path::Path,
};

pub type ErrorCodeMap = BTreeMap<String, String>;

/// Reject duplicate JSON5 keys before an ordinary map could silently overwrite them.
struct UniqueEntries(BTreeMap<String, Value>);

impl<'de> Deserialize<'de> for UniqueEntries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntriesVisitor;
        impl<'de> Visitor<'de> for EntriesVisitor {
            type Value = UniqueEntries;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object with unique error keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    if entries.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("Duplicate error key"));
                    }
                }
                Ok(UniqueEntries(entries))
            }
        }
        deserializer.deserialize_map(EntriesVisitor)
    }
}

pub fn parse_error_codes_with_environment(
    text: &str,
    environment: &impl Fn(&str) -> Option<String>,
) -> Result<ErrorCodeMap, ConfigError> {
    let entries: UniqueEntries = json5::from_str(text)
        .map_err(|_| invalid("Invalid error-code JSON5: expected an object with unique keys"))?;
    let mut value = Value::Object(entries.0.into_iter().collect());
    resolve_environment_references(&mut value, environment);
    let map: ErrorCodeMap = serde_json::from_value(value)
        .map_err(|_| invalid("Error codes must resolve to strings"))?;
    validate_error_codes(&map)?;
    Ok(map)
}

pub fn load_error_codes(path: impl AsRef<Path>) -> Result<ErrorCodeMap, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|_| invalid("Cannot read error-code file"))?;
    parse_error_codes_with_environment(&text, &|name| std::env::var(name).ok())
}

/// Exactly the styleguide's 15 hex payload nibbles followed by a checksum nibble.
pub fn validate_error_code(code: &str) -> bool {
    code.len() == 16
        && code
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(&c))
        && checksum(&code[..15]) == code.as_bytes()[15] as char
}

pub fn validate_error_codes(map: &ErrorCodeMap) -> Result<(), ConfigError> {
    let mut seen = BTreeSet::new();
    for (key, code) in map {
        validate_key(key)?;
        if !validate_error_code(code) {
            return Err(invalid("Catalog contains an invalid error code (expected 16 uppercase hex digits with a valid checksum)"));
        }
        if !seen.insert(code) {
            return Err(invalid("Catalog contains duplicate error codes"));
        }
    }
    Ok(())
}

fn validate_key(key: &str) -> Result<(), ConfigError> {
    if key.trim().is_empty() {
        return Err(invalid("Error key must not be blank"));
    }
    Ok(())
}

fn checksum(payload: &str) -> char {
    let sum: u32 = payload.chars().filter_map(|c| c.to_digit(16)).sum();
    char::from_digit(sum % 16, 16).unwrap().to_ascii_uppercase()
}

fn stable_hex(input: &str, length: usize) -> String {
    format!("{:X}", Sha256::digest(input.as_bytes()))[..length].to_owned()
}

pub fn add_error_code(
    map: &mut ErrorCodeMap,
    key: &str,
    prefix: &str,
    deterministic: bool,
) -> Result<String, ConfigError> {
    validate_error_codes(map)?;
    validate_key(key)?;
    if map.contains_key(key) {
        return Err(invalid("Error key already exists"));
    }
    let prefix = prefix.to_ascii_uppercase();
    if prefix.len() > 4 || !prefix.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid("Prefix must contain 0–4 hexadecimal characters"));
    }
    let stable_len = 10 - prefix.len();
    for attempt in 0..32 {
        let input = format!("{key}:{attempt}");
        let entropy = if deterministic {
            stable_hex(&format!("{input}:entropy"), 4)
        } else {
            let mut bytes = [0_u8; 2];
            rand::rngs::OsRng
                .try_fill_bytes(&mut bytes)
                .map_err(|_| invalid("Cannot obtain randomness for error code"))?;
            format!("{:02X}{:02X}", bytes[0], bytes[1])
        };
        let payload = format!(
            "{prefix}{}{entropy}{:X}",
            stable_hex(&input, stable_len),
            map.len() % 16
        );
        let candidate = format!("{payload}{}", checksum(&payload));
        if !map.values().any(|code| code == &candidate) {
            map.insert(key.to_owned(), candidate.clone());
            return Ok(candidate);
        }
    }
    Err(invalid(
        "Unable to generate a unique error code after 32 attempts",
    ))
}

/// Rename a key, replace its code, or both; a rename alone preserves the code.
pub fn edit_error_code(
    map: &mut ErrorCodeMap,
    key: &str,
    new_key: Option<&str>,
    new_code: Option<&str>,
) -> Result<String, ConfigError> {
    validate_error_codes(map)?;
    let old = map
        .get(key)
        .ok_or_else(|| invalid("Error key does not exist"))?;
    if new_key.is_none() && new_code.is_none() {
        return Err(invalid("Edit requires --new-error-key and/or --error-code"));
    }
    let target = new_key.unwrap_or(key);
    validate_key(target)?;
    if target != key && map.contains_key(target) {
        return Err(invalid("New error key already exists"));
    }
    let code = new_code
        .map(str::to_ascii_uppercase)
        .unwrap_or_else(|| old.clone());
    if !validate_error_code(&code) {
        return Err(invalid("Invalid replacement error code"));
    }
    if map
        .iter()
        .any(|(other_key, other_code)| other_key != key && other_code == &code)
    {
        return Err(invalid("Replacement error code is already in use"));
    }
    map.remove(key);
    map.insert(target.to_owned(), code.clone());
    Ok(code)
}

pub fn delete_error_code(map: &mut ErrorCodeMap, key: &str) -> Result<String, ConfigError> {
    map.remove(key)
        .ok_or_else(|| invalid("Error key does not exist"))
}

/// Atomically write a sorted catalog. Strict JSON is also valid JSON5.
/// Comments/formatting are canonicalized on mutation, never on read-only actions.
pub fn save_error_codes(path: impl AsRef<Path>, map: &ErrorCodeMap) -> Result<(), ConfigError> {
    validate_error_codes(map)?;
    write_catalog(path.as_ref(), map)
}

/// Validate resolved codes, but persist the original `${ENV_VAR}` reference strings.
pub fn save_error_codes_preserving_references(
    path: impl AsRef<Path>,
    map: &ErrorCodeMap,
) -> Result<(), ConfigError> {
    let text =
        serde_json::to_string(map).map_err(|_| invalid("Cannot serialize error-code catalog"))?;
    parse_error_codes_with_environment(&text, &|name| std::env::var(name).ok())?;
    write_catalog(path.as_ref(), map)
}

fn write_catalog(path: &Path, map: &ErrorCodeMap) -> Result<(), ConfigError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|_| invalid("Cannot create error-code directory"))?;
    let mut output = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| invalid("Cannot create temporary error-code file"))?;
    if let Ok(metadata) = std::fs::metadata(path) {
        output
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(|_| invalid("Cannot preserve error-code file permissions"))?;
    }
    serde_json::to_writer_pretty(&mut output, map)
        .map_err(|_| invalid("Cannot write error-code catalog"))?;
    output
        .write_all(b"\n")
        .and_then(|_| output.as_file().sync_all())
        .map_err(|_| invalid("Cannot flush error-code catalog"))?;
    output
        .persist(path)
        .map_err(|_| invalid("Cannot replace error-code file"))?;
    Ok(())
}
