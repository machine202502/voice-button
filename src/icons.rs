use image::GenericImageView;
use std::sync::OnceLock;
use tray_icon::Icon;

const ICON_DATA: &[u8] = include_bytes!("../assets/icon.png");

/// Size Windows uses for notification-area icons at 100–200 % scaling. Feeding
/// the shell the 512 px master and letting it downscale produced the fuzzy tray
/// icon, so the master is resampled once with a proper filter.
const TRAY_SIDE: u32 = 32;

struct BaseIcon {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn base() -> &'static BaseIcon {
    static BASE: OnceLock<BaseIcon> = OnceLock::new();
    BASE.get_or_init(|| {
        let img = image::load_from_memory(ICON_DATA).expect("embedded icon.png is valid");
        let (width, height) = img.dimensions();
        BaseIcon {
            width,
            height,
            rgba: img.to_rgba8().into_raw(),
        }
    })
}

/// The master icon resampled to tray size with a high-quality filter.
fn tray_base() -> &'static BaseIcon {
    static TRAY: OnceLock<BaseIcon> = OnceLock::new();
    TRAY.get_or_init(|| {
        let base = base();
        let image = image::RgbaImage::from_raw(base.width, base.height, base.rgba.clone())
            .expect("embedded icon has matching dimensions");
        let resized = image::imageops::resize(
            &image,
            TRAY_SIDE,
            TRAY_SIDE,
            image::imageops::FilterType::Lanczos3,
        );
        BaseIcon {
            width: TRAY_SIDE,
            height: TRAY_SIDE,
            rgba: resized.into_raw(),
        }
    })
}

fn tinted_base(icon: &BaseIcon, ratio: [f32; 3]) -> Icon {
    let mut rgba = icon.rgba.clone();
    for px in rgba.chunks_exact_mut(4) {
        px[0] = (px[0] as f32 * ratio[0]).min(255.0) as u8;
        px[1] = (px[1] as f32 * ratio[1]).min(255.0) as u8;
        px[2] = (px[2] as f32 * ratio[2]).min(255.0) as u8;
    }
    Icon::from_rgba(rgba, icon.width, icon.height).expect("valid tray icon")
}

fn tinted(ratio: [f32; 3]) -> Icon {
    tinted_base(tray_base(), ratio)
}

pub fn icon_idle() -> Icon {
    tinted([1.0, 1.0, 1.0])
}

pub fn icon_recording() -> Icon {
    tinted([1.0, 0.35, 0.35])
}

pub fn icon_transcribing() -> Icon {
    tinted([1.0, 0.65, 0.25])
}

/// Grayed-out icon, used when no usable model is selected so recording is
/// unavailable.
pub fn icon_disabled() -> Icon {
    tinted([0.45, 0.45, 0.48])
}

/// Icon for the application window (taskbar, title bar, Task Manager).
pub fn window_icon() -> eframe::egui::IconData {
    let base = base();
    eframe::egui::IconData {
        rgba: base.rgba.clone(),
        width: base.width,
        height: base.height,
    }
}
