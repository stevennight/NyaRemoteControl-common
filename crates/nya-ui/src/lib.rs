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

/// Add a CJK font from Windows (Microsoft YaHei, falling back to others).
pub fn install_fonts(ctx: &egui::Context) {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let candidates = ["msyh.ttc", "msyh.ttf", "Deng.ttf", "simhei.ttf", "simsun.ttc"];
    let mut fonts = egui::FontDefinitions::default();
    for name in candidates {
        let path = std::path::Path::new(&windir).join("Fonts").join(name);
        if let Ok(bytes) = std::fs::read(&path) {
            fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".into());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
    tracing::warn!("no CJK font found under {windir}\\Fonts; Chinese text may not render");
}

pub fn apply_theme(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::dark());
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
        s.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
        s.text_styles.insert(egui::TextStyle::Heading, egui::FontId::proportional(22.0));
    });
}
