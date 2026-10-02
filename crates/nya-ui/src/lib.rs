//! Shared GUI layer: egui drawn with Direct3D 11 (works on machines without
//! usable OpenGL, e.g. cloud desktops), Chinese fonts and a common theme.

mod painter;
mod surface;

use std::time::Duration;

use anyhow::Result;
pub use egui;
pub use egui_winit;
use egui_winit::EventResponse;
pub use painter::Painter;
pub use surface::Surface;
use windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView;
use winit::event::WindowEvent;
use winit::window::Window;

use nya_win::d3d::D3dDevice;

/// Output of one UI frame, ready to paint.
pub struct FrameOutput {
    pub primitives: Vec<egui::ClippedPrimitive>,
    pub textures: egui::TexturesDelta,
    pub pixels_per_point: f32,
    /// When egui wants the next frame (`Duration::ZERO` = as soon as possible).
    pub repaint_after: Duration,
}

/// egui context + winit integration + D3D11 painter for one window.
pub struct Gui {
    pub ctx: egui::Context,
    state: egui_winit::State,
    painter: Painter,
}

impl Gui {
    pub fn new(window: &Window, dev: &D3dDevice) -> Result<Self> {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        apply_theme(&ctx);
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            None,
            Some(8192),
        );
        Ok(Self { ctx, state, painter: Painter::new(dev)? })
    }

    /// The painter is tied to a device; call after the device was recreated.
    pub fn set_device(&mut self, dev: &D3dDevice) -> Result<()> {
        self.painter = Painter::new(dev)?;
        // Textures lived on the old device: make egui upload everything again.
        self.ctx.forget_all_images();
        self.ctx.memory_mut(|m| *m = Default::default());
        Ok(())
    }

    pub fn on_event(&mut self, window: &Window, event: &WindowEvent) -> EventResponse {
        self.state.on_window_event(window, event)
    }

    pub fn run(&mut self, window: &Window, ui: impl FnMut(&egui::Context)) -> FrameOutput {
        let input = self.state.take_egui_input(window);
        let out = self.ctx.run(input, ui);
        self.state.handle_platform_output(window, out.platform_output);
        let repaint_after = out
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(Duration::MAX);
        FrameOutput {
            primitives: self.ctx.tessellate(out.shapes, out.pixels_per_point),
            textures: out.textures_delta,
            pixels_per_point: out.pixels_per_point,
            repaint_after,
        }
    }

    pub fn paint(&mut self, rtv: &ID3D11RenderTargetView, size: (u32, u32), frame: &FrameOutput) -> Result<()> {
        self.painter.paint(rtv, size, frame.pixels_per_point, &frame.primitives, &frame.textures)
    }

    /// egui wants the pointer / keyboard (a widget is hovered or focused).
    pub fn wants_input(&self) -> bool {
        self.ctx.wants_pointer_input() || self.ctx.wants_keyboard_input()
    }
}

/// Add fallback fonts from Windows: a CJK font (Microsoft YaHei, falling back
/// to others), then Segoe UI Symbol for symbols neither egui's fonts nor the
/// CJK font have (▾ ⋯ ✕ ✓ …, otherwise drawn as boxes).
pub fn install_fonts(ctx: &egui::Context) {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let dir = std::path::Path::new(&windir).join("Fonts");
    let mut fonts = egui::FontDefinitions::default();
    let mut add = |key: &str, candidates: &[&str]| {
        let Some(bytes) = candidates.iter().find_map(|n| std::fs::read(dir.join(n)).ok()) else { return false };
        fonts.font_data.insert(key.into(), egui::FontData::from_owned(bytes).into());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(key.into());
        }
        true
    };
    if !add("cjk", &["msyh.ttc", "msyh.ttf", "Deng.ttf", "simhei.ttf", "simsun.ttc"]) {
        tracing::warn!("no CJK font found under {}; Chinese text may not render", dir.display());
    }
    if !add("symbols", &["seguisym.ttf"]) {
        tracing::warn!("Segoe UI Symbol not found under {}; some symbols may not render", dir.display());
    }
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    #[test]
    fn symbols_used_by_the_ui_have_glyphs() {
        let ctx = egui::Context::default();
        super::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        let id = egui::FontId::proportional(14.0);
        // Only meaningful where the Windows fonts exist (not on bare CI images).
        if !std::path::Path::new(r"C:\Windows\Fonts\seguisym.ttf").exists() {
            return;
        }
        let missing: String = ctx.fonts(|f| "▾▸⋯✕✓…·→↑↓—×＋".chars().filter(|&c| !f.has_glyph(&id, c)).collect());
        assert!(missing.is_empty(), "no glyph for {missing}");
    }
}

/// Dark theme matching the web pages (common/web/src/lib/theme.css).
pub fn apply_theme(ctx: &egui::Context) {
    use egui::{Color32, CornerRadius, Shadow, Stroke};
    let rgb = Color32::from_rgb;
    let accent = rgb(0xff, 0x78, 0x96);
    let line = rgb(0x2d, 0x30, 0x38);
    let mut v = egui::Visuals::dark();
    v.window_fill = rgb(0x1e, 0x20, 0x26);
    v.panel_fill = v.window_fill;
    v.faint_bg_color = rgb(0x25, 0x27, 0x2e);
    v.extreme_bg_color = rgb(0x16, 0x17, 0x1b);
    v.window_stroke = Stroke::new(1.0_f32, line);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow { offset: [0, 8], blur: 32, spread: 0, color: Color32::from_black_alpha(110) };
    v.popup_shadow = Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(100) };
    v.hyperlink_color = accent;
    v.selection.bg_fill = rgb(0x7a, 0x33, 0x48);
    v.selection.stroke = Stroke::new(1.0_f32, rgb(0xff, 0xb3, 0xc4));
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, line);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, rgb(0xa3, 0xa9, 0xb4));
    for (w, fill) in [
        (&mut v.widgets.inactive, rgb(0x2a, 0x2c, 0x33)),
        (&mut v.widgets.hovered, rgb(0x36, 0x39, 0x42)),
        (&mut v.widgets.active, rgb(0x40, 0x43, 0x4d)),
        (&mut v.widgets.open, rgb(0x36, 0x39, 0x42)),
    ] {
        w.corner_radius = CornerRadius::same(8);
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::NONE;
    }
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, rgb(0xd7, 0xda, 0xe0));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.menu_margin = egui::Margin::same(6);
        s.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.5));
        s.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.5));
        s.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
        s.text_styles.insert(egui::TextStyle::Heading, egui::FontId::proportional(20.0));
    });
}
