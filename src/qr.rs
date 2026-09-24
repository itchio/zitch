use qrcodegen::{QrCode as Encoded, QrCodeEcc};

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
    fn verification_url_is_a_small_code() {
        let url = "https://itch.io/user/oauth/device?code=41iCR8LYdZpF6cZNBzc9cM";
        assert_eq!(QrCode::encode(url).unwrap().size, 33);
    }
}
