//! Signing in from a device with no browser: the OAuth device grant
//! (RFC 8628) with PKCE on top, as `itchio-site/zitch-device-auth.md`
//! lays it out. The server mints a device code to poll with and a short
//! user code for the phone; the verifier stays here, so the code the
//! poll hands back is no use to anyone who only saw the screen.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use qrcodegen::{QrCode as Encoded, QrCodeEcc};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const CLIENT_ID: &str = "d59031fc193811bb3e6af0f8eeeb2c18";
/// Sent only at the exchange; the server holds the code for the poll.
pub const REDIRECT_URI: &str = "urn:itchio:poll";
const SCOPE: &str = "itch";
const TIMEOUT: Duration = Duration::from_secs(20);

/// One sign-in request. Never log the verifier or the device code.
pub struct DeviceLogin {
    pub verifier: String,
    device_code: String,
    pub user_code: String,
    /// What the QR code holds.
    pub verification_url: String,
    pub expires_in: Duration,
    pub interval: Duration,
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct PollResponse {
    status: String,
    code: Option<String>,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct Errors {
    errors: Vec<String>,
}

pub enum Poll {
    Pending {
        interval: Duration,
    },
    Approved {
        code: String,
    },
    Denied,
    Expired,
    /// Too many polls from this address; wait longer.
    SlowDown,
}

impl DeviceLogin {
    pub fn start(api_url: &str) -> Result<DeviceLogin> {
        let verifier = base64url(&random(32));
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        let (status, body) = post(
            &format!("{api_url}/oauth/device"),
            &[
                ("client_id", CLIENT_ID),
                ("scope", SCOPE),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
            ],
        )?;
        if status != 200 {
            bail!("starting sign-in: {}", describe(status, &body));
        }
        let response: DeviceResponse = serde_json::from_str(&body).context("sign-in response")?;
        Ok(DeviceLogin {
            verifier,
            device_code: response.device_code,
            user_code: response.user_code,
            verification_url: response.verification_uri_complete,
            expires_in: Duration::from_secs(response.expires_in),
            interval: Duration::from_secs(response.interval.max(1)),
        })
    }

    pub fn poll(&self, api_url: &str) -> Result<Poll> {
        let (status, body) = post(
            &format!("{api_url}/oauth/device/poll"),
            &[("client_id", CLIENT_ID), ("device_code", &self.device_code)],
        )?;
        match status {
            200 => {}
            429 => return Ok(Poll::SlowDown),
            _ => bail!("polling sign-in: {}", describe(status, &body)),
        }
        let response: PollResponse = serde_json::from_str(&body).context("poll response")?;
        Ok(match response.status.as_str() {
            "pending" => Poll::Pending {
                interval: Duration::from_secs(response.interval.unwrap_or(5).max(1)),
            },
            "approved" => Poll::Approved {
                code: response
                    .code
                    .ok_or_else(|| anyhow!("approved without a code"))?,
            },
            "denied" => Poll::Denied,
            "expired" => Poll::Expired,
            other => bail!("unknown sign-in status {other:?}"),
        })
    }
}

/// A form post; error statuses come back as values so the body can be read.
fn post(url: &str, form: &[(&str, &str)]) -> Result<(u16, String)> {
    let mut response = ureq::post(url)
        .config()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .build()
        .send_form(form.iter().copied())
        .with_context(|| format!("POST {url}"))?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .with_context(|| format!("reading {url}"))?;
    Ok((status, body))
}

fn describe(status: u16, body: &str) -> String {
    match serde_json::from_str::<Errors>(body) {
        Ok(errors) if !errors.errors.is_empty() => errors.errors.join(", "),
        _ => format!("HTTP {status}"),
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
    fn challenge_matches_rfc_7636() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            base64url(&Sha256::digest(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verification_url_is_a_small_code() {
        let url = "https://itch.io/user/oauth/device?code=41iCR8LYdZpF6cZNBzc9cM";
        assert_eq!(QrCode::encode(url).unwrap().size, 33);
    }
}
