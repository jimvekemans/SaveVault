use crate::theme::JizihubPalette;
use eframe::egui::{self, Color32, IconData, Pos2, Stroke, Vec2, pos2, vec2};
use flate2::{Compression, write::ZlibEncoder};
use std::io::Write;

/// Primary desktop application name.
pub const APP_NAME: &str = "SaveVault";
/// Top bar subtitle rendered beneath the application name.
pub const APP_SUBTITLE: &str = "GAME SAVE SYNC & ROLLBACK";
/// Window title shown by the OS window manager.
pub const WINDOW_TITLE: &str = "SaveVault — Game Save Sync & Rollback";

/// Embedded SVG icon for desktop shortcuts and asset export.
pub const SVG_LOGO: &str = include_str!("../assets/savevault.svg");

/// Generates a 64x64 RGBA window/launcher icon depicting the SaveVault shield and vault dial.
pub fn build_app_icon() -> IconData {
    const SIZE: usize = 64;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let center = 31.5_f32;

    for y in 0..SIZE {
        for x in 0..SIZE {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            let idx = (y * SIZE + x) * 4;

            // Rounded square badge bounds.
            let rx = (fx - center).abs();
            let ry = (fy - center).abs();
            let half = 29.0_f32;
            let corner_r = 9.0_f32;
            let dx = (rx - (half - corner_r)).max(0.0);
            let dy = (ry - (half - corner_r)).max(0.0);
            let badge_dist = (dx * dx + dy * dy).sqrt() - corner_r;

            if badge_dist > 0.6 {
                continue;
            }

            let alpha = ((0.6 - badge_dist).clamp(0.0, 1.0) * 255.0) as u8;
            // Base obsidian background: #161920
            let mut r = 0x16_u8;
            let mut g = 0x19_u8;
            let mut b = 0x20_u8;

            // Outer orange accent border ring inside rounded rect
            if badge_dist > -1.8 {
                r = 0xff;
                g = 0x6a;
                b = 0x2b;
            } else {
                // Inner circular vault dial centered at (32, 32)
                let dist = ((fx - center).powi(2) + (fy - center).powi(2)).sqrt();
                let on_crosshair = ((fx - center).abs() < 2.2 || (fy - center).abs() < 2.2)
                    && (12.0..=21.0).contains(&dist);

                if dist <= 5.8 || (13.5..=17.5).contains(&dist) || on_crosshair {
                    r = 0xff;
                    g = 0x6a;
                    b = 0x2b;
                } else if dist <= 13.5 {
                    r = 0x1e;
                    g = 0x22;
                    b = 0x2b;
                }
            }

            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            rgba[idx + 3] = alpha;
        }
    }

    IconData {
        rgba,
        width: SIZE as u32,
        height: SIZE as u32,
    }
}

fn crc32_ieee(chunk_type: &[u8; 4], data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for &byte in chunk_type.iter().chain(data.iter()) {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32_ieee(chunk_type, data).to_be_bytes());
}

/// Encodes `build_app_icon()` as a 64x64 RGBA PNG byte buffer.
pub fn build_app_icon_png() -> Vec<u8> {
    let icon = build_app_icon();
    let row_bytes = icon.width as usize * 4;
    let mut raw = Vec::with_capacity((row_bytes + 1) * icon.height as usize);
    for row in icon.rgba.chunks_exact(row_bytes) {
        raw.push(0);
        raw.extend_from_slice(row);
    }

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    let _ = encoder.write_all(&raw);
    let compressed = encoder.finish().unwrap_or_default();

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&icon.width.to_be_bytes());
    ihdr.extend_from_slice(&icon.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);

    let mut png = Vec::with_capacity(64 + compressed.len());
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    write_png_chunk(&mut png, b"IHDR", &ihdr);
    write_png_chunk(&mut png, b"IDAT", &compressed);
    write_png_chunk(&mut png, b"IEND", &[]);
    png
}

/// Paints the custom SaveVault vector shield + vault dial mark in the UI top bar.
pub fn paint_logo_mark(ui: &mut egui::Ui, pal: &JizihubPalette) {
    let size = Vec2::splat(30.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();

    painter.rect(
        rect,
        egui::CornerRadius::same(6),
        pal.surface_solid_1,
        Stroke::new(1.5, pal.accent_border),
        egui::StrokeKind::Inside,
    );

    let c = rect.center();
    let w = rect.width();
    let h = rect.height();

    // Shield outline inside the tile
    let shield: Vec<Pos2> = vec![
        pos2(c.x, rect.top() + h * 0.14),
        pos2(rect.right() - w * 0.20, rect.top() + h * 0.27),
        pos2(rect.right() - w * 0.20, rect.top() + h * 0.55),
        pos2(c.x, rect.bottom() - h * 0.14),
        pos2(rect.left() + w * 0.20, rect.top() + h * 0.55),
        pos2(rect.left() + w * 0.20, rect.top() + h * 0.27),
    ];
    painter.add(egui::Shape::closed_line(
        shield,
        Stroke::new(1.5, pal.accent),
    ));

    // Vault dial circle & spokes
    painter.circle_stroke(c + vec2(0.0, 0.5), 4.6, Stroke::new(1.4, pal.accent_text));
    painter.circle_filled(c + vec2(0.0, 0.5), 2.1, pal.accent);
    for (dx, dy) in [(0.0, -5.8), (0.0, 6.8), (-6.3, 0.5), (6.3, 0.5)] {
        let p = c + vec2(dx, dy);
        painter.circle_filled(p, 0.9, Color32::from_rgb(0xff, 0x9b, 0x6b));
    }
}
