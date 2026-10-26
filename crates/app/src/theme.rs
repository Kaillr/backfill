//! Colour tokens. The calendar stays quiet graphite so that issue colours and the amber
//! "unlogged" hatching are the only loud things on screen.

use std::{cell::RefCell, collections::HashMap};

use backfill_core::IssueKey;
use gpui::{Hsla, Window, WindowAppearance, hsla};

#[derive(Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    /// Window background.
    pub canvas: Hsla,
    /// Day columns and panels.
    pub surface: Hsla,
    /// Things that sit on a surface: activity blocks, popovers.
    pub raised: Hsla,
    pub line: Hsla,
    pub line_strong: Hsla,
    pub ink: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    /// Unlogged time and the "now" marker: the things that need you.
    pub amber: Hsla,
    pub amber_ink: Hsla,
    pub focus: Hsla,
    pub danger: Hsla,
    pub ok: Hsla,
    pub scrim: Hsla,
}

impl Theme {
    /// Follows the system appearance unless `BACKFILL_THEME` is `light` or `dark`.
    pub fn for_window(window: &Window) -> Self {
        match std::env::var("BACKFILL_THEME").as_deref() {
            Ok("light") => return Self::light(),
            Ok("dark") => return Self::dark(),
            _ => {}
        }
        match window.appearance() {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            _ => Self::light(),
        }
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            canvas: hsla(0.61, 0.16, 0.10, 1.),
            surface: hsla(0.61, 0.14, 0.135, 1.),
            raised: hsla(0.61, 0.13, 0.18, 1.),
            line: hsla(0.61, 0.12, 0.21, 1.),
            line_strong: hsla(0.61, 0.10, 0.30, 1.),
            ink: hsla(0.61, 0.20, 0.92, 1.),
            muted: hsla(0.61, 0.10, 0.62, 1.),
            faint: hsla(0.61, 0.08, 0.42, 1.),
            amber: hsla(0.105, 0.90, 0.58, 1.),
            amber_ink: hsla(0.105, 0.95, 0.70, 1.),
            focus: hsla(0.61, 0.85, 0.72, 1.),
            danger: hsla(0.99, 0.75, 0.64, 1.),
            ok: hsla(0.40, 0.55, 0.55, 1.),
            scrim: hsla(0.61, 0.30, 0.04, 0.55),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            canvas: hsla(0.60, 0.20, 0.94, 1.),
            surface: hsla(0., 0., 1., 1.),
            raised: hsla(0.60, 0.25, 0.97, 1.),
            line: hsla(0.60, 0.18, 0.89, 1.),
            line_strong: hsla(0.60, 0.14, 0.78, 1.),
            ink: hsla(0.62, 0.30, 0.16, 1.),
            muted: hsla(0.62, 0.12, 0.42, 1.),
            faint: hsla(0.62, 0.10, 0.62, 1.),
            amber: hsla(0.10, 0.92, 0.50, 1.),
            amber_ink: hsla(0.09, 0.85, 0.36, 1.),
            focus: hsla(0.62, 0.80, 0.52, 1.),
            danger: hsla(0.99, 0.70, 0.50, 1.),
            ok: hsla(0.40, 0.55, 0.38, 1.),
            scrim: hsla(0.62, 0.30, 0.20, 0.25),
        }
    }

    /// The fill colour for an issue. Issues take colour slots in the order they are first
    /// seen, so the issues on screen together stay easy to tell apart. The amber band is
    /// left out: it is reserved for unlogged time.
    pub fn issue(&self, key: &IssueKey) -> Hsla {
        let hue = issue_hue(key);
        if self.dark {
            hsla(hue, 0.58, 0.66, 1.)
        } else {
            hsla(hue, 0.62, 0.70, 1.)
        }
    }

    /// Text that sits on [`Theme::issue`]: the same hue, very deep.
    pub fn issue_ink(&self, key: &IssueKey) -> Hsla {
        hsla(issue_hue(key), 0.55, 0.13, 1.)
    }
}

pub fn with_alpha(mut color: Hsla, alpha: f32) -> Hsla {
    color.alpha = alpha;
    color
}

/// Gives each issue the next free colour slot. Call it with the known issues at startup
/// so colours don't depend on which week is opened first.
pub fn assign_issue_colors<'a>(keys: impl IntoIterator<Item = &'a IssueKey>) {
    for key in keys {
        issue_hue(key);
    }
}

thread_local! {
    static SLOTS: RefCell<HashMap<IssueKey, usize>> = RefCell::new(HashMap::new());
}

fn issue_hue(key: &IssueKey) -> f32 {
    // Neighbouring slots sit far apart on the wheel, so issues that arrive together differ.
    const HUES: [f32; 12] = [
        0.58, 0.36, 0.80, 0.50, 0.95, 0.70, 0.43, 0.88, 0.63, 0.29, 0.00, 0.53,
    ];
    SLOTS.with_borrow_mut(|slots| {
        let next = slots.len();
        let slot = *slots.entry(key.clone()).or_insert(next);
        HUES[slot % HUES.len()]
    })
}
