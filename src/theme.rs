use crate::models::ThemeMode;
use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, Stroke, Vec2, pos2, vec2};

/// Color tokens and geometry translated from `jizihub.tokens.json` and `jizihub.css`.
#[derive(Debug, Clone)]
pub struct JizihubPalette {
    pub mode: ThemeMode,
    pub bg_canvas: Color32,
    pub bg_orb_1: Color32,
    pub bg_orb_2: Color32,
    pub bg_orb_3: Color32,
    pub grid_color: Color32,
    pub glass_1: Color32,
    pub glass_2: Color32,
    pub glass_3: Color32,
    pub glass_hover: Color32,
    pub surface_solid_1: Color32,
    pub surface_solid_2: Color32,
    pub surface_solid_3: Color32,
    pub border_glass: Color32,
    pub border_strong: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_text: Color32,
    pub accent_soft: Color32,
    pub accent_border: Color32,
    pub accent_contrast: Color32,
    pub text_1: Color32,
    pub text_2: Color32,
    pub text_3: Color32,
    pub warn_fg: Color32,
    pub error_fg: Color32,
    pub ok_fg: Color32,
}

/// Returns the `jizihub` colorway palette for the given `ThemeMode`.
pub fn palette(mode: ThemeMode) -> JizihubPalette {
    match mode {
        ThemeMode::Dark => JizihubPalette {
            mode,
            // Smoked Obsidian
            bg_canvas: Color32::from_rgb(0x0c, 0x0e, 0x12),
            bg_orb_1: Color32::from_rgba_unmultiplied(255, 106, 43, 46),
            bg_orb_2: Color32::from_rgba_unmultiplied(56, 76, 110, 44),
            bg_orb_3: Color32::from_rgba_unmultiplied(234, 88, 12, 32),
            grid_color: Color32::from_rgba_unmultiplied(255, 255, 255, 12),
            glass_1: Color32::from_rgba_unmultiplied(22, 25, 32, 212),
            glass_2: Color32::from_rgba_unmultiplied(30, 34, 43, 226),
            glass_3: Color32::from_rgba_unmultiplied(36, 40, 52, 242),
            glass_hover: Color32::from_rgba_unmultiplied(48, 54, 68, 232),
            surface_solid_1: Color32::from_rgb(0x16, 0x19, 0x20),
            surface_solid_2: Color32::from_rgb(0x1e, 0x22, 0x2b),
            surface_solid_3: Color32::from_rgb(0x24, 0x28, 0x34),
            border_glass: Color32::from_rgba_unmultiplied(255, 255, 255, 24),
            border_strong: Color32::from_rgba_unmultiplied(255, 255, 255, 44),
            accent: Color32::from_rgb(0xff, 0x6a, 0x2b),
            accent_hover: Color32::from_rgb(0xff, 0x81, 0x4a),
            accent_text: Color32::from_rgb(0xff, 0x82, 0x4d),
            accent_soft: Color32::from_rgba_unmultiplied(255, 106, 43, 38),
            accent_border: Color32::from_rgba_unmultiplied(255, 106, 43, 120),
            accent_contrast: Color32::from_rgb(0x11, 0x0c, 0x09),
            text_1: Color32::from_rgb(0xf4, 0xf2, 0xee),
            text_2: Color32::from_rgb(0xb8, 0xbd, 0xc8),
            text_3: Color32::from_rgb(0x8c, 0x93, 0xa3),
            warn_fg: Color32::from_rgb(245, 190, 72),
            error_fg: Color32::from_rgb(240, 98, 92),
            ok_fg: Color32::from_rgb(112, 206, 146),
        },
        ThemeMode::Light => JizihubPalette {
            mode,
            // Warm Sandstone
            bg_canvas: Color32::from_rgb(0xe7, 0xdf, 0xd4),
            bg_orb_1: Color32::from_rgba_unmultiplied(224, 98, 36, 48),
            bg_orb_2: Color32::from_rgba_unmultiplied(168, 149, 126, 60),
            bg_orb_3: Color32::from_rgba_unmultiplied(245, 158, 92, 54),
            grid_color: Color32::from_rgba_unmultiplied(66, 52, 38, 15),
            glass_1: Color32::from_rgba_unmultiplied(250, 245, 238, 214),
            glass_2: Color32::from_rgba_unmultiplied(252, 248, 243, 228),
            glass_3: Color32::from_rgba_unmultiplied(255, 252, 248, 244),
            glass_hover: Color32::from_rgba_unmultiplied(255, 252, 248, 252),
            surface_solid_1: Color32::from_rgb(0xf4, 0xed, 0xe3),
            surface_solid_2: Color32::from_rgb(0xfa, 0xf6, 0xf0),
            surface_solid_3: Color32::from_rgb(0xff, 0xfc, 0xf8),
            border_glass: Color32::from_rgba_unmultiplied(72, 54, 38, 28),
            border_strong: Color32::from_rgba_unmultiplied(72, 54, 38, 54),
            accent: Color32::from_rgb(0xd9, 0x4e, 0x14),
            accent_hover: Color32::from_rgb(0xbf, 0x42, 0x0e),
            accent_text: Color32::from_rgb(0xb8, 0x3d, 0x0b),
            accent_soft: Color32::from_rgba_unmultiplied(217, 78, 20, 32),
            accent_border: Color32::from_rgba_unmultiplied(217, 78, 20, 98),
            accent_contrast: Color32::from_rgb(0xff, 0xfa, 0xf6),
            text_1: Color32::from_rgb(0x24, 0x1c, 0x15),
            text_2: Color32::from_rgb(0x52, 0x45, 0x39),
            text_3: Color32::from_rgb(0x68, 0x5a, 0x4d),
            warn_fg: Color32::from_rgb(168, 92, 10),
            error_fg: Color32::from_rgb(185, 42, 36),
            ok_fg: Color32::from_rgb(32, 128, 72),
        },
    }
}

