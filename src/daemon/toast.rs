//! The bottom-center "Copied" toast: a click-through layer-shell surface
//! rendered on the CPU with tiny-skia + cosmic-text.

use std::time::{Duration, Instant};

use cosmic_text::{Attrs, Buffer, Color as TextColor, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::calloop::{
        RegistrationToken,
        timer::{TimeoutAction, Timer},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};
use wayland_client::{
    Connection, QueueHandle,
    protocol::{wl_output, wl_shm, wl_surface},
};

use super::Daemon;

/// Logical size of the (mostly transparent) toast surface.
const SURFACE_W: u32 = 720;
const SURFACE_H: u32 = 64;
const PILL_H: f32 = 40.0;
const PAD_X: f32 = 18.0;
const DOT_R: f32 = 4.0;
const DOT_GAP: f32 = 12.0;
const FADE_IN: Duration = Duration::from_millis(120);
const FADE_OUT: Duration = Duration::from_millis(240);
const FRAME: Duration = Duration::from_millis(16);

#[derive(Debug, Clone, Copy)]
pub enum ToastKind {
    Copied,
    Note,
    Info,
    Error,
}

impl ToastKind {
    fn accent(self) -> (u8, u8, u8) {
        match self {
            ToastKind::Copied => (61, 174, 233), // Breeze blue
            ToastKind::Note => (39, 174, 96),    // green
            ToastKind::Info => (160, 164, 172),  // grey
            ToastKind::Error => (218, 68, 83),   // red
        }
    }
}

struct Content {
    kind: ToastKind,
    title: String,
    body: Option<String>,
}

struct ToastSurface {
    layer: LayerSurface,
    pool: SlotPool,
    configured: bool,
    scale: i32,
}

pub struct Toast {
    font_system: Option<FontSystem>,
    swash: SwashCache,
    surface: Option<ToastSurface>,
    content: Option<Content>,
    started: Option<Instant>,
    alpha: f32,
    timer: Option<RegistrationToken>,
}

impl Toast {
    pub fn new() -> Self {
        Self {
            font_system: None, // loaded lazily: scanning fonts takes a moment
            swash: SwashCache::new(),
            surface: None,
            content: None,
            started: None,
            alpha: 0.0,
            timer: None,
        }
    }
}

impl Daemon {
    pub fn show_toast(&mut self, kind: ToastKind, title: &str, body: Option<String>) {
        let visible = self.toast.surface.is_some() && self.toast.started.is_some();
        self.toast.content = Some(Content { kind, title: title.to_string(), body });
        let now = Instant::now();
        // Already on screen: swap the text and restart the hold, no fade-in.
        self.toast.started = Some(if visible { now.checked_sub(FADE_IN).unwrap_or(now) } else { now });

        if self.toast.surface.is_none() {
            self.create_toast_surface();
        }
        if let Some(token) = self.toast.timer.take() {
            self.loop_handle.remove(token);
        }
        self.toast.timer = self
            .loop_handle
            .insert_source(Timer::immediate(), |_, _, state: &mut Daemon| state.toast_tick())
            .ok();
    }

    fn create_toast_surface(&mut self) {
        let surface = self.compositor.create_surface(&self.qh);
        // Empty input region: clicks go straight through the toast.
        match Region::new(&self.compositor) {
            Ok(region) => surface.set_input_region(Some(region.wl_region())),
            Err(e) => log::warn!("input region: {e}"),
        }
        let scale = self
            .output_state
            .outputs()
            .filter_map(|o| self.output_state.info(&o))
            .map(|info| info.scale_factor)
            .max()
            .unwrap_or(1)
            .max(1);

        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Overlay,
            Some("collage-toast"),
            None,
        );
        layer.set_anchor(Anchor::BOTTOM);
        layer.set_size(SURFACE_W, SURFACE_H);
        layer.set_margin(0, 0, self.cfg.toast_bottom_margin, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(0);
        layer.commit();

        let bytes = (SURFACE_W * SURFACE_H * 4) as usize * (scale * scale) as usize;
        match SlotPool::new(bytes, &self.shm) {
            Ok(pool) => {
                self.toast.surface = Some(ToastSurface { layer, pool, configured: false, scale })
            }
            Err(e) => log::warn!("toast buffer pool: {e}"),
        }
    }

    fn hide_toast(&mut self) {
        self.toast.surface = None; // dropping the LayerSurface destroys it
        self.toast.content = None;
        self.toast.started = None;
    }

    fn toast_tick(&mut self) -> TimeoutAction {
        let Some(started) = self.toast.started else {
            self.toast.timer = None;
            return TimeoutAction::Drop;
        };
        let hold = Duration::from_millis(self.cfg.toast_duration_ms);
        let t = started.elapsed();
        let (alpha, next) = if t < FADE_IN {
            (t.as_secs_f32() / FADE_IN.as_secs_f32(), Some(FRAME))
        } else if t < FADE_IN + hold {
            (1.0, Some(FADE_IN + hold - t))
        } else if t < FADE_IN + hold + FADE_OUT {
            let out = (t - FADE_IN - hold).as_secs_f32() / FADE_OUT.as_secs_f32();
            (1.0 - out, Some(FRAME))
        } else {
            (0.0, None)
        };
        match next {
            Some(next) => {
                self.toast.alpha = ease(alpha);
                self.draw_toast();
                TimeoutAction::ToDuration(next)
            }
            None => {
                self.hide_toast();
                self.toast.timer = None;
                TimeoutAction::Drop
            }
        }
    }

    fn draw_toast(&mut self) {
        let font_size = self.cfg.toast_font_size;
        let Toast { font_system, swash, surface, content, alpha, .. } = &mut self.toast;
        let (Some(surf), Some(content)) = (surface.as_mut(), content.as_ref()) else { return };
        if !surf.configured {
            return;
        }
        let fonts = font_system.get_or_insert_with(FontSystem::new);
        let scale = surf.scale.max(1);
        let (w, h) = (SURFACE_W * scale as u32, SURFACE_H * scale as u32);
        let Some(pixmap) = render(fonts, swash, content, *alpha, scale as f32, w, h, font_size) else {
            return;
        };

        let (buffer, canvas) = match surf.pool.create_buffer(
            w as i32,
            h as i32,
            (w * 4) as i32,
            wl_shm::Format::Argb8888,
        ) {
            Ok(b) => b,
            Err(e) => return log::warn!("toast buffer: {e}"),
        };
        // tiny-skia is premultiplied RGBA; wl_shm ARGB8888 is BGRA in memory.
        for (dst, src) in canvas.chunks_exact_mut(4).zip(pixmap.data().chunks_exact(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }
        let wl = surf.layer.wl_surface();
        wl.set_buffer_scale(scale);
        wl.damage_buffer(0, 0, w as i32, h as i32);
        if let Err(e) = buffer.attach_to(wl) {
            return log::warn!("toast attach: {e}");
        }
        surf.layer.commit();
    }
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[allow(clippy::too_many_arguments)]
fn render(
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    content: &Content,
    alpha: f32,
    s: f32,
    w: u32,
    h: u32,
    font_size: f32,
) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(w, h)?;
    let (ar, ag, ab) = content.kind.accent();

    // Text layout (single line, no wrapping).
    let size = font_size * s;
    let metrics = Metrics::new(size, (size * 1.3).round());
    let mut buffer = Buffer::new(fonts, metrics);
    buffer.set_size(fonts, None, None);
    let title_attrs = Attrs::new()
        .family(Family::SansSerif)
        .weight(Weight::SEMIBOLD)
        .color(TextColor::rgb(255, 255, 255));
    let body_attrs = Attrs::new()
        .family(Family::SansSerif)
        .color(TextColor::rgb(200, 203, 210));
    let mut spans = vec![(content.title.as_str(), title_attrs.clone())];
    if let Some(body) = content.body.as_deref().filter(|b| !b.is_empty()) {
        spans.push(("   ", body_attrs.clone()));
        spans.push((body, body_attrs.clone()));
    }
    buffer.set_rich_text(fonts, spans, &body_attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(fonts, false);
    let text_w = buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);

    // Pill geometry, centered in the surface.
    let pill_h = PILL_H * s;
    let lead = PAD_X * s + DOT_R * 2.0 * s + DOT_GAP * s;
    let max_w = w as f32 - 24.0 * s;
    let pill_w = (lead + text_w + PAD_X * s).min(max_w);
    let x0 = ((w as f32 - pill_w) / 2.0).round();
    let y0 = ((h as f32 - pill_h) / 2.0).round();
    let radius = pill_h / 2.0;

    let mut paint = Paint { anti_alias: true, ..Paint::default() };

    // Soft shadow.
    for i in 1..=4 {
        let d = i as f32 * 1.5 * s;
        paint.set_color_rgba8(0, 0, 0, (14.0 * alpha) as u8);
        if let Some(path) = rounded_rect(x0 - d, y0 - d + 2.0 * s, pill_w + 2.0 * d, pill_h + 2.0 * d, radius + d) {
            pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }
    // Background + hairline border.
    paint.set_color_rgba8(255, 255, 255, (22.0 * alpha) as u8);
    if let Some(path) = rounded_rect(x0 - s, y0 - s, pill_w + 2.0 * s, pill_h + 2.0 * s, radius + s) {
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
    paint.set_color_rgba8(32, 34, 40, (242.0 * alpha) as u8);
    if let Some(path) = rounded_rect(x0, y0, pill_w, pill_h, radius) {
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
    // Accent dot.
    paint.set_color_rgba8(ar, ag, ab, (255.0 * alpha) as u8);
    let dot_cx = x0 + PAD_X * s + DOT_R * s;
    if let Some(path) = PathBuilder::from_circle(dot_cx, y0 + pill_h / 2.0, DOT_R * s) {
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }

    // Glyphs, clipped to the pill's inner area.
    let tx = (x0 + lead).round() as i32;
    let ty = (y0 + (pill_h - metrics.line_height) / 2.0).round() as i32;
    let clip_right = (x0 + pill_w - PAD_X * s * 0.6) as i32;
    let (pw, ph) = (w as i32, h as i32);
    let data = pixmap.data_mut();
    buffer.draw(fonts, swash, TextColor::rgb(255, 255, 255), |gx, gy, gw, gh, color| {
        let a = (color.a() as f32 * alpha) as u32;
        if a == 0 {
            return;
        }
        for py in (ty + gy)..(ty + gy + gh as i32) {
            for px in (tx + gx)..(tx + gx + gw as i32) {
                if px < 0 || py < 0 || px >= pw || py >= ph || px >= clip_right {
                    continue;
                }
                let i = ((py * pw + px) * 4) as usize;
                let inv = 255 - a;
                for (k, c) in [color.r(), color.g(), color.b()].into_iter().enumerate() {
                    let src = c as u32 * a / 255;
                    data[i + k] = (src + data[i + k] as u32 * inv / 255) as u8;
                }
                data[i + 3] = (a + data[i + 3] as u32 * inv / 255) as u8;
            }
        }
    });
    Some(pixmap)
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = r * 0.552_284_8; // cubic approximation of a quarter circle
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

// ---- sctk handlers ----------------------------------------------------------

impl CompositorHandler for Daemon {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, factor: i32) {
        if let Some(surf) = self.toast.surface.as_mut()
            && surf.layer.wl_surface() == surface
            && surf.scale != factor
        {
            surf.scale = factor;
            self.draw_toast();
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl LayerShellHandler for Daemon {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.hide_toast();
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, _: LayerSurfaceConfigure, _: u32) {
        if let Some(surf) = self.toast.surface.as_mut()
            && &surf.layer == layer
        {
            surf.configured = true;
            self.draw_toast();
        }
    }
}

impl OutputHandler for Daemon {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Daemon {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Daemon);
delegate_output!(Daemon);
delegate_shm!(Daemon);
delegate_layer!(Daemon);
