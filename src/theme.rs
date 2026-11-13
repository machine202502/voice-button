//! The window's two themes and the colours they paint with.
//!
//! The values come from the product's own stylesheets (`src/web/css/styles.css`
//! and `site/styles.css`): the dark theme is the window's original look, the
//! light theme is the same design on white-green surfaces. Everything the
//! interface draws — egui widgets, the QR code, the About microphone — reads
//! its colour from a [`Palette`], so a theme switch has no leftovers.
//!
//! `accent` is a fill/icon colour (a lime in the dark theme, a mid green in the
//! light one); it is deliberately too light for text. Text that should look
//! like the accent — links, emphasised labels — uses `accent_ink`, which is the
//! lime on dark surfaces and a darker green on light ones.

use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};

/// Which colour theme the window is painted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    /// The original dark green window.
    Dark,
    /// The same design on white-green surfaces.
    Light,
    /// Follow the Windows light/dark setting. The default for a fresh config.
    #[default]
    System,
}

impl Theme {
    /// Name shown in Settings. The caller translates it.
    pub fn label(self) -> &'static str {
        match self {
            Theme::Dark => "Dark",
            Theme::Light => "Light",
            Theme::System => "System",
        }
    }
}

/// Resolve a setting to the theme that is actually painted. egui only ever sees
/// a concrete [`Theme::Dark`] or [`Theme::Light`]; `System` is answered here.
pub fn resolve(theme: Theme) -> Theme {
    match theme {
        Theme::Dark | Theme::Light => theme,
        Theme::System => {
            // Snapshots are committed files and may not depend on the machine's
            // Windows setting: in snapshot mode System means Dark.
            if std::env::var("VB_SNAPSHOT").is_ok() {
                Theme::Dark
            } else {
                windows_theme()
            }
        }
    }
}

/// Windows' own light/dark choice. A missing key or an unreadable value keeps
/// the product's original dark look; that fallback is deliberate.
fn windows_theme() -> Theme {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let personalize = RegKey::predef(HKEY_CURRENT_USER).open_subkey(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
    );
    let Ok(personalize) = personalize else {
        return Theme::Dark;
    };
    match personalize.get_value::<u32, _>("AppsUseLightTheme") {
        Ok(1) => Theme::Light,
        // 0 is dark; anything else is not a light request.
        _ => Theme::Dark,
    }
}

/// Named semantic colours of one theme. Adding a colour here instead of a
/// literal at the call site is what lets the light theme adjust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// The window background.
    pub bg: Color32,
    /// Raised surfaces: windows, cards, popups.
    pub panel: Color32,
    /// Controls and inputs sitting on a panel.
    pub panel_alt: Color32,
    /// Body text.
    pub ink: Color32,
    /// Secondary text: hints, placeholders, disabled-ish labels.
    pub muted: Color32,
    /// Hairlines and separators. Translucent.
    pub line: Color32,
    /// Fills and icons: selection, progress, focus rings. Not for text.
    pub accent: Color32,
    /// Text in the accent hue, readable on this theme's surfaces.
    pub accent_ink: Color32,
    /// Warnings and work in progress.
    pub amber: Color32,
    /// The phone.
    pub violet: Color32,
    /// Errors and the recording state.
    pub red: Color32,
    /// Finished work and "ready" states: the file status.
    pub success: Color32,
    /// Informational states: the Telegram source, incomplete entries.
    pub info: Color32,
    /// Selected fills (selected tab, progress bar, text selection).
    pub selection: Color32,
    /// Text drawn on `selection`.
    pub selection_ink: Color32,
    /// Dim behind the About modal.
    pub overlay: Color32,
    /// Quiet zone of the QR code and its modules.
    pub qr_bg: Color32,
    pub qr_fg: Color32,
}

