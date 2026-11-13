//! Tray and window icons.
//!
//! The artwork lives in SVG files that hold the microphone alone, with its
//! colour left as `currentColor`. Two things are built from those files:
//!
//! - the tray icon, which needs a tile behind the microphone and a real colour;
//! - the web page, which inlines the same file and lets the record button's
//!   colour through, so the icon follows the recording state by itself.
//!
//! `resvg` rasterizes the composed SVG at the exact size Windows asks for
//! instead of downscaling a stored bitmap, which is what kept the small tray
//! sizes soft.
use std::sync::OnceLock;
use tray_icon::Icon;

const ICON_SVG: &str = include_str!("../assets/icon.svg");

/// Size Windows uses for notification-area icons at 100–200 % scaling.
const TRAY_SIDE: u32 = 32;
/// Size of the window icon, matching the shell's larger surfaces.
const WINDOW_SIDE: u32 = 256;

/// The tile behind the microphone, in the `viewBox` units of the icon.
const TILE_RADIUS: f32 = 112.0 / 512.0 * 24.0;
/// The microphone fills 90 % of the tile: any smaller left it looking like a
/// small mark in a large square.
const INSET: f32 = 0.9;
const SPAN: f32 = 24.0 * INSET;
/// What the tile's colour is mixed over: the dark surface the mockup uses.
const BACKGROUND: [u8; 3] = [0x11, 0x17, 0x13];
/// How much of the holiday colour the tile carries.
const TILE_ALPHA: f32 = 0.13;

/// The accent of every holiday, in the order the mockup lists them. The same
/// value tints the tile and is the `color` the microphone inherits.
const HOLIDAYS: [(&str, [u8; 3], &str); 17] = [
    ("newyear", [0x7f, 0xd4, 0xff], include_str!("../assets/holidays/newyear.svg")),
    ("cny", [0xff, 0x6b, 0x57], include_str!("../assets/holidays/cny.svg")),
    ("love", [0xff, 0x7f, 0xa8], include_str!("../assets/holidays/love.svg")),
    ("defender", [0x8f, 0xb8, 0xff], include_str!("../assets/holidays/defender.svg")),
    ("women", [0xff, 0x9e, 0xc7], include_str!("../assets/holidays/women.svg")),
    ("maslenitsa", [0xff, 0xc8, 0x5c], include_str!("../assets/holidays/maslenitsa.svg")),
    ("friday", [0xb7, 0xa6, 0xff], include_str!("../assets/holidays/friday.svg")),
    ("clover", [0x6b, 0xe3, 0x8a], include_str!("../assets/holidays/clover.svg")),
    ("easter", [0xff, 0xd3, 0x6e], include_str!("../assets/holidays/easter.svg")),
    ("space", [0x78, 0xb7, 0xff], include_str!("../assets/holidays/space.svg")),
    ("victory", [0xff, 0xb2, 0x5e], include_str!("../assets/holidays/victory.svg")),
    ("halloween", [0xff, 0x9b, 0x4a], include_str!("../assets/holidays/halloween.svg")),
    ("diwali", [0xff, 0xc8, 0x5c], include_str!("../assets/holidays/diwali.svg")),
    ("dead", [0xff, 0x77, 0xb0], include_str!("../assets/holidays/dead.svg")),
    ("christmas", [0x8f, 0xe3, 0xb0], include_str!("../assets/holidays/christmas.svg")),
    ("hanukkah", [0x79, 0xc7, 0xff], include_str!("../assets/holidays/hanukkah.svg")),
    ("eid", [0xa7, 0xe8, 0xc8], include_str!("../assets/holidays/eid.svg")),
];

/// The plain microphone is the application's own accent.
const BASE_ACCENT: [u8; 3] = [0xc7, 0xf3, 0x6b];

fn hex(color: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

fn mix(color: [u8; 3], over: [u8; 3], alpha: f32) -> [u8; 3] {
    let mut out = [0u8; 3];
    for i in 0..3 {
        out[i] = (color[i] as f32 * alpha + over[i] as f32 * (1.0 - alpha)).round() as u8;
    }
    out
}

/// The drawing inside an SVG file: everything between the root tag and its end.
fn inner(svg: &str) -> &str {
    let start = svg.find('>').map(|i| i + 1).unwrap_or(0);
    let end = svg.rfind("</svg>").unwrap_or(svg.len());
    svg[start..end].trim()
}

/// The file's microphone on the tile, coloured `accent`: what the tray draws.
fn tiled(body: &str, accent: [u8; 3]) -> String {
    let tile = hex(mix(accent, BACKGROUND, TILE_ALPHA));
    let offset = (24.0 - SPAN) / 2.0;
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24" color="{accent}"><rect width="24" height="24" rx="{radius:.3}" fill="{tile}"/><g transform="translate({offset:.3} {offset:.3}) scale({INSET})">{inner}</g></svg>"#,
        accent = hex(accent),
        radius = TILE_RADIUS,
        offset = offset,
        inner = inner(body).replace("currentColor", &hex(accent)),
    )
}

