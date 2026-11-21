//! A secret longer than one keyring entry holds, kept as several.
//!
//! The Windows Credential Manager refuses a secret over 2560 bytes, which is
//! 1280 characters as `keyring` writes them. Access tokens that are JWTs and
//! OAuth credential sets are often longer. Such a secret is cut into parts,
//! each in an entry of its own, and the entry under the secret's key holds a
//! header naming them instead of the secret.
//!
//! A secret that fits is stored as it always was, so entries written before
//! this existed read back unchanged.

use crate::SecretStore;

/// What the entry under a key starts with when it names parts. A secret that
/// starts with it is stored in parts whatever its length, so an entry that
/// starts with it is always a header.
const HEADER: &str = "coco-mcp-parts:1:";

/// The parts of one secret: `count` entries named after `id`.
///
/// Each write of a secret takes a fresh `id`, so the parts of the value being
/// replaced stay whole until the header names the new ones.
#[derive(Debug, PartialEq, Eq)]
struct Parts {
    id: String,
    count: usize,
}

impl Parts {
    fn parse(entry: &str) -> Option<Self> {
        let (id, count) = entry.strip_prefix(HEADER)?.split_once(':')?;
        Some(Self {
            id: id.to_owned(),
            count: count.parse().ok()?,
        })
    }

    fn header(&self) -> String {
        format!("{HEADER}{}:{}", self.id, self.count)
    }

    /// The entry name of part `index` of the secret under `key`.
    fn entry(&self, key: &str, index: usize) -> String {
        format!("{key}:part:{}:{index}", self.id)
    }

    /// Remove the parts, written or not. Every one is tried; the first
    /// failure is returned.
    fn remove(&self, entries: &dyn SecretStore, key: &str) -> std::result::Result<(), String> {
        let mut removed = Ok(());
        for index in 0..self.count {
            removed = removed.and(entries.delete(&self.entry(key, index)));
        }
        removed
    }
}

/// The parts the entry under `key` names, if it names any.
fn parts_of(entries: &dyn SecretStore, key: &str) -> Option<Parts> {
    Parts::parse(&entries.get(key).ok()??)
}

/// Read the secret under `key`, joining its parts.
pub(crate) fn get(
    entries: &dyn SecretStore,
    key: &str,
) -> std::result::Result<Option<String>, String> {
    let Some(entry) = entries.get(key)? else {
        return Ok(None);
    };
    if !entry.starts_with(HEADER) {
        return Ok(Some(entry));
    }
    let parts =
        Parts::parse(&entry).ok_or_else(|| format!("the keyring entry `{key}` is damaged"))?;
    let mut secret = String::new();
    for index in 0..parts.count {
        let part = entries.get(&parts.entry(key, index))?.ok_or_else(|| {
            format!(
                "part {} of {} of the keyring entry `{key}` is missing",
                index + 1,
                parts.count
            )
        })?;
        secret.push_str(&part);
    }
    Ok(Some(secret))
}

/// Write `value` under `key`, in parts when it is over `limit` UTF-16 code
/// units, then remove the parts of the value it replaces.
///
/// A write that fails leaves the value that was there readable.
pub(crate) fn set(
    entries: &dyn SecretStore,
    limit: usize,
    key: &str,
    value: &str,
) -> std::result::Result<(), String> {
    let replaced = parts_of(entries, key);
    if value.encode_utf16().count() <= limit && !value.starts_with(HEADER) {
        entries.set(key, value)?;
    } else {
        let pieces = split(value, limit);
        let parts = Parts {
            id: uuid::Uuid::now_v7().simple().to_string(),
            count: pieces.len(),
        };
        let written = pieces
            .iter()
            .enumerate()
            .try_for_each(|(index, piece)| entries.set(&parts.entry(key, index), piece))
            .and_then(|()| entries.set(key, &parts.header()));
        if let Err(e) = written {
            // The header still names the old parts, if any: drop the new.
            let _ = parts.remove(entries, key);
            return Err(e);
        }
    }
    // The value is stored; a part left behind is only clutter.
    if let Some(old) = replaced
        && let Err(e) = old.remove(entries, key)
    {
        tracing::warn!("parts of the replaced keyring entry `{key}` were not removed: {e}");
    }
    Ok(())
}

/// Remove the secret under `key` and its parts. The parts go first, so a
/// removal that fails midway can be tried again.
pub(crate) fn delete(entries: &dyn SecretStore, key: &str) -> std::result::Result<(), String> {
    if let Some(parts) = parts_of(entries, key) {
        parts.remove(entries, key)?;
    }
    entries.delete(key)
}