/// The palette of `theme`. `System` is resolved to a concrete theme first.
pub fn palette(theme: Theme) -> Palette {
    match resolve(theme) {
        Theme::Light => Palette {
            bg: rgb(0xf4f7ef),
            panel: rgb(0xffffff),
            panel_alt: rgb(0xeaf0e2),
            ink: rgb(0x17200f),
            muted: rgb(0x5d6b57),
            line: Color32::from_rgba_unmultiplied(23, 32, 15, 31),
            accent: rgb(0x6a9422),
            accent_ink: rgb(0x4f7a12),
            amber: rgb(0x956310),
            violet: rgb(0x6b53a8),
            red: rgb(0xb3403f),
            success: rgb(0x337a40),
            info: rgb(0x2f6fb0),
            selection: rgb(0x6a9422),
            selection_ink: rgb(0x17200f),
            overlay: Color32::from_black_alpha(90),
            qr_bg: rgb(0xffffff),
            qr_fg: rgb(0x17200f),
        },
        // `resolve` answers `System`, so only Dark is left; it is the product's
        // original look and the fallback for anything unexpected.
        _ => Palette {
            bg: rgb(0x111713),
            panel: rgb(0x182019),
            panel_alt: rgb(0x1d271e),
            ink: rgb(0xf0f4e9),
            muted: rgb(0x8f9c8b),
            line: Color32::from_rgba_unmultiplied(224, 239, 211, 31),
            accent: rgb(0xc7f36b),
            accent_ink: rgb(0xc7f36b),
            amber: rgb(0xffbd62),
            violet: rgb(0xb9a2f0),
            red: rgb(0xff8f8f),
            success: rgb(0x5ac878),
            info: rgb(0x5aaadc),
            selection: rgb(0xc7f36b),
            selection_ink: rgb(0x141c0a),
            overlay: Color32::from_black_alpha(140),
            qr_bg: Color32::WHITE,
            qr_fg: rgb(0x141814),
        },
    }
}

/// The theme the context is currently painted with. `apply` keeps the visuals
/// in sync, so this is how a page finds its palette without a parameter.
pub fn active(ctx: &egui::Context) -> Theme {
    if ctx.style().visuals.dark_mode {
        Theme::Dark
    } else {
        Theme::Light
    }
}