/// The plain microphone on its tile: the application's own icon.
pub fn base_tiled_svg() -> String {
    tiled(ICON_SVG, BASE_ACCENT)
}

/// One holiday microphone on its tile, or `None` for an unknown id.
pub fn holiday_tiled_svg(id: &str) -> Option<String> {
    holiday(id).map(|(_, accent, body)| tiled(body, accent))
}

/// Repaint every stroke and fill of a fragment with one colour.
///
/// `fill="none"` survives: it is the absence of paint, not a colour.
fn paint_all(fragment: &str, paint: &str) -> String {
    let mut drawn = fragment.replace("currentColor", paint);
    for prefix in ["stroke", "fill"] {
        let mut out = String::with_capacity(drawn.len());
        let mut rest = drawn.as_str();
        let needle = format!("{prefix}=\"#");
        while let Some(at) = rest.find(&needle) {
            out.push_str(&rest[..at]);
            out.push_str(&format!("{prefix}=\"{paint}\""));
            let tail = &rest[at + needle.len()..];
            match tail.find('"') {
                Some(end) => rest = &tail[end + 1..],
                None => {
                    rest = "";
                    break;
                }
            }
        }
        out.push_str(rest);
        drawn = out;
    }
    drawn
}

/// The tray icon: the tile and the stand carry the status colour, the head keeps
/// the holiday's own colours from the mockup.
///
/// The plain microphone has no head to decorate, so it is painted whole.
fn tray_drawing(body: &str, accent: Option<[u8; 3]>, color: [u8; 3]) -> String {
    let status = hex(color);
    let inner = inner(body);
    let drawn = match accent {
        None => paint_all(inner, &status),
        Some(accent) => {
            // The file is the stand followed by one group holding the head.
            let head_at = inner.find("<g ").unwrap_or(inner.len());
            let (stand, head) = inner.split_at(head_at);
            let head = head.replace("currentColor", &hex(accent));
            format!("{} {}", paint_all(stand, &status), head)
        }
    };
    let tile = hex(mix(color, BACKGROUND, TILE_ALPHA));
    let offset = (24.0 - SPAN) / 2.0;
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24"><rect width="24" height="24" rx="{radius:.3}" fill="{tile}"/><g transform="translate({offset:.3} {offset:.3}) scale({INSET})">{drawn}</g></svg>"#,
        radius = TILE_RADIUS,
        offset = offset,
    )
}

/// A tray icon: the status colour on the tile and the stand, the holiday's own
/// colours on the head.
pub fn tray_icon(holiday_id: Option<&str>, color: [u8; 3]) -> Icon {
    let (rgba, width, height) =
        render_rgba(&tray_svg(holiday_id, color), TRAY_SIDE).expect("every icon renders");
    Icon::from_rgba(rgba, width, height).expect("valid tray icon")
}

/// The same drawing as [`tray_icon`], as SVG. The preview tool and the tests use
/// it so they look at exactly what the tray draws.
pub fn tray_svg(holiday_id: Option<&str>, color: [u8; 3]) -> String {
    match holiday_id.and_then(holiday) {
        Some((_, accent, body)) => tray_drawing(body, Some(accent), color),
        None => tray_drawing(ICON_SVG, None, color),
    }
}

/// The holiday microphone for the web page: the stand keeps `currentColor`, so
/// the record button's colour reaches it, and the head keeps the holiday's own
/// colours exactly as in the tray.
pub fn holiday_page_svg(id: &str) -> Option<String> {
    let (_, accent, body) = holiday(id)?;
    let inner = inner(body);
    let head_at = inner.find("<g ").unwrap_or(inner.len());
    let (stand, head) = inner.split_at(head_at);
    let head = head.replace("currentColor", &hex(accent));
    Some(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">{stand}{head}</svg>"#
    ))
}

/// Whether `id` names a holiday icon. The config's `debug_holiday` is checked
/// with this so a typo cannot blank the tray.
pub fn is_holiday(id: &str) -> bool {
    holiday(id).is_some()
}

fn holiday(id: &str) -> Option<(&'static str, [u8; 3], &'static str)> {
    HOLIDAYS.iter().find(|(name, _, _)| *name == id).copied()
}

fn options() -> &'static resvg::usvg::Options<'static> {
    static OPTIONS: OnceLock<resvg::usvg::Options<'static>> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        let mut options = resvg::usvg::Options::default();
        // The calendar holiday draws its day number as text, so the loader
        // needs the installed fonts.
        options.fontdb_mut().load_system_fonts();
        options
    })
}

