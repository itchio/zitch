//! Signing in with a QR code. The code holds an OAuth authorize URL, the
//! same one the itch app opens in a browser, for a phone to open instead.
//! The PKCE verifier stays here, so the code the server issues is no use
//! to anyone who only saw the screen.

use qrcodegen::{QrCode as Encoded, QrCodeEcc};
use sha2::{Digest, Sha256};

/// The itch app's client, until zitch has one of its own.
pub const CLIENT_ID: &str = "85252daf268d27fbefac93e1ac462bfd";
/// Asks the server to hold the code for polling instead of redirecting.
pub const REDIRECT_URI: &str = "urn:itchio:poll";
const SCOPE: &str = "itch";

/// One sign-in attempt. Never log the verifier.
pub struct Login {
    pub url: String,
    pub state: String,
    /// For the code exchange once the poll finds the approval.
    #[allow(dead_code)]
    pub verifier: String,
}

impl Login {
    pub fn generate(web_url: &str) -> Login {
        let state = base64url(&random(16));
        let verifier = base64url(&random(32));
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        let url = format!(
            "{web_url}/user/oauth?client_id={CLIENT_ID}&scope={SCOPE}&redirect_uri={REDIRECT_URI}\
             &state={state}&response_type=code&code_challenge={challenge}&code_challenge_method=S256"
        );
        Login {
            url,
            state,
            verifier,
        }
    }
}

fn random(len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    getrandom::fill(&mut bytes).expect("system randomness");
    bytes
}

/// Unpadded, like the itch app's `base64url`.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

pub struct QrCode {
    pub size: usize,
    dark: Vec<bool>,
}

impl QrCode {
    pub fn encode(text: &str) -> Option<QrCode> {
        // A screen does not get scuffed, so the lowest correction level:
        // fewer modules, and each one bigger on a small display.
        let code = Encoded::encode_text(text, QrCodeEcc::Low).ok()?;
        let size = code.size() as usize;
        let dark = (0..size)
            .flat_map(|y| (0..size).map(move |x| (x, y)))
            .map(|(x, y)| code.get_module(x as i32, y as i32))
            .collect();
        Some(QrCode { size, dark })
    }

    pub fn dark(&self, x: usize, y: usize) -> bool {
        self.dark[y * self.size + x]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_matches_rfc() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn login_url_fits_a_small_code() {
        let login = Login::generate("https://itch.io");
        assert_eq!(login.state.len(), 22);
        assert_eq!(login.verifier.len(), 43);
        // 243 bytes: version 10 at low correction. Every parameter the
        // server can default away takes the code down a version.
        assert_eq!(login.url.len(), 243);
        let qr = QrCode::encode(&login.url).unwrap();
        assert_eq!(qr.size, 57);
    }
}
