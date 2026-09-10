//! Button glyphs for the hint footer, from Kenney's CC0 input prompts
//! (assets/prompts). Decoded once at startup into a handful of textures.

use std::collections::HashMap;

use egui::TextureHandle;

/// Which device the hints should speak to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputMode {
    Keyboard,
    Gamepad,
    /// No hints: touch has no buttons to name.
    Touch,
}

/// An action as the footer names it, independent of device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Glyph {
    Confirm,
    Back,
    Navigate,
    NavigateHorizontal,
    TabLeft,
    TabRight,
    FilterLeft,
    FilterRight,
    Search,
    /// Start on a pad, Escape on a keyboard.
    Menu,
}

pub struct Glyphs {
    textures: HashMap<(InputMode, Glyph), TextureHandle>,
    logo: Option<TextureHandle>,
}

const LOGO: &[u8] = include_bytes!("../assets/itch-logo.png");

fn decode(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<TextureHandle> {
    let decoded = match image::load_from_memory(bytes) {
        Ok(image) => image.into_rgba8(),
        Err(error) => {
            log::error!("{name}: {error}");
            return None;
        }
    };
    let size = [decoded.width() as usize, decoded.height() as usize];
    let image = egui::ColorImage::from_rgba_unmultiplied(size, decoded.as_raw());
    Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR))
}

macro_rules! glyph_files {
    ($($mode:ident, $glyph:ident => $file:literal),* $(,)?) => {
        &[$((InputMode::$mode, Glyph::$glyph, $file, include_bytes!(concat!("../assets/prompts/", $file)) as &[u8])),*]
    };
}

const FILES: &[(InputMode, Glyph, &str, &[u8])] = glyph_files![
    Gamepad, Confirm => "xbox_button_color_a.png",
    Gamepad, Back => "xbox_button_color_b.png",
    Gamepad, Navigate => "xbox_dpad_all.png",
    Gamepad, NavigateHorizontal => "xbox_dpad_horizontal.png",
    Gamepad, TabLeft => "xbox_lb.png",
    Gamepad, TabRight => "xbox_rb.png",
    Gamepad, FilterLeft => "xbox_lt.png",
    Gamepad, FilterRight => "xbox_rt.png",
    Gamepad, Search => "xbox_button_color_y.png",
    Gamepad, Menu => "xbox_button_menu.png",
    Keyboard, Confirm => "keyboard_enter.png",
    Keyboard, Back => "keyboard_escape.png",
    Keyboard, Navigate => "keyboard_arrows_all.png",
    Keyboard, NavigateHorizontal => "keyboard_arrows_horizontal.png",
    Keyboard, TabLeft => "keyboard_q.png",
    Keyboard, TabRight => "keyboard_e.png",
    Keyboard, FilterRight => "keyboard_tab.png",
    Keyboard, Search => "keyboard_slash_forward.png",
    Keyboard, Menu => "keyboard_escape.png",
];

impl Glyphs {
    pub fn load(ctx: &egui::Context) -> Self {
        let mut textures = HashMap::new();
        for (mode, glyph, name, bytes) in FILES {
            if let Some(texture) = decode(ctx, &format!("glyph/{name}"), bytes) {
                textures.insert((*mode, *glyph), texture);
            }
        }
        Self {
            textures,
            logo: decode(ctx, "logo", LOGO),
        }
    }

    pub fn logo(&self) -> Option<&TextureHandle> {
        self.logo.as_ref()
    }

    pub fn get(&self, mode: InputMode, glyph: Glyph) -> Option<&TextureHandle> {
        self.textures.get(&(mode, glyph))
    }
}
