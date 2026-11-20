//! Look and feel: colors, rounding, spacing and the small drawing helpers
//! shared by the panels in `app.rs`.

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Painter, Rect, Shadow, Stroke, Style, TextStyle, Theme};

/// Corner radius for buttons, rows and inputs.
pub const RADIUS: u8 = 8;

/// Corner radius for cards and dialogs.
const CARD_RADIUS: u8 = 12;

/// Colors used for sender avatars, picked by a hash of the sender's name.
const AVATAR_HUES: [Color32; 8] = [
    Color32::from_rgb(214, 96, 104),
    Color32::from_rgb(214, 128, 60),
    Color32::from_rgb(170, 138, 36),
    Color32::from_rgb(68, 150, 96),
    Color32::from_rgb(38, 146, 150),
    Color32::from_rgb(60, 116, 204),
    Color32::from_rgb(118, 92, 206),
    Color32::from_rgb(184, 86, 164),
];

/// Install the custom style for both light and dark mode. The app's theme
/// preference picks which one is used.
pub fn install(ctx: &egui::Context) {
    ctx.style_mut_of(Theme::Dark, |s| apply(s, true));
    ctx.style_mut_of(Theme::Light, |s| apply(s, false));
}

fn apply(style: &mut Style, dark: bool) {
    let p = Palette::new(dark);
    let v = &mut style.visuals;
    v.dark_mode = dark;
    v.panel_fill = p.panel;
    v.window_fill = p.window;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(CARD_RADIUS);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 30,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 150 } else { 40 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 130 } else { 36 }),
    };
    v.extreme_bg_color = p.field;
    v.faint_bg_color = p.faint;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);

    let radius = CornerRadius::same(RADIUS);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.faint;
    w.noninteractive.weak_bg_fill = p.faint;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = p.control;
    w.inactive.weak_bg_fill = p.control;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = radius;

    w.hovered.bg_fill = p.control_hover;
    w.hovered.weak_bg_fill = p.control_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent.gamma_multiply(0.7));
    w.hovered.fg_stroke = Stroke::new(1.0, p.text_strong);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.control_active;
    w.active.weak_bg_fill = p.control_active;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text_strong);
    w.active.corner_radius = radius;

    w.open.bg_fill = p.control;
    w.open.weak_bg_fill = p.control;
    w.open.bg_stroke = Stroke::new(1.0, p.border);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = radius;

    let sp = &mut style.spacing;
    sp.item_spacing = egui::vec2(8.0, 8.0);
    sp.button_padding = egui::vec2(10.0, 5.0);
    sp.window_margin = egui::Margin::same(14);
    sp.menu_margin = egui::Margin::same(8);
    sp.interact_size.y = 28.0;

    let fonts = &mut style.text_styles;
    fonts.insert(TextStyle::Heading, FontId::proportional(22.0));
    fonts.insert(TextStyle::Body, FontId::proportional(14.5));
    fonts.insert(TextStyle::Button, FontId::proportional(14.0));
    fonts.insert(TextStyle::Small, FontId::proportional(12.0));
}

/// The raw colors of one mode. Kept private; widgets read `ui.visuals()`.
struct Palette {
    accent: Color32,
    panel: Color32,
    window: Color32,
    field: Color32,
    faint: Color32,
    control: Color32,
    control_hover: Color32,
    control_active: Color32,
    border: Color32,
    text: Color32,
    text_strong: Color32,
    warn: Color32,
}

impl Palette {
    fn new(dark: bool) -> Self {
        if dark {
            Self {
                accent: Color32::from_rgb(124, 137, 255),
                panel: Color32::from_rgb(20, 22, 28),
                window: Color32::from_rgb(28, 31, 39),
                field: Color32::from_rgb(14, 16, 20),
                faint: Color32::from_rgb(25, 28, 35),
                control: Color32::from_rgb(38, 42, 52),
                control_hover: Color32::from_rgb(48, 53, 66),
                control_active: Color32::from_rgb(58, 64, 80),
                border: Color32::from_rgb(48, 53, 64),
                text: Color32::from_rgb(200, 206, 216),
                text_strong: Color32::from_rgb(244, 246, 250),
                warn: Color32::from_rgb(240, 180, 90),
            }
        } else {
            Self {
                accent: Color32::from_rgb(70, 86, 220),
                panel: Color32::from_rgb(243, 244, 247),
                window: Color32::WHITE,
                field: Color32::WHITE,
                faint: Color32::from_rgb(249, 250, 252),
                control: Color32::WHITE,
                control_hover: Color32::from_rgb(238, 240, 246),
                control_active: Color32::from_rgb(226, 231, 242),
                border: Color32::from_rgb(218, 222, 230),
                text: Color32::from_rgb(52, 58, 72),
                text_strong: Color32::from_rgb(20, 24, 34),
                warn: Color32::from_rgb(176, 110, 20),
            }
        }
    }
}

/// A rounded, softly outlined container for grouped content.
pub fn card(ui: &egui::Ui) -> egui::Frame {
    let v = ui.visuals();
    egui::Frame::new()
        .fill(v.window_fill)
        .stroke(Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(14.0)
}

/// Stable color for a sender, so the same person always gets the same avatar.
fn hue_index(name: &str) -> usize {
    name.to_lowercase()
        .bytes()
        .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(usize::from(b)))
        % AVATAR_HUES.len()
}

/// Draw a colored circle with the sender's initial inside `rect`.
pub fn paint_avatar(painter: &Painter, rect: Rect, name: &str) {
    let radius = rect.width().min(rect.height()) / 2.0;
    painter.circle_filled(rect.center(), radius, AVATAR_HUES[hue_index(name)]);
    let initial = name
        .chars()
        .find(|c| c.is_alphabetic())
        .map_or('?', |c| c.to_uppercase().next().unwrap_or(c));
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(radius * 0.9),
        Color32::WHITE,
    );
}

/// The app mark: a rounded accent square with a "G".
pub fn logo(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let accent = ui.visuals().hyperlink_color;
    ui.painter().rect_filled(rect, size * 0.28, accent);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        "G",
        FontId::proportional(size * 0.56),
        Color32::WHITE,
    );
}
