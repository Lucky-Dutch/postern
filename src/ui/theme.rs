use eframe::egui::{self, Color32, Stroke};
use std::sync::atomic::{AtomicBool, Ordering};

/// Colors of one appearance.
struct Palette {
    background: Color32,
    sidebar: Color32,
    toolbar: Color32,
    border: Color32,
    muted: Color32,
    accent: Color32,
    green: Color32,
    gold: Color32,
    selected: Color32,
    warning_fill: Color32,
    extreme: Color32,
    faint: Color32,
    widget: Color32,
    hovered: Color32,
    selection: Color32,
}

const DARK: Palette = Palette {
    background: Color32::from_rgb(37, 38, 40),
    sidebar: Color32::from_rgb(44, 46, 49),
    toolbar: Color32::from_rgb(30, 35, 41),
    border: Color32::from_rgb(65, 68, 73),
    muted: Color32::from_rgb(151, 157, 167),
    accent: Color32::from_rgb(39, 128, 244),
    green: Color32::from_rgb(92, 187, 141),
    gold: Color32::from_rgb(191, 163, 100),
    selected: Color32::from_rgb(48, 65, 85),
    warning_fill: Color32::from_rgb(70, 58, 30),
    extreme: Color32::from_rgb(30, 32, 35),
    faint: Color32::from_rgb(43, 44, 46),
    widget: Color32::from_rgb(57, 59, 63),
    hovered: Color32::from_rgb(68, 72, 79),
    selection: Color32::from_rgb(54, 91, 135),
};

const LIGHT: Palette = Palette {
    background: Color32::from_rgb(252, 252, 253),
    sidebar: Color32::from_rgb(243, 244, 246),
    toolbar: Color32::from_rgb(234, 236, 240),
    border: Color32::from_rgb(208, 211, 217),
    muted: Color32::from_rgb(100, 106, 118),
    accent: Color32::from_rgb(24, 110, 230),
    green: Color32::from_rgb(28, 135, 85),
    gold: Color32::from_rgb(150, 108, 22),
    selected: Color32::from_rgb(214, 229, 250),
    warning_fill: Color32::from_rgb(252, 239, 204),
    extreme: Color32::from_rgb(255, 255, 255),
    faint: Color32::from_rgb(245, 246, 248),
    widget: Color32::from_rgb(228, 230, 234),
    hovered: Color32::from_rgb(216, 219, 225),
    selection: Color32::from_rgb(190, 214, 248),
};

/// Server colors. They read well on both appearances.
pub const COLORS: [Color32; 7] = [
    Color32::from_rgb(39, 128, 244),
    Color32::from_rgb(70, 170, 120),
    Color32::from_rgb(191, 150, 70),
    Color32::from_rgb(223, 113, 111),
    Color32::from_rgb(163, 130, 210),
    Color32::from_rgb(60, 165, 180),
    Color32::from_rgb(140, 146, 156),
];

static DARK_MODE: AtomicBool = AtomicBool::new(true);

fn palette() -> &'static Palette {
    if DARK_MODE.load(Ordering::Relaxed) {
        &DARK
    } else {
        &LIGHT
    }
}

pub fn is_dark() -> bool {
    DARK_MODE.load(Ordering::Relaxed)
}

pub fn background() -> Color32 {
    palette().background
}
pub fn sidebar() -> Color32 {
    palette().sidebar
}
pub fn toolbar() -> Color32 {
    palette().toolbar
}
pub fn border() -> Color32 {
    palette().border
}
pub fn muted() -> Color32 {
    palette().muted
}
pub fn accent() -> Color32 {
    palette().accent
}
pub fn green() -> Color32 {
    palette().green
}
pub fn gold() -> Color32 {
    palette().gold
}
/// Background of a selected card, such as the chosen saved server.
pub fn selected() -> Color32 {
    palette().selected
}
pub fn warning_fill() -> Color32 {
    palette().warning_fill
}

/// Syntax highlighting for the current appearance.
pub fn code_theme(font_size: f32) -> egui_extras::syntax_highlighting::CodeTheme {
    if is_dark() {
        egui_extras::syntax_highlighting::CodeTheme::dark(font_size)
    } else {
        egui_extras::syntax_highlighting::CodeTheme::light(font_size)
    }
}

/// Installs fonts and both appearances' styles. egui picks the style for the
/// current theme preference.
pub fn apply(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, path, family) in [
        (
            "macos-ui",
            "/System/Library/Fonts/SFNS.ttf",
            egui::FontFamily::Proportional,
        ),
        (
            "macos-code",
            "/System/Library/Fonts/SFNSMono.ttf",
            egui::FontFamily::Monospace,
        ),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.into(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, name.into());
        }
    }
    ctx.set_fonts(fonts);
    ctx.set_style_of(egui::Theme::Dark, style(&DARK, egui::Visuals::dark()));
    ctx.set_style_of(egui::Theme::Light, style(&LIGHT, egui::Visuals::light()));
}

