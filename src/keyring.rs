// Secrets in the system keyring: Secret Service (libsecret) on Unix,
// Credential Manager on Windows.

/// Keyring id for an SSH login password.
pub fn password_id(target: &str) -> String {
    format!("password:{target}")
}

/// Keyring id for a private key passphrase.
pub fn key_id(path: &str) -> String {
    format!("key:{path}")
}

pub use backend::{clear, get, set};

#[cfg(unix)]
mod backend {
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
}

#[cfg(windows)]
mod backend {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
    use windows_sys::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn target(id: &str) -> Vec<u16> {
        wide(&format!("termvisor:{id}"))
    }

    pub fn get(id: &str) -> Option<String> {
        let t = target(id);
        let mut c: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: valid name; the blob is read before CredFree.
        unsafe {
            if CredReadW(t.as_ptr(), CRED_TYPE_GENERIC, 0, &mut c) == 0 {
                if GetLastError() != ERROR_NOT_FOUND {
                    crate::warn(&format!("keyring lookup: error {}", GetLastError()));
                }
                return None;
            }
            let blob = std::slice::from_raw_parts((*c).CredentialBlob, (*c).CredentialBlobSize as usize);
            let secret = String::from_utf8(blob.to_vec()).ok();
            CredFree(c as *const _);
            secret
        }
    }

    pub fn set(id: &str, secret: &str) -> Result<(), String> {
        let mut t = target(id);
        let mut user = wide("termvisor");
        let blob = secret.as_bytes();
        // SAFETY: every pointer outlives the call.
        unsafe {
            let mut c: CREDENTIALW = std::mem::zeroed();
            c.Type = CRED_TYPE_GENERIC;
            c.TargetName = t.as_mut_ptr();
            c.UserName = user.as_mut_ptr();
            c.CredentialBlobSize = blob.len() as u32;
            c.CredentialBlob = blob.as_ptr() as *mut u8;
            c.Persist = CRED_PERSIST_LOCAL_MACHINE;
            if CredWriteW(&c, 0) == 0 {
                return Err(format!("keyring store: error {}", GetLastError()));
            }
        }
        Ok(())
    }

    pub fn clear(id: &str) -> Result<(), String> {
        let t = target(id);
        // SAFETY: valid name.
        unsafe {
            if CredDeleteW(t.as_ptr(), CRED_TYPE_GENERIC, 0) == 0 && GetLastError() != ERROR_NOT_FOUND {
                return Err(format!("keyring clear: error {}", GetLastError()));
            }
        }
        Ok(())
    }
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
