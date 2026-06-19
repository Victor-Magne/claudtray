//! Thin wrapper over the Windows Data Protection API (DPAPI).
//!
//! `protect`/`unprotect` encrypt small secrets (API tokens, proxy URLs) tied to
//! the current Windows user. The ciphertext can only be decrypted by the same
//! user on the same machine, so the on-disk `state.json` no longer exposes
//! credentials in plaintext to other processes or offline copies of the file.

use winapi::shared::minwindef::DWORD;
use winapi::um::dpapi::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN};
use winapi::um::wincrypt::DATA_BLOB;
use winapi::um::winbase::LocalFree;

fn blob(data: &[u8]) -> DATA_BLOB {
    DATA_BLOB {
        cbData: data.len() as DWORD,
        // CryptProtectData/Unprotect treat the input buffer as read-only.
        pbData: data.as_ptr() as *mut u8,
    }
}

/// Read out an output blob into a `Vec` and free the OS-allocated buffer.
unsafe fn take_blob(out: &DATA_BLOB) -> Vec<u8> {
    let slice = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
    let vec = slice.to_vec();
    LocalFree(out.pbData as *mut winapi::ctypes::c_void);
    vec
}

/// Encrypt `plaintext` for the current user. Returns `None` if DPAPI fails.
pub fn protect(plaintext: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let mut in_blob = blob(plaintext);
        let mut out_blob: DATA_BLOB = std::mem::zeroed();
        let ok = CryptProtectData(
            &mut in_blob,
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        );
        if ok == 0 || out_blob.pbData.is_null() {
            return None;
        }
        Some(take_blob(&out_blob))
    }
}

/// Decrypt a blob previously produced by [`protect`]. Returns `None` on failure
/// (e.g. the file was copied from another user/machine, or it is not DPAPI data).
pub fn unprotect(ciphertext: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let mut in_blob = blob(ciphertext);
        let mut out_blob: DATA_BLOB = std::mem::zeroed();
        let ok = CryptUnprotectData(
            &mut in_blob,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        );
        if ok == 0 || out_blob.pbData.is_null() {
            return None;
        }
        Some(take_blob(&out_blob))
    }
}

/// Lowercase-hex encode (used to store DPAPI blobs as JSON strings).
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
    fn dpapi_round_trip() {
        let secret = "ghp_TopSecretToken_0123456789";
        let blob = protect(secret.as_bytes()).expect("protect failed");
        assert_ne!(blob, secret.as_bytes(), "ciphertext must differ from plaintext");
        let plain = unprotect(&blob).expect("unprotect failed");
        assert_eq!(plain, secret.as_bytes());
    }

    #[test]
    fn unprotect_rejects_garbage() {
        // Random non-DPAPI bytes must fail cleanly (used as the migration signal).
        assert!(unprotect(b"not a real dpapi blob at all").is_none());
    }
}
