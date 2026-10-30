// Secrets in the system keyring (Secret Service via libsecret).

use std::collections::HashMap;

use gtk::gio;
use libsecret::{Schema, SchemaAttributeType, SchemaFlags};

fn schema() -> Schema {
    Schema::new(
        "io.github.ykurt52.termvisor.Secret",
        SchemaFlags::NONE,
        HashMap::from([("id", SchemaAttributeType::String)]),
    )
}

/// Keyring id for an SSH login password.
pub fn password_id(target: &str) -> String {
    format!("password:{target}")
}

/// Keyring id for a private key passphrase.
pub fn key_id(path: &str) -> String {
    format!("key:{path}")
}

pub fn get(id: &str) -> Option<String> {
    match libsecret::password_lookup_sync(Some(&schema()), HashMap::from([("id", id)]), None::<&gio::Cancellable>) {
        Ok(v) => v.map(|s| s.to_string()),
        Err(e) => {
            crate::warn(&format!("keyring lookup: {e}"));
            None
        }
    }
}

pub fn set(id: &str, secret: &str) -> Result<(), String> {
    libsecret::password_store_sync(
        Some(&schema()),
        HashMap::from([("id", id)]),
        Some(libsecret::COLLECTION_DEFAULT),
        &format!("termvisor {id}"),
        secret,
        None::<&gio::Cancellable>,
    )
    .map_err(|e| format!("keyring store: {e}"))
}

pub fn clear(id: &str) -> Result<(), String> {
    libsecret::password_clear_sync(Some(&schema()), HashMap::from([("id", id)]), None::<&gio::Cancellable>)
        .map_err(|e| format!("keyring clear: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real keyring; run with `cargo test -- --ignored keyring`.
    #[test]
    #[ignore]
    fn keyring_roundtrip() {
        let id = "selftest:termvisor";
        set(id, "s3cret").expect("set");
        assert_eq!(get(id).as_deref(), Some("s3cret"));
        clear(id).expect("clear");
        assert_eq!(get(id), None);
    }
}
