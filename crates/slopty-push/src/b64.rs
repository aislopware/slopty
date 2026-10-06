//! Base64 as the push path writes it: URL-safe, unpadded, as a JWT's parts are.

use data_encoding::BASE64URL_NOPAD;

/// `bytes`, written.
pub fn write(bytes: &[u8]) -> String {
    BASE64URL_NOPAD.encode(bytes)
}

/// `text`, read; `None` when it is not base64 as [`write()`] writes it.
pub fn read(text: &str) -> Option<Vec<u8>> {
    BASE64URL_NOPAD.decode(text.as_bytes()).ok()
}
