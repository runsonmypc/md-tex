//! Light and dark theme tokens, and the `system` mode that follows the OS appearance.

use gpui::{Hsla, WindowAppearance, rgb};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub background: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub link: Hsla,
    pub code_text: Hsla,
    pub code_background: Hsla,
    pub rule: Hsla,
    pub selection: Hsla,
    pub match_highlight: Hsla,
    /// The current find match.
    pub match_current: Hsla,
    pub overlay_background: Hsla,
    pub overlay_text: Hsla,
    pub overlay_button: Hsla,
    /// Raised surfaces that sit on the page: the mode switcher and dialogs.
    pub surface: Hsla,
    /// Alert accents: note, tip, important, warning, caution.
    pub alerts: [Hsla; 5],
    /// Code highlight colours, indexed by `mdt_highlight::Class`.
    pub syntax: [Hsla; 13],
}

pub fn light() -> Theme {
    Theme {
        dark: false,
        background: rgb(0xffffff).into(),
        text: rgb(0x1f2328).into(),
        muted: rgb(0x656d76).into(),
        link: rgb(0x0969da).into(),
        code_text: rgb(0x1f2328).into(),
        code_background: rgb(0xf3f4f6).into(),
        rule: rgb(0xd1d9e0).into(),
        selection: rgb(0xb4d5fe).into(),
        match_highlight: rgb(0xfff2a8).into(),
        match_current: rgb(0xffc53d).into(),
        overlay_background: rgb(0x2b2b2b).into(),
        overlay_text: rgb(0xf2f2f2).into(),
        overlay_button: rgb(0x4a4a4a).into(),
        surface: rgb(0xffffff).into(),
        alerts: [
            rgb(0x0969da).into(),
            rgb(0x1a7f37).into(),
            rgb(0x8250df).into(),
            rgb(0x9a6700).into(),
            rgb(0xcf222e).into(),
        ],
        syntax: [
            rgb(0xcf222e).into(), // keyword
            rgb(0x0a3069).into(), // string
            rgb(0x6e7781).into(), // comment
            rgb(0x0550ae).into(), // number
            rgb(0x8250df).into(), // function
            rgb(0x953800).into(), // type
            rgb(0x0550ae).into(), // constant
            rgb(0x0550ae).into(), // property
            rgb(0xcf222e).into(), // operator
            rgb(0x1f2328).into(), // punctuation
            rgb(0x1f2328).into(), // variable
            rgb(0x0550ae).into(), // attribute
            rgb(0x116329).into(), // tag
        ],
    }
}

pub fn dark() -> Theme {
    Theme {
        dark: true,
        background: rgb(0x0d1117).into(),
        text: rgb(0xe6edf3).into(),
        muted: rgb(0x8d96a0).into(),
        link: rgb(0x4493f8).into(),
        code_text: rgb(0xe6edf3).into(),
        code_background: rgb(0x1f2428).into(),
        rule: rgb(0x30363d).into(),
        selection: rgb(0x264f78).into(),
        match_highlight: rgb(0x5a4a00).into(),
        match_current: rgb(0x9e6a03).into(),
        overlay_background: rgb(0xe8e8e8).into(),
        overlay_text: rgb(0x1a1a1a).into(),
        overlay_button: rgb(0xc8c8c8).into(),
        surface: rgb(0x161b22).into(),
        alerts: [
            rgb(0x4493f8).into(),
            rgb(0x3fb950).into(),
            rgb(0xab7df8).into(),
            rgb(0xd29922).into(),
            rgb(0xf85149).into(),
        ],
        syntax: [
            rgb(0xff7b72).into(), // keyword
            rgb(0xa5d6ff).into(), // string
            rgb(0x8b949e).into(), // comment
            rgb(0x79c0ff).into(), // number
            rgb(0xd2a8ff).into(), // function
            rgb(0xffa657).into(), // type
            rgb(0x79c0ff).into(), // constant
            rgb(0x79c0ff).into(), // property
            rgb(0xff7b72).into(), // operator
            rgb(0xe6edf3).into(), // punctuation
            rgb(0xe6edf3).into(), // variable
            rgb(0x79c0ff).into(), // attribute
            rgb(0x7ee787).into(), // tag
        ],
    }
}

/// The theme for a mode, given the window's current OS appearance.
pub fn resolve(mode: ThemeMode, appearance: WindowAppearance) -> Theme {
    let dark_os = matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
    match mode {
        ThemeMode::Light => light(),
        ThemeMode::Dark => dark(),
        ThemeMode::System if dark_os => dark(),
        ThemeMode::System => light(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_follows_appearance_and_fixed_modes_do_not() {
        use WindowAppearance::*;
        assert!(!resolve(ThemeMode::System, Light).dark);
        assert!(resolve(ThemeMode::System, Dark).dark);
        assert!(resolve(ThemeMode::System, VibrantDark).dark);
        assert!(!resolve(ThemeMode::System, VibrantLight).dark);
        assert!(resolve(ThemeMode::Dark, Light).dark);
        assert!(!resolve(ThemeMode::Light, Dark).dark);
    }

    #[test]
    fn text_contrasts_with_background() {
        for t in [light(), dark()] {
            assert!((t.text.l - t.background.l).abs() > 0.7, "{t:?}");
        }
    }
}