/// Applies `jizihub` typography spacing, semi-rectangular radii, and colorway visuals to `egui`.
pub fn apply_jizihub_theme(ctx: &egui::Context, mode: ThemeMode) {
    let pal = palette(mode);
    let egui_theme = match mode {
        ThemeMode::Dark => egui::Theme::Dark,
        ThemeMode::Light => egui::Theme::Light,
    };
    ctx.set_theme(egui_theme);

    ctx.style_mut_of(egui_theme, |style| {
        style.spacing.item_spacing = vec2(10.0, 8.0);
        style.spacing.button_padding = vec2(12.0, 6.0);
        style.spacing.interact_size = vec2(40.0, 26.0);
        style.spacing.window_margin = Margin::same(16);

        let mut visuals = match mode {
            ThemeMode::Dark => egui::Visuals::dark(),
            ThemeMode::Light => egui::Visuals::light(),
        };

        visuals.override_text_color = Some(pal.text_1);
        visuals.weak_text_color = Some(pal.text_3);
        visuals.panel_fill = pal.bg_canvas;
        visuals.window_fill = pal.surface_solid_3;
        visuals.window_stroke = Stroke::new(1.0, pal.border_strong);
        visuals.window_corner_radius = CornerRadius::same(6);
        visuals.menu_corner_radius = CornerRadius::same(5);
        visuals.faint_bg_color = pal.glass_1;
        visuals.extreme_bg_color = pal.surface_solid_1;
        visuals.text_edit_bg_color = Some(pal.surface_solid_2);
        visuals.code_bg_color = pal.surface_solid_2;
        visuals.warn_fg_color = pal.warn_fg;
        visuals.error_fg_color = pal.error_fg;
        visuals.hyperlink_color = pal.accent_text;

        visuals.selection.bg_fill = pal.accent_soft;
        visuals.selection.stroke = Stroke::new(1.0, pal.accent);

        // Non-interactive borders and text
        visuals.widgets.noninteractive.bg_fill = pal.glass_1;
        visuals.widgets.noninteractive.weak_bg_fill = pal.surface_solid_1;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal.border_glass);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, pal.text_2);
        visuals.widgets.noninteractive.corner_radius = CornerRadius::same(4);

        // Inactive buttons / controls
        visuals.widgets.inactive.bg_fill = pal.glass_2;
        visuals.widgets.inactive.weak_bg_fill = pal.glass_2;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, pal.border_strong);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, pal.text_1);
        visuals.widgets.inactive.corner_radius = CornerRadius::same(4);

        // Hovered buttons / controls
        visuals.widgets.hovered.bg_fill = pal.glass_hover;
        visuals.widgets.hovered.weak_bg_fill = pal.glass_hover;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, pal.accent_border);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, pal.text_1);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(4);

        // Active / pressed controls
        visuals.widgets.active.bg_fill = pal.accent_soft;
        visuals.widgets.active.weak_bg_fill = pal.accent_soft;
        visuals.widgets.active.bg_stroke = Stroke::new(1.5, pal.accent);
        visuals.widgets.active.fg_stroke = Stroke::new(1.5, pal.accent_text);
        visuals.widgets.active.corner_radius = CornerRadius::same(4);

        // Open menus / combos
        visuals.widgets.open.bg_fill = pal.glass_3;
        visuals.widgets.open.weak_bg_fill = pal.glass_3;
        visuals.widgets.open.bg_stroke = Stroke::new(1.0, pal.accent_border);
        visuals.widgets.open.fg_stroke = Stroke::new(1.0, pal.text_1);
        visuals.widgets.open.corner_radius = CornerRadius::same(5);

        visuals.slider_trailing_fill = true;
        style.visuals = visuals;
    });
}

