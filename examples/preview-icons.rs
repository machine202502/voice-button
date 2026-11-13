//! Render the tray icons exactly as the application draws them: one row per
//! status colour, one column per icon, at the 32 px size Windows asks for.
//!
//!   cargo run --release --example preview-icons
//!
//! Writes `target/tray-icons.png` on a dark and a light taskbar.
use image::{Rgba, RgbaImage};
use voice_button::icons;

const IDS: [(&str, Option<&str>); 18] = [
    ("plain", None),
    ("newyear", Some("newyear")),
    ("cny", Some("cny")),
    ("love", Some("love")),
    ("defender", Some("defender")),
    ("women", Some("women")),
    ("maslenitsa", Some("maslenitsa")),
    ("friday", Some("friday")),
    ("clover", Some("clover")),
    ("easter", Some("easter")),
    ("space", Some("space")),
    ("victory", Some("victory")),
    ("halloween", Some("halloween")),
    ("diwali", Some("diwali")),
    ("dead", Some("dead")),
    ("christmas", Some("christmas")),
    ("hanukkah", Some("hanukkah")),
    ("eid", Some("eid")),
];

/// The tray's status colours, in the order the statuses are described.
const STATES: [(&str, [u8; 3]); 7] = [
    ("ready", [0xff, 0xff, 0xff]),
    ("recording", [0xff, 0x5c, 0x5c]),
    ("transcribing", [0xff, 0x9f, 0x43]),
    ("file", [0x5c, 0xe0, 0x7a]),
    ("background", [0xb9, 0xa2, 0xf0]),
    ("recovering", [0x6e, 0x9b, 0xff]),
    ("no model", [0x8a, 0x8a, 0x8f]),
];

const SIDE: u32 = 32;
const ZOOM: u32 = 3;
const GAP: u32 = 10;
const DARK: [u8; 4] = [32, 32, 32, 255];
const LIGHT: [u8; 4] = [243, 243, 243, 255];

fn main() -> anyhow::Result<()> {
    std::fs::create_dir_all("target")?;
    let cell = SIDE * ZOOM + GAP;
    let width = IDS.len() as u32 * cell + GAP;
    let height = 2 * (STATES.len() as u32 * cell + GAP) + GAP;
    let mut canvas = RgbaImage::from_pixel(width, height, Rgba([26, 26, 26, 255]));

    for (half, (offset, background)) in
        [(0i64, DARK), ((STATES.len() as u32 * cell + GAP) as i64, LIGHT)]
            .into_iter()
            .enumerate()
    {
        let _ = half;
        for (row, (state, color)) in STATES.iter().enumerate() {
            for (column, (name, holiday)) in IDS.iter().enumerate() {
                // The grey row is "no model": the whole icon is grey there, so a
                // holiday keeps no colours, exactly as the tray does it.
                let holiday = if *state == "no model" { None } else { *holiday };
                let (rgba, w, h) = icons::render_rgba(&icons::tray_svg(holiday, *color), SIDE)
                    .ok_or_else(|| anyhow::anyhow!("{name} does not render"))?;
                let tile = RgbaImage::from_raw(w, h, rgba).expect("matching dimensions");
                let tile = image::imageops::resize(
                    &tile,
                    w * ZOOM,
                    h * ZOOM,
                    image::imageops::FilterType::Nearest,
                );
                let mut composed = RgbaImage::from_pixel(tile.width(), tile.height(), Rgba(background));
                image::imageops::overlay(&mut composed, &tile, 0, 0);
                let x = GAP as i64 + column as i64 * cell as i64;
                let y = offset + GAP as i64 + row as i64 * cell as i64;
                image::imageops::overlay(&mut canvas, &composed, x, y);
            }
        }
    }

    canvas.save("target/tray-icons.png")?;
    println!("target/tray-icons.png ({width}x{height})");
    Ok(())
}
