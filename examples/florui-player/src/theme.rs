//! Three skins, swapped live by adding one of these class names to the
//! app root -- real CSS custom properties (`--bg`/`--panel`/`--accent`/
//! `--text`), resolved through the real cascade, not an inline-style
//! hack. See `app.css`'s own `.skin-*` rules for the actual values.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Skin {
    Terminal,
    Amber,
    Light,
}

impl Skin {
    pub fn class(self) -> &'static str {
        match self {
            Skin::Terminal => "skin-terminal",
            Skin::Amber => "skin-amber",
            Skin::Light => "skin-light",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Skin::Terminal => "Terminal",
            Skin::Amber => "Amber",
            Skin::Light => "Light",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Skin::Terminal => Skin::Amber,
            Skin::Amber => Skin::Light,
            Skin::Light => Skin::Terminal,
        }
    }
}