/// Paints the `jizihub` Ambient Orbs + 48px architectural grid behind a panel.
pub fn paint_ambient_backdrop(ui: &egui::Ui, pal: &JizihubPalette) {
    let rect = ui.max_rect();
    let painter = ui.painter();

    painter.rect_filled(rect, 0.0, pal.bg_canvas);

    // Soft radial orbs rendered via layered concentric translucent circles.
    paint_radial_orb(
        painter,
        pos2(rect.right() - rect.width() * 0.14, rect.top() + 90.0),
        340.0,
        pal.bg_orb_1,
    );
    paint_radial_orb(
        painter,
        pos2(rect.left() + rect.width() * 0.14, rect.bottom() - 80.0),
        320.0,
        pal.bg_orb_2,
    );
    paint_radial_orb(
        painter,
        pos2(rect.left() + rect.width() * 0.45, rect.center().y),
        260.0,
        pal.bg_orb_3,
    );

    // 48px architectural grid
    let step = 48.0_f32;
    let stroke = Stroke::new(1.0, pal.grid_color);
    let mut x = rect.left();
    while x <= rect.right() {
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], stroke);
        x += step;
    }
    let mut y = rect.top();
    while y <= rect.bottom() {
        painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], stroke);
        y += step;
    }
}

fn paint_radial_orb(painter: &egui::Painter, center: egui::Pos2, radius: f32, color: Color32) {
    let rings = 6;
    for i in (1..=rings).rev() {
        let t = i as f32 / rings as f32;
        let r = radius * t;
        let alpha = ((color.a() as f32) * (1.0 - t * 0.72) / rings as f32 * 2.2)
            .clamp(0.0, 255.0) as u8;
        let ring_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
        painter.circle_filled(center, r, ring_color);
    }
}

/// Tier-1 outer frosted glass panel (`6px` radius, `16px` margin).
pub fn glass_frame_1(pal: &JizihubPalette) -> Frame {
    Frame::new()
        .fill(pal.glass_1)
        .stroke(Stroke::new(1.0, pal.border_glass))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(16))
}

/// Tier-2 nested interactive panel (`5px` radius, `12px` margin).
pub fn glass_frame_2(pal: &JizihubPalette) -> Frame {
    Frame::new()
        .fill(pal.glass_2)
        .stroke(Stroke::new(1.0, pal.border_glass))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::same(12))
}

/// Tier-3 floating top bar / modal / highlighted card (`5px` radius, `12px` margin, stronger border).
pub fn glass_frame_3(pal: &JizihubPalette) -> Frame {
    Frame::new()
        .fill(pal.glass_3)
        .stroke(Stroke::new(1.0, pal.border_strong))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::same(12))
}

/// Renders an orange primary action button matching `.btn-accent`.
pub fn accent_button(
    ui: &mut egui::Ui,
    pal: &JizihubPalette,
    enabled: bool,
    text: &str,
) -> egui::Response {
    let label = egui::RichText::new(text).color(pal.accent_contrast).strong();
    let btn = egui::Button::new(label)
        .fill(pal.accent)
        .stroke(Stroke::new(1.0, pal.accent))
        .corner_radius(CornerRadius::same(4))
        .min_size(Vec2::new(0.0, 30.0));
    ui.add_enabled(enabled, btn)
}

/// Renders an outlined orange secondary action button matching `.btn-outline-orange`.
pub fn outline_orange_button(
    ui: &mut egui::Ui,
    pal: &JizihubPalette,
    enabled: bool,
    text: &str,
) -> egui::Response {
    let label = egui::RichText::new(text).color(pal.accent_text).strong();
    let btn = egui::Button::new(label)
        .fill(pal.accent_soft)
        .stroke(Stroke::new(1.0, pal.accent_border))
        .corner_radius(CornerRadius::same(4))
        .min_size(Vec2::new(0.0, 28.0));
    ui.add_enabled(enabled, btn)
}

/// Renders a semi-rectangular pill badge (accent orange or subtle glass).
pub fn render_badge(ui: &mut egui::Ui, pal: &JizihubPalette, text: &str, is_accent: bool) {
    let (fill, border, fg) = if is_accent {
        (pal.accent_soft, pal.accent_border, pal.accent_text)
    } else {
        (pal.glass_1, pal.border_strong, pal.text_2)
    };
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(3))
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).small().strong().color(fg));
        });
}
