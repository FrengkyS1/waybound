//! DPAPI-backed protection for secrets persisted to `config.toml`.
//!
//! Values are stored as `dpapi:<base64 ciphertext>`. The ciphertext is bound
//! to the current Windows user via `CryptProtectData`, so the config file is
//! useless when copied to another machine or user account. On non-Windows
//! builds (untested platforms) values fall back to plaintext, matching the
//! old behavior.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

const PREFIX: &str = "dpapi:";

pub fn is_protected(stored: &str) -> bool {
    stored.starts_with(PREFIX)
}

/// Encrypt for storage. `None` when DPAPI is unavailable (non-Windows).
pub fn protect(plaintext: &str) -> Option<String> {
    dpapi::encrypt(plaintext.as_bytes()).map(|cipher| format!("{PREFIX}{}", BASE64.encode(cipher)))
}

/// Encrypt, or keep plaintext where DPAPI is unavailable.
pub fn protect_or_plain(plaintext: &str) -> String {
    protect(plaintext).unwrap_or_else(|| plaintext.to_string())
}

/// Decrypt a stored value; legacy plaintext (no prefix) passes through as-is.
pub fn reveal(stored: &str) -> Option<String> {
    match stored.strip_prefix(PREFIX) {
        None => Some(stored.to_string()),
        Some(b64) => {
            let cipher = BASE64.decode(b64).ok()?;
            let plain = dpapi::decrypt(&cipher)?;
            String::from_utf8(plain).ok()
        }
    }
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };

    pub fn encrypt(data: &[u8]) -> Option<Vec<u8>> {
        transform(data, true)
    }

    pub fn decrypt(data: &[u8]) -> Option<Vec<u8>> {
        transform(data, false)
    }

    fn transform(data: &[u8], protect: bool) -> Option<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(data.len()).ok()?,
            pbData: data.as_ptr().cast_mut(),
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        // SAFETY: `input` borrows `data` for the duration of the call only;
        // `output` is API-allocated and freed with LocalFree below.
        let ok = unsafe {
            if protect {
                CryptProtectData(
                    &input,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    0,
                    &mut output,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    0,
                    &mut output,
                )
            }
        };
        if ok == 0 || output.pbData.is_null() {
            return None;
        }

        let result =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
        unsafe { LocalFree(output.pbData.cast()) };
        Some(result)
    }
}

#[cfg(not(windows))]
mod dpapi {
    // ponytail: non-Windows is unsupported today; secrets stay plaintext there.
    pub fn encrypt(_data: &[u8]) -> Option<Vec<u8>> {
        None
    }
    pub fn decrypt(_data: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::{is_protected, protect, reveal};

    #[test]
    fn roundtrip_recovers_secret() {
        let secret = "$2a$10$not-a-real-key-just-a-test";
        let stored = protect(secret).expect("DPAPI is available on Windows");
        assert!(is_protected(&stored));
        assert_ne!(stored, secret);
        assert_eq!(reveal(&stored).as_deref(), Some(secret));
    }

    #[test]
    fn legacy_plaintext_passes_through() {
        assert!(!is_protected("legacy-plain"));
        assert_eq!(reveal("legacy-plain").as_deref(), Some("legacy-plain"));
    }
}