/// Cut `value` into pieces of at most `limit` UTF-16 code units, never
/// inside a character.
fn split(value: &str, limit: usize) -> Vec<&str> {
    let mut pieces = Vec::new();
    let (mut start, mut units) = (0, 0);
    for (at, c) in value.char_indices() {
        if units + c.len_utf16() > limit {
            pieces.push(&value[start..at]);
            (start, units) = (at, 0);
        }
        units += c.len_utf16();
    }
    pieces.push(&value[start..]);
    pieces
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::MemoryStore;

    const LIMIT: usize = 64;

    /// A keyring that refuses a secret over `LIMIT` UTF-16 code units, as
    /// the Windows Credential Manager does over its own, and every write
    /// once `full`.
    #[derive(Default)]
    struct Capped {
        entries: MemoryStore,
        full: AtomicBool,
    }

    impl SecretStore for Capped {
        fn get(&self, key: &str) -> std::result::Result<Option<String>, String> {
            self.entries.get(key)
        }

        fn set(&self, key: &str, value: &str) -> std::result::Result<(), String> {
            if self.full.load(Ordering::Relaxed) {
                return Err("full".into());
            }
            if value.encode_utf16().count() > LIMIT {
                return Err("longer than the platform limit".into());
            }
            self.entries.set(key, value)
        }

        fn delete(&self, key: &str) -> std::result::Result<(), String> {
            self.entries.delete(key)
        }
    }

    fn long(seed: &str) -> String {
        seed.repeat(40)
    }

    #[test]
    fn a_secret_that_fits_is_one_entry_as_before() {
        let keyring = Capped::default();
        let token = "a".repeat(LIMIT);
        set(&keyring, LIMIT, "k", &token).unwrap();
        assert_eq!(keyring.entries.get("k").unwrap(), Some(token.clone()));
        assert_eq!(keyring.entries.len(), 1);
        assert_eq!(get(&keyring, "k").unwrap(), Some(token));
        delete(&keyring, "k").unwrap();
        assert!(keyring.entries.is_empty());
        assert_eq!(get(&keyring, "k").unwrap(), None);
    }

    #[test]
    fn a_secret_over_the_limit_round_trips_in_parts() {
        let keyring = Capped::default();
        // Two UTF-16 units a character, and one more than a part holds.
        for token in ["a".repeat(LIMIT + 1), long("eyJ.🔑é")] {
            set(&keyring, LIMIT, "k", &token).unwrap();
            assert!(keyring.entries.len() > 1);
            assert_eq!(get(&keyring, "k").unwrap(), Some(token));
        }
        delete(&keyring, "k").unwrap();
        assert!(keyring.entries.is_empty(), "no part is left behind");
    }

    #[test]
    fn replacing_a_secret_leaves_only_the_new_one() {
        let keyring = Capped::default();
        set(&keyring, LIMIT, "k", &long("old")).unwrap();
        set(&keyring, LIMIT, "k", &long("new-and-longer")).unwrap();
        assert_eq!(get(&keyring, "k").unwrap(), Some(long("new-and-longer")));
        set(&keyring, LIMIT, "k", "short").unwrap();
        assert_eq!(get(&keyring, "k").unwrap().as_deref(), Some("short"));
        assert_eq!(
            keyring.entries.len(),
            1,
            "the parts went with the old value"
        );
    }

    #[test]
    fn a_write_the_keyring_refuses_keeps_the_old_secret() {
        let keyring = Capped::default();
        set(&keyring, LIMIT, "k", &long("old")).unwrap();
        let before = keyring.entries.len();
        keyring.full.store(true, Ordering::Relaxed);
        assert_eq!(set(&keyring, LIMIT, "k", &long("new")), Err("full".into()));
        assert_eq!(get(&keyring, "k").unwrap(), Some(long("old")));
        assert_eq!(keyring.entries.len(), before, "nothing new is left behind");
    }

    #[test]
    fn a_secret_that_looks_like_a_header_is_still_a_secret() {
        let keyring = Capped::default();
        let token = format!("{HEADER}abc:3");
        set(&keyring, LIMIT, "k", &token).unwrap();
        assert_eq!(get(&keyring, "k").unwrap(), Some(token));
        delete(&keyring, "k").unwrap();
        assert!(keyring.entries.is_empty());
    }

    #[test]
    fn a_missing_part_is_an_error_not_a_shorter_secret() {
        let keyring = Capped::default();
        set(&keyring, LIMIT, "k", &long("token")).unwrap();
        let parts = parts_of(&keyring, "k").unwrap();
        keyring.entries.delete(&parts.entry("k", 1)).unwrap();
        let error = get(&keyring, "k").unwrap_err();
        assert!(error.contains("part 2 of"), "{error}");
        // What is left can still be removed.
        delete(&keyring, "k").unwrap();
        assert!(keyring.entries.is_empty());
    }

    #[test]
    fn pieces_hold_whole_characters() {
        assert_eq!(split("abcde", 2), ["ab", "cd", "e"]);
        assert_eq!(split("a🔑b", 2), ["a", "🔑", "b"]);
        assert_eq!(split("abcd", 4), ["abcd"]);
    }
}
