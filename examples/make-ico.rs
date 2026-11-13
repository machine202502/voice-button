//! Build the bitmaps Windows and the phone's launcher cannot take as SVG:
//! `assets/icon.ico` for the executable and the three PWA icons under
//! `src/web/icons/`. Everything else — tray, window, web page — reads
//! `assets/icon.svg` directly through `voice_button::icons`.
//!
//!   cargo run --release --example make-ico

use std::fs;

use image::{Rgba, RgbaImage};
use voice_button::icons;

/// Sizes Windows asks for: the shell scales down from these.
const SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];

/// The icon's tile colour, used to fill the maskable icon's safe area.
const TILE: [u8; 4] = [0x29, 0x34, 0x1e, 255];

fn main() -> anyhow::Result<()> {
    let svg = icons::base_tiled_svg();
    let mut pngs = Vec::new();
    for side in SIZES {
        let png = icons::render_png(&svg, side).ok_or_else(|| anyhow::anyhow!("render {side} px"))?;
        pngs.push((side, png));
    }
    fs::write("assets/icon.ico", ico(&pngs))?;
    println!("wrote assets/icon.ico with {} sizes", pngs.len());

    // The phone's launcher takes PNG from the manifest; the installed icon is
    // static by design, so it is built once and shipped.
    for side in [192u32, 512] {
        let png = icons::render_png(&svg, side).ok_or_else(|| anyhow::anyhow!("render {side} px"))?;
        fs::write(format!("web/public/icons/icon-{side}.png"), png)?;
        println!("wrote web/public/icons/icon-{side}.png");
    }
    // Maskable icons are cropped to a circle on some launchers, so the artwork
    // must stay inside the middle 80 %; the rest is the tile colour.
    let inner = icons::render_rgba(&svg, 410).ok_or_else(|| anyhow::anyhow!("render maskable"))?;
    let inner = RgbaImage::from_raw(inner.1, inner.2, inner.0).expect("matching dimensions");
    let mut maskable = RgbaImage::from_pixel(512, 512, Rgba(TILE));
    image::imageops::overlay(&mut maskable, &inner, 51, 51);
    maskable.save("web/public/icons/maskable-512.png")?;
    println!("wrote web/public/icons/maskable-512.png");

    // The favicon is the same artwork in vector form: the phone page and the
    // promo site both show it in a browser tab, and a hand-drawn second copy is
    // exactly how they ended up with an icon the app no longer uses.
    for target in ["web/public/favicon.svg", "site/assets/favicon.svg"] {
        fs::write(target, &svg)?;
        println!("wrote {target}");
    }
    Ok(())
}

/// An ICO container with PNG-compressed entries, which Windows has accepted
/// since Vista and which avoids hand-writing per-pixel-size DIBs.
fn ico(pngs: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&(pngs.len() as u16).to_le_bytes());

    // 256 is stored as 0 in the directory entry.
    let dimension = |side: u32| if side >= 256 { 0u8 } else { side as u8 };
    let mut offset = 6 + 16 * pngs.len() as u32;
    for (side, png) in pngs {
        out.push(dimension(*side));
        out.push(dimension(*side));
        out.push(0); // palette size
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in pngs {
        out.extend_from_slice(png);
    }
    out
}