/// Applies an appearance preference and keeps the palette in step with the
/// theme egui is using, which follows the system for `System`.
pub fn sync(ctx: &egui::Context, preference: egui::ThemePreference) {
    if ctx.options(|options| options.theme_preference) != preference {
        ctx.set_theme(preference);
    }
    DARK_MODE.store(ctx.theme() == egui::Theme::Dark, Ordering::Relaxed);
}

fn style(palette: &Palette, visuals: egui::Visuals) -> egui::Style {
    let mut style = egui::Style {
        visuals,
        ..Default::default()
    };
    let text = if palette.background.r() > 128 {
        Color32::from_gray(30)
    } else {
        Color32::from_gray(213)
    };
    style.visuals.panel_fill = palette.background;
    style.visuals.window_fill = palette.sidebar;
    style.visuals.extreme_bg_color = palette.extreme;
    style.visuals.faint_bg_color = palette.faint;
    style.visuals.selection.bg_fill = palette.selection;
    style.visuals.selection.stroke = Stroke::new(1.0, text);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text);
    style.visuals.widgets.inactive.bg_fill = palette.widget;
    style.visuals.widgets.inactive.weak_bg_fill = palette.widget;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, palette.border);
    style.visuals.widgets.hovered.bg_fill = palette.hovered;
    style.visuals.widgets.active.bg_fill = palette.accent;
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 28.0;
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, egui::FontId::monospace(14.0));
    style
}

pub fn navigation_item(
    ui: &mut egui::Ui,
    name: &str,
    selected: bool,
    kind: &str,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
    let painter = ui.painter().with_clip_rect(rect);
    if selected || response.hovered() {
        painter.rect_filled(
            rect,
            4,
            if selected {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().widgets.hovered.weak_bg_fill
            },
        );
    }
    let icon = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 15.0, rect.center().y),
        egui::vec2(13.0, 12.0),
    );
    let stroke = Stroke::new(1.0, if kind == "table" { gold() } else { muted() });
    if kind == "function" {
        painter.text(
            icon.center(),
            egui::Align2::CENTER_CENTER,
            "ƒ",
            egui::FontId::proportional(13.0),
            green(),
        );
    } else if kind == "sql" {
        painter.text(
            icon.center(),
            egui::Align2::CENTER_CENTER,
            "SQL",
            egui::FontId::monospace(8.0),
            muted(),
        );
    } else {
        painter.rect_stroke(icon, 1, stroke, egui::StrokeKind::Inside);
        for fraction in [0.33, 0.66] {
            let y = icon.top() + icon.height() * fraction;
            painter.line_segment(
                [egui::pos2(icon.left(), y), egui::pos2(icon.right(), y)],
                stroke,
            );
        }
        if kind == "materialized" {
            painter.rect_filled(icon.shrink(3.0), 0, muted().gamma_multiply(0.6));
        }
        if kind == "table" {
            painter.line_segment(
                [
                    egui::pos2(icon.center().x, icon.top()),
                    egui::pos2(icon.center().x, icon.bottom()),
                ],
                stroke,
            );
        }
    }
    painter.text(
        egui::pos2(rect.left() + 30.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(14.0),
        ui.visuals().text_color(),
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn primary(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(egui::Button::new(egui::RichText::new(label).color(Color32::WHITE)).fill(accent()))
}

/// The application icon: a mammoth whose head is a database cylinder. Generated
/// by `scripts/make-artwork.sh`.
const APP_ICON: &[u8] = include_bytes!("../../assets/icon-512.png");

/// The application icon for the window and the Dock.
pub fn app_icon() -> Option<egui::IconData> {
    eframe::icon_data::from_png_bytes(APP_ICON).ok()
}

/// The application icon as a texture, for the server list's sidebar.
pub fn logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let icon = app_icon()?;
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.rgba,
    );
    Some(ctx.load_texture(
        "postern-logo",
        image,
        egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear)),
    ))
}

pub fn database_icon(ui: &mut egui::Ui, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let p = ui.painter();
    let x1 = rect.left() + size * 0.15;
    let x2 = rect.right() - size * 0.15;
    let top = rect.top() + size * 0.2;
    let bottom = rect.bottom() - size * 0.2;
    let stroke = Stroke::new((size / 35.0).max(1.3), color);
    p.line_segment([egui::pos2(x1, top), egui::pos2(x1, bottom)], stroke);
    p.line_segment([egui::pos2(x2, top), egui::pos2(x2, bottom)], stroke);
    for y in [top, (top + bottom) / 2.0, bottom] {
        p.add(egui::epaint::EllipseShape::stroke(
            egui::pos2(rect.center().x, y),
            egui::vec2((x2 - x1) / 2.0, size * 0.1),
            stroke,
        ));
    }
}
