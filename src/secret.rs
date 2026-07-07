//! Cross-platform secret protection for the credential fields in `state.json`.
//!
//! `protect`/`unprotect` encrypt small secrets (API tokens, proxy URLs) tied to
//! the current user, so the on-disk `state.json` never exposes credentials in
//! plaintext to other processes or offline copies of the file.
//!
//! - **Windows**: DPAPI (`CryptProtectData`), key managed by the OS per user.
//! - **Linux**: ChaCha20-Poly1305 with a random per-user key stored at
//!   `~/.config/ClaudTray/secret.key` with `0600` permissions — same threat
//!   model as DPAPI (protects offline copies of `state.json`; a process running
//!   as the same user can read the key, just like it could call DPAPI).

#[cfg(windows)]
pub use crate::dpapi::{protect, unprotect};

#[cfg(target_os = "linux")]
pub use linux::{protect, unprotect};

/// Lowercase-hex encode (used to store encrypted blobs as JSON strings).
pub fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Decode a lowercase/uppercase hex string. Returns `None` on malformed input.
pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Some(out)
}

#[cfg(target_os = "linux")]
mod linux {
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
    use std::io::{Read, Write};
    use std::path::PathBuf;

    const NONCE_LEN: usize = 12;
    const KEY_LEN: usize = 32;

    fn urandom(buf: &mut [u8]) -> Option<()> {
        std::fs::File::open("/dev/urandom").ok()?.read_exact(buf).ok()
    }

    fn key_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("ClaudTray").join("secret.key"))
    }

    /// Load the per-user key, creating it (mode 0600) on first use. Cached for
    /// the process lifetime so every protect/unprotect in this run agrees on
    /// one key even if the file is being created concurrently. The tmp+rename
    /// dance keeps creation atomic across processes.
    fn load_or_create_key() -> Option<[u8; KEY_LEN]> {
        use std::sync::OnceLock;
        static KEY: OnceLock<Option<[u8; KEY_LEN]>> = OnceLock::new();
        *KEY.get_or_init(|| {
            let path = key_path()?;
            if let Ok(bytes) = std::fs::read(&path) {
                if bytes.len() == KEY_LEN {
                    return bytes.try_into().ok();
                }
            }
            let mut key = [0u8; KEY_LEN];
            urandom(&mut key)?;
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            use std::os::unix::fs::OpenOptionsExt;
            let tmp = path.with_extension(format!("key.tmp{}", std::process::id()));
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .ok()?;
            f.write_all(&key).ok()?;
            drop(f);
            // hard_link fails if the key already exists, so whichever process
            // gets here first wins and everyone else adopts its key.
            let _ = std::fs::hard_link(&tmp, &path);
            let _ = std::fs::remove_file(&tmp);
            let bytes = std::fs::read(&path).ok()?;
            bytes.try_into().ok()
        })
    }

    /// Encrypt `plaintext` for the current user. Returns `None` on failure.
    /// Output layout: `nonce (12 bytes) || ciphertext+tag`.
    pub fn protect(plaintext: &[u8]) -> Option<Vec<u8>> {
        let key = load_or_create_key()?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let mut nonce = [0u8; NONCE_LEN];
        urandom(&mut nonce)?;
        let ct = cipher.encrypt(Nonce::from_slice(&nonce), plaintext).ok()?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ct);
        Some(out)
    }

    /// Decrypt a blob previously produced by [`protect`]. Returns `None` on
    /// failure (wrong key, truncated blob, or not our format — used as the
    /// legacy-plaintext migration signal in `state.rs`).
    pub fn unprotect(ciphertext: &[u8]) -> Option<Vec<u8>> {
        if ciphertext.len() <= NONCE_LEN {
            return None;
        }
        let key = load_or_create_key()?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        cipher
            .decrypt(Nonce::from_slice(&ciphertext[..NONCE_LEN]), &ciphertext[NONCE_LEN..])
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let data = b"sk-or-v1-abc\x00\xff\x10";
        assert_eq!(from_hex(&to_hex(data)).as_deref(), Some(&data[..]));
        assert_eq!(from_hex("zz"), None); // non-hex
        assert_eq!(from_hex("abc"), None); // odd length
    }

    #[test]
    fn protect_round_trip() {
        let secret = "ghp_TopSecretToken_0123456789";
        let blob = protect(secret.as_bytes()).expect("protect failed");
        assert_ne!(blob, secret.as_bytes(), "ciphertext must differ from plaintext");
        let plain = unprotect(&blob).expect("unprotect failed");
        assert_eq!(plain, secret.as_bytes());
    }

    #[test]
    fn unprotect_rejects_garbage() {
        // Random non-encrypted bytes must fail cleanly (used as the migration signal).
        assert!(unprotect(b"not a real encrypted blob at all").is_none());
    }
}