/// Paint `theme` into every part of egui's default style: backgrounds, text,
/// selection, hyperlinks, all widget states and the rounding/spacing the app
/// uses. Called at startup, when Settings changes the theme, and by the
/// snapshot walk when it switches to a light frame.
pub fn apply(ctx: &egui::Context, theme: Theme) {
    debug_assert!(
        theme != Theme::System,
        "resolve the System setting before applying a theme"
    );
    let theme = resolve(theme);
    let p = palette(theme);
    let dark = theme == Theme::Dark;
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let rounding = egui::Rounding::same(12.0);
    // Interaction shades are mixed from the palette, not written as literals.
    let hover_fill = mix(p.panel_alt, p.accent, 0.20);
    let active_fill = mix(p.panel_alt, p.accent, 0.35);
    let shadow_alpha = if dark { 120 } else { 40 };

    visuals.dark_mode = dark;
    // No global override: widgets without an explicit colour take the colour of
    // their state (a selected label takes `selection.stroke`, so text on the
    // lime fill stays dark). Plain labels read `noninteractive.fg_stroke`.
    visuals.override_text_color = None;
    visuals.selection = egui::style::Selection {
        bg_fill: p.selection,
        stroke: egui::Stroke::new(1.0_f32, p.selection_ink),
    };
    visuals.hyperlink_color = p.accent_ink;
    visuals.faint_bg_color = p.panel_alt;
    // Text edits, scrollbar troughs and progress tracks.
    visuals.extreme_bg_color = if dark { rgb(0x0d120f) } else { p.panel };
    visuals.code_bg_color = p.panel_alt;
    visuals.warn_fg_color = p.amber;
    visuals.error_fg_color = p.red;
    visuals.window_rounding = egui::Rounding::same(16.0);
    visuals.menu_rounding = rounding;
    visuals.window_fill = p.panel;
    visuals.window_stroke = egui::Stroke::new(1.0_f32, p.line);
    visuals.panel_fill = p.bg;
    visuals.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(shadow_alpha),
    };
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 16.0,
        spread: 0.0,
        color: Color32::from_black_alpha(shadow_alpha),
    };
    visuals.text_cursor = egui::Stroke::new(2.0_f32, p.accent_ink);
    visuals.resize_corner_size = 12.0;
    visuals.striped = false;
    visuals.slider_trailing_fill = true;

    visuals.widgets = egui::style::Widgets {
        // `weak_bg_fill` is where egui fades weak text towards; pointing it at
        // the palette's muted tone keeps hints subdued in both themes.
        noninteractive: egui::style::WidgetVisuals {
            bg_fill: p.bg,
            weak_bg_fill: p.muted,
            bg_stroke: egui::Stroke::new(1.0_f32, p.line),
            fg_stroke: egui::Stroke::new(1.0_f32, p.ink),
            rounding,
            expansion: 0.0,
        },
        inactive: egui::style::WidgetVisuals {
            bg_fill: p.panel_alt,
            weak_bg_fill: p.panel_alt,
            bg_stroke: egui::Stroke::new(1.0_f32, p.line),
            fg_stroke: egui::Stroke::new(1.0_f32, p.ink),
            rounding,
            expansion: 0.0,
        },
        hovered: egui::style::WidgetVisuals {
            bg_fill: hover_fill,
            weak_bg_fill: hover_fill,
            bg_stroke: egui::Stroke::new(1.0_f32, p.accent),
            fg_stroke: egui::Stroke::new(1.5_f32, p.ink),
            rounding,
            expansion: 1.0,
        },
        active: egui::style::WidgetVisuals {
            bg_fill: active_fill,
            weak_bg_fill: active_fill,
            bg_stroke: egui::Stroke::new(1.0_f32, p.accent),
            fg_stroke: egui::Stroke::new(1.5_f32, p.ink),
            rounding,
            expansion: 0.0,
        },
        open: egui::style::WidgetVisuals {
            bg_fill: p.panel_alt,
            weak_bg_fill: p.panel_alt,
            bg_stroke: egui::Stroke::new(1.0_f32, p.accent),
            fg_stroke: egui::Stroke::new(1.0_f32, p.ink),
            rounding,
            expansion: 0.0,
        },
    };

    let mut style = (*ctx.style()).clone();
    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.window_margin = egui::Margin::same(12.0);
    style.spacing.menu_margin = egui::Margin::same(8.0);
    ctx.set_style(style);
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb(
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    )
}

/// Linear mix of two opaque colours; `t == 0.0` is `a`, `1.0` is `b`.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let channel = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: Color32) -> f32 {
        let channel = |value: u8| {
            let value = value as f32 / 255.0;
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// The palette promises readable text and distinguishable accents in both
    /// themes. A colour tweak that breaks it fails here instead of shipping.
    #[test]
    fn both_palettes_keep_their_contrast_promises() {
        for theme in [Theme::Dark, Theme::Light] {
            let p = palette(theme);
            let promises = [
                ("ink on bg", p.ink, p.bg, 4.5),
                ("ink on panel", p.ink, p.panel, 4.5),
                ("muted on panel", p.muted, p.panel, 4.5),
                ("accent_ink on bg", p.accent_ink, p.bg, 4.5),
                ("accent_ink on panel", p.accent_ink, p.panel, 4.5),
                ("accent on panel", p.accent, p.panel, 3.0),
                ("amber on bg", p.amber, p.bg, 4.5),
                ("success on bg", p.success, p.bg, 4.5),
                ("violet on bg", p.violet, p.bg, 4.5),
                ("red on bg", p.red, p.bg, 4.5),
                ("info on bg", p.info, p.bg, 4.5),
                ("text on the accent fill", p.selection_ink, p.selection, 4.5),
            ];
            for (what, foreground, background, minimum) in promises {
                let ratio = contrast(foreground, background);
                assert!(
                    ratio >= minimum,
                    "{theme:?}: {what} is {ratio:.2}:1, needs {minimum:.1}:1"
                );
            }
        }
    }
}
