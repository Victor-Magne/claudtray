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

// Hex encoding and the cross-platform round-trip tests live in `secret.rs`,
// which re-exports `protect`/`unprotect` from here on Windows.
