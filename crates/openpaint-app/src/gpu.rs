//! Minimal wgpu setup for OpenPaint.
//!
//! Phase 0: stand up the GPU, configure the surface, hold the tiled canvas and
//! its renderer, and draw the canvas fitted in the window. Painting is driven
//! from `main.rs` via [`Gpu::stroke_begin`] / [`Gpu::stroke_to`] / [`Gpu::stroke_end`].

use std::sync::Arc;

use openpaint_core::{Brush, Canvas, StrokeState};
use winit::window::Window;

use crate::canvas_renderer::CanvasRenderer;
use crate::input::PenSample;

/// Fixed test-bed canvas size for the Phase 0 slice. The real document/page
/// model (and growable/webtoon canvases) arrives in Phase 2.
const CANVAS_W: u32 = 2048;
const CANVAS_H: u32 = 2048;

/// Owns everything needed to draw to the window's surface.
pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: winit::dpi::PhysicalSize<u32>,

    canvas: Canvas,
    canvas_renderer: CanvasRenderer,
    brush: Brush,
    stroke: StrokeState,
    /// Dabs emitted by the brush this update, reused to avoid per-stroke
    /// allocation. Lives between dab emission and rasterization -- see
    /// `Gpu::flush_dabs`.
    dabs: Vec<openpaint_core::Dab>,
    /// Per-stroke paint accumulation and pre-stroke tile snapshot. This is what
    /// makes flow build up toward the opacity ceiling instead of darkening on
    /// every overlap; see `openpaint_core::stroke`.
    painter: openpaint_core::StrokePainter,
    /// Throwaway debug panel (see `crate::ui`). Owned here because it draws into
    /// the same frame and edits the brush.
    ui: crate::ui::Ui,
    drawing: bool,

    window: Arc<Window>,
}

impl Gpu {
    /// Create the GPU context for a window. Blocks on async setup via pollster.
    pub fn new(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| format!("create_surface failed: {e}"))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| "no suitable GPU adapter found".to_string())?;

        let info = adapter.get_info();
        println!(
            "GPU: {} ({:?}) via {:?}",
            info.name, info.device_type, info.backend
        );

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("openpaint-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .map_err(|e| format!("request_device failed: {e}"))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let canvas = Canvas::new(CANVAS_W, CANVAS_H);
        let canvas_renderer = CanvasRenderer::new(&device, &queue, format, &canvas);
        let ui = crate::ui::Ui::new(&device, format, &window);
        canvas_renderer.update_placement(&queue, config.width, config.height);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            size,
            canvas,
            canvas_renderer,
            brush: Brush::default(),
            stroke: StrokeState::new(),
            dabs: Vec::new(),
            painter: openpaint_core::StrokePainter::new(),
            ui,
            drawing: false,
            window,
        })
    }

    /// Handle a window resize by reconfiguring the surface and re-fitting the
    /// canvas placement.
    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.size = new_size;
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);
        self.canvas_renderer
            .update_placement(&self.queue, self.config.width, self.config.height);
    }

    pub fn reconfigure(&mut self) {
        self.surface.configure(&self.device, &self.config);
    }

    // --- Painting -----------------------------------------------------------

    /// Map a window position to canvas space using the renderer's placement.
    fn to_canvas(&self, px: f64, py: f64) -> Option<(f32, f32)> {
        self.canvas_renderer
            .screen_to_canvas(px, py, self.config.width, self.config.height)
    }

    /// Accumulate whatever the brush just emitted and re-composite, then clear
    /// the dab buffer.
    ///
    /// This is the per-dab / per-pixel seam (see `openpaint_core::dab`): the
    /// brush produced dabs without touching a pixel. Paint accumulates per
    /// stroke, so overlapping dabs build toward `brush.opacity` rather than
    /// darkening each time. When dab rasterization moves to the GPU, this is the
    /// only call site that changes.
    fn flush_dabs(&mut self) {
        self.painter.add_dabs(&self.canvas, &self.dabs);
        self.dabs.clear();
        self.painter.composite(
            &mut self.canvas,
            self.brush.color_linear_premul(),
            self.brush.opacity,
        );
        self.window.request_redraw();
    }

    /// Begin a stroke at a pen sample (if it lands on the canvas).
    pub fn stroke_begin(&mut self, s: &PenSample) {
        if self.ui_blocks_point(s.x, s.y) {
            return;
        }
        if let Some((cx, cy)) = self.to_canvas(s.x, s.y) {
            // A fresh stroke resets accumulation, so its opacity ceiling is
            // independent of the previous stroke's.
            self.painter.begin();
            self.brush
                .stroke_begin(&mut self.dabs, &mut self.stroke, cx, cy, s.pressure);
            self.drawing = true;
            self.flush_dabs();
        }
    }

    /// Continue the current stroke to a new pen sample.
    pub fn stroke_to(&mut self, s: &PenSample) {
        if !self.drawing {
            return;
        }
        if let Some((cx, cy)) = self.to_canvas(s.x, s.y) {
            self.brush
                .stroke_to(&mut self.dabs, &mut self.stroke, cx, cy, s.pressure);
            self.flush_dabs();
        }
    }

    /// End the current stroke.
    pub fn stroke_end(&mut self) {
        self.drawing = false;
        self.stroke = StrokeState::new();
    }

    // --- Rendering ----------------------------------------------------------

    /// Draw one frame: upload dirty tiles, clear the backdrop, draw the canvas.
    pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        self.canvas_renderer
            .upload_dirty(&self.queue, &mut self.canvas);

        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("canvas-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Neutral gray backdrop framing the canvas sheet.
                        // LINEAR values: the surface is an *Srgb format, so wgpu
                        // encodes on write. These are sRGB 0.16/0.16/0.17 taken
                        // through the transfer function -- passing the sRGB
                        // numbers directly would render a washed-out mid-grey.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.021_9,
                            g: 0.021_9,
                            b: 0.024_7,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.canvas_renderer.draw(&mut pass);
        }

        // Panel draws over the canvas, in the same frame.
        let surface_size = [self.config.width, self.config.height];
        let ui_wants_repaint = self.ui.render(
            &self.window,
            &self.device,
            &self.queue,
            &mut encoder,
            &view,
            surface_size,
            &mut self.brush,
        );
        if ui_wants_repaint {
            self.window.request_redraw();
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }

    /// Offer a window event to the debug panel first. Returns `true` if the panel
    /// consumed it, in which case it must not be treated as canvas input.
    pub fn ui_handled_event(&mut self, event: &winit::event::WindowEvent) -> bool {
        let consumed = self.ui.on_window_event(&self.window, event);
        // Always request a frame, not just when egui consumed the event. egui
        // processes queued input during `render`, so with demand-driven painting
        // it cannot react to anything unless a frame follows -- including
        // deciding whether to consume the *next* event. Gating this on `consumed`
        // deadlocks the panel: input arrives, no frame runs, nothing is ever
        // consumed, so no frame is ever requested.
        self.window.request_redraw();
        consumed
    }

    /// Whether a window-space point is over the panel rather than the canvas.
    ///
    /// Needed for the pen specifically: pen input never reaches egui, so egui's
    /// own pointer capture cannot exclude it (see `crate::ui`).
    pub fn ui_blocks_point(&self, x: f64, y: f64) -> bool {
        self.ui.blocks_point(x, y)
    }

    pub fn window(&self) -> &Window {
        &self.window
    }
}
