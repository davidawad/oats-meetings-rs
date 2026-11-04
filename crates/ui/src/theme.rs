//! The "drafting table" design tokens (docs/design/drafting-table/design.md): an ink-blue dark
//! palette and a paper light one, chosen by `ui.theme` (`system` follows the OS appearance), and
//! the bundled IBM Plex fonts. `MainWindow::render` installs the palette for the frame with `set`;
//! every view and helper reads it back with `current`, so a helper needs no context to pick a
//! colour. GPUI renders on one thread, so the palette is a thread-local.

use std::borrow::Cow;
use std::cell::Cell;

use gpui::{App, WindowAppearance};
use oats_core::config::typed::Theme as Pref;

pub const SANS: &str = "IBM Plex Sans";
pub const MONO: &str = "IBM Plex Mono";

/// The bundled Plex faces (SIL OFL, `assets/fonts/LICENSE.txt`), registered at launch so the UI
/// does not depend on installed fonts.
const FONTS: &[&[u8]] = &[
    include_bytes!("../../../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSans-Medium.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSans-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-Medium.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-SemiBold.ttf"),
];

/// Register the bundled fonts with the text system; a failure leaves the system fallback.
pub fn load_fonts(cx: &App) {
    let fonts = FONTS.iter().map(|f| Cow::Borrowed(*f)).collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        oats_core::elog!("oats-meetings-rs: bundled fonts: {e}");
    }
}

/// One palette: `0xRRGGBB` colours (for `rgb`) except `grid`, which is `0xRRGGBBAA` (`rgba`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub dark: bool,
    pub bg: u32,
    pub side: u32,
    pub surface: u32,
    pub raised: u32,
    pub line: u32,
    pub line2: u32,
    pub text: u32,
    pub dim: u32,
    pub faint: u32,
    pub accent: u32,
    pub accent_ink: u32,
    pub accent_soft: u32,
    pub rec: u32,
    pub rec_soft: u32,
    pub rec_line: u32,
    pub pause: u32,
    pub ok: u32,
    pub warn: u32,
    pub warn_soft: u32,
    pub err: u32,
    pub toggle_off: u32,
    pub you: u32,
    pub remote: [u32; 3],
    pub grid: u32,
}

impl Theme {
    pub const DARK: Theme = Theme {
        dark: true,
        bg: 0x0E1A2B,
        side: 0x0A1423,
        surface: 0x14233A,
        raised: 0x1B2E4A,
        line: 0x243A59,
        line2: 0x2F4B72,
        text: 0xE8EFF8,
        dim: 0x93A6BF,
        faint: 0x62789A,
        accent: 0x5C9DF5,
        accent_ink: 0x06142A,
        accent_soft: 0x16305A,
        rec: 0xFF5A52,
        rec_soft: 0x3A1A24,
        rec_line: 0x8A3440,
        pause: 0xF2B134,
        ok: 0x4CC38A,
        warn: 0xF2B134,
        warn_soft: 0x33290F,
        err: 0xFF7A72,
        toggle_off: 0x2F4B72,
        you: 0x78AEFF,
        remote: [0xF2B866, 0x5ED3B9, 0xC79BFF],
        grid: 0x5C9DF512,
    };

    pub const LIGHT: Theme = Theme {
        dark: false,
        bg: 0xF4F7FB,
        side: 0xE8EEF6,
        surface: 0xFFFFFF,
        raised: 0xEAF0F8,
        line: 0xD2DCE9,
        line2: 0xB8C6D9,
        text: 0x12233B,
        dim: 0x52657F,
        faint: 0x8393A9,
        accent: 0x1E4D8C,
        accent_ink: 0xFFFFFF,
        accent_soft: 0xDCE8F8,
        rec: 0xD12B2B,
        rec_soft: 0xFBE3E1,
        rec_line: 0xE7A3A0,
        pause: 0xA56A00,
        ok: 0x14804F,
        warn: 0x8A5A00,
        warn_soft: 0xFBF0D3,
        err: 0xB3261E,
        toggle_off: 0x9FB0C7,
        you: 0x1E5BB8,
        remote: [0xA15C00, 0x0B7F69, 0x6B3FB5],
        grid: 0x1E4D8C0F,
    };

    /// The palette for a `ui.theme` value and the window's appearance.
    pub fn resolve(pref: Pref, appearance: WindowAppearance) -> Theme {
        match pref {
            Pref::Dark => Theme::DARK,
            Pref::Light => Theme::LIGHT,
            Pref::System => match appearance {
                WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::LIGHT,
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::DARK,
            },
        }
    }

    pub fn name(&self) -> &'static str {
        if self.dark {
            "dark"
        } else {
            "light"
        }
    }

    /// A transcript speaker's colour: `you` fixed, `remote_N` cycling the three remote hues from
    /// `remote_1`; anything else takes the dim text colour.
    pub fn speaker(&self, speaker: &str) -> u32 {
        if speaker == "you" {
            return self.you;
        }
        match speaker
            .strip_prefix("remote_")
            .and_then(|n| n.parse::<usize>().ok())
        {
            Some(n) if n > 0 => self.remote[(n - 1) % self.remote.len()],
            _ => self.dim,
        }
    }
}

thread_local! {
    static CURRENT: Cell<Theme> = const { Cell::new(Theme::DARK) };
}

/// Make `t` the palette the views draw with (the top of each window render).
pub fn set(t: Theme) {
    CURRENT.with(|c| c.set(t));
}

/// The palette of the frame being drawn.
pub fn current() -> Theme {
    CURRENT.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speakers_get_fixed_and_cycling_hues() {
        let t = Theme::DARK;
        assert_eq!(t.speaker("you"), t.you);
        assert_eq!(t.speaker("remote_1"), t.remote[0]);
        assert_eq!(t.speaker("remote_3"), t.remote[2]);
        assert_eq!(t.speaker("remote_4"), t.remote[0], "remote_4+ cycles");
        assert_eq!(t.speaker("remote_0"), t.dim);
        assert_eq!(t.speaker("mic"), t.dim);
    }

    #[test]
    fn the_preference_picks_the_palette_and_system_follows_the_os() {
        assert_eq!(
            Theme::resolve(Pref::Dark, WindowAppearance::Light),
            Theme::DARK
        );
        assert_eq!(
            Theme::resolve(Pref::Light, WindowAppearance::Dark),
            Theme::LIGHT
        );
        assert_eq!(
            Theme::resolve(Pref::System, WindowAppearance::VibrantLight),
            Theme::LIGHT
        );
        assert_eq!(
            Theme::resolve(Pref::System, WindowAppearance::Dark),
            Theme::DARK
        );
    }

    #[test]
    fn the_bundled_fonts_are_truetype() {
        for f in FONTS {
            assert_eq!(&f[..4], &[0, 1, 0, 0], "a TrueType font");
        }
    }
}