/// Rasterize `svg` into straight RGBA at `side` × `side`.
///
/// `tiny-skia` works in premultiplied alpha; `Icon::from_rgba` expects straight
/// alpha, so the channels are divided back out.
pub fn render_rgba(svg: &str, side: u32) -> Option<(Vec<u8>, u32, u32)> {
    let tree = resvg::usvg::Tree::from_str(svg, options()).ok()?;
    let size = tree.size();
    let scale = (side as f32 / size.width()).min(side as f32 / size.height());
    let mut pixmap = resvg::tiny_skia::Pixmap::new(side, side)?;
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for pixel in pixmap.pixels() {
        let alpha = pixel.alpha();
        if alpha == 0 {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        let unpremultiply = |channel: u8| {
            ((channel as u32 * 255 + (alpha as u32 / 2)) / alpha as u32).min(255) as u8
        };
        rgba.extend_from_slice(&[
            unpremultiply(pixel.red()),
            unpremultiply(pixel.green()),
            unpremultiply(pixel.blue()),
            alpha,
        ]);
    }
    Some((rgba, side, side))
}

/// Rasterize an icon SVG to PNG bytes. Used by the tool that builds the
/// executable's `.ico` and the phone's launcher icons, so the shipped bitmaps
/// come from the same SVG as the tray.
pub fn render_png(svg: &str, side: u32) -> Option<Vec<u8>> {
    let (rgba, width, height) = render_rgba(svg, side)?;
    let mut png = Vec::new();
    image::RgbaImage::from_raw(width, height, rgba)?
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// Icon for the application window (taskbar, title bar, Task Manager).
pub fn window_icon() -> eframe::egui::IconData {
    let (rgba, width, height) =
        render_rgba(&base_tiled_svg(), WINDOW_SIDE).expect("the embedded base icon renders");
    eframe::egui::IconData { rgba, width, height }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white(id: &str) -> String {
        tray_svg(Some(id), [0xff, 0xff, 0xff])
    }

    #[test]
    fn the_stand_takes_the_status_colour_and_the_head_keeps_the_holiday_colours() {
        // The New Year head is drawn in the mockup's own gold and blue.
        let svg = white("newyear");
        assert!(svg.contains("#ffd36e"), "the clock's gold must survive");
        assert!(
            svg.contains("stroke=\"#ffffff\""),
            "the stand must be painted in the status colour"
        );
        assert!(!svg.contains("currentColor"), "nothing may be inherited in the tray");
    }

    #[test]
    fn a_head_drawn_with_current_colour_takes_the_holiday_accent() {
        assert!(white("love").contains("#ff7fa8"), "the heart keeps its accent");
        assert!(white("christmas").contains("#8fe3b0"), "the tree keeps its green");
        assert!(white("maslenitsa").contains("#ffd36e"), "the sun keeps its gold");
    }

    #[test]
    fn the_plain_microphone_is_painted_whole() {
        let svg = tray_svg(None, [0xff, 0x5c, 0x5c]);
        assert!(!svg.contains("currentColor"));
        assert!(svg.contains("#ff5c5c"));
    }

    #[test]
    fn a_holiday_still_changes_the_shape() {
        let plain = tray_svg(None, [0xff, 0xff, 0xff]);
        for (id, _, _) in HOLIDAYS {
            assert_ne!(white(id), plain, "{id} must still have its own head");
        }
    }

    #[test]
    fn the_tile_is_the_colour_over_the_dark_surface() {
        // 13 % of the Christmas green over #111713.
        let mixed = mix([0x8f, 0xe3, 0xb0], BACKGROUND, TILE_ALPHA);
        assert_eq!(hex(mixed), "#213227");
    }

    #[test]
    fn the_microphone_fills_ninety_percent_of_the_tile() {
        assert!((SPAN - 21.6).abs() < 1e-3);
    }

    #[test]
    fn every_holiday_renders_at_tray_size() {
        for (id, _, _) in HOLIDAYS {
            let (rgba, width, height) = render_rgba(&white(id), TRAY_SIDE).expect(id);
            assert_eq!((width, height), (TRAY_SIDE, TRAY_SIDE));
            assert_eq!(rgba.len(), (TRAY_SIDE * TRAY_SIDE * 4) as usize);
        }
    }

    #[test]
    fn an_unknown_holiday_has_no_icon() {
        assert!(holiday_page_svg("no-such-day").is_none());
    }

    #[test]
    fn the_page_inherits_the_stand_colour_and_keeps_the_head_colours() {
        let svg = holiday_page_svg("newyear").unwrap();
        assert!(svg.contains("currentColor"), "the stand must follow the button");
        assert!(svg.contains("#ffd36e"), "the clock keeps the mockup's gold");
        assert!(!svg.contains("<rect"), "the tile belongs to the tray only");
        assert!(holiday_page_svg("love").unwrap().contains("#ff7fa8"));
        assert!(holiday_page_svg("no-such-day").is_none());
    }
}
