//! GPU context and frame presentation.
//!
//! Deliberately narrow: this owns wgpu resources and draws what it is given. It
//! does **not** own the document, the brush, the stroke state, or the UI — those
//! are [`crate::editor::Editor`], [`crate::view::View`], and `crate::ui`
//! respectively.
//!
//! That separation is not tidiness for its own sake. This type previously held all
//! of them, which meant every new feature (pan/zoom, pages, layers, undo) would
//! have landed on one struct that was simultaneously the renderer, the document
//! owner, and the stroke state machine.
//!
//! # The UI seam
//!
//! Anything drawing on top of the canvas arrives as a closure taking an
//! [`Overlay`], so this module has no dependency on egui or on any UI library.
//! Swapping the UI (Q4 is still open, and egui is explicitly throwaway) touches
//! the callback, not the renderer.

use std::sync::Arc;

use openpaint_core::Canvas;
use winit::window::Window;

use crate::canvas_renderer::{CanvasRenderer, CANVAS_FORMAT};
use crate::editor::StrokeOp;
use crate::history::{self, Entry, History};
use crate::stroke_layer::StrokeLayer;
use crate::view::Placement;

/// Bytes per canvas texel (`Rgba16Float`), for history's memory accounting.
const CANVAS_BYTES_PER_TEXEL: usize = 8;

/// Everything an overlay needs to draw itself into the current frame.
pub struct Overlay<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// The frame's color target, already containing the canvas.
    pub target: &'a wgpu::TextureView,
    /// Surface size in physical pixels.
    pub size_px: [u32; 2],
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    canvas_renderer: CanvasRenderer,
    /// GPU dab rasterization and the in-progress stroke.
    stroke_layer: StrokeLayer,
    /// Undo/redo. GPU-side because the GPU owns the pixels (see `crate::history`).
    history: History,
    /// Dabs of the stroke being recorded, accumulated across frames so redo can
    /// replay it. A stroke spans many frames, and each frame's dabs are cleared
    /// once executed, so history has to keep its own copy.
    recording: Vec<openpaint_core::Dab>,
    /// Paint of the stroke being recorded, captured at Begin.
    recording_paint: ([f32; 4], f32),
    window: Arc<Window>,
}

impl Renderer {
    /// Create the GPU context for a window. Blocks on async setup via pollster.
    pub fn new(window: Arc<Window>, canvas: &Canvas) -> Result<Self, String> {
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

        // Default limits (not downlevel-relaxed, not raised): the target is a
        // Surface-class integrated GPU, so staying at defaults keeps us portable.
        // See DECISIONS §2.
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
        // Prefer an sRGB surface so wgpu encodes on write and we can work in
        // linear throughout (DECISIONS §4b).
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

        let canvas_renderer = CanvasRenderer::new(&device, &queue, format, canvas);
        let stroke_layer = StrokeLayer::new(
            &device,
            canvas.width(),
            canvas.height(),
            CANVAS_FORMAT,
            format,
        );

        Ok(Self {
            surface,
            device,
            queue,
            config,
            canvas_renderer,
            stroke_layer,
            history: History::new(CANVAS_BYTES_PER_TEXEL),
            recording: Vec::new(),
            recording_paint: ([0.0; 4], 1.0),
            window,
        })
    }

    #[must_use]
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    #[must_use]
    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Surface size in physical pixels.
    #[must_use]
    pub fn size_px(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn reconfigure(&mut self) {
        self.surface.configure(&self.device, &self.config);
    }

    /// Push the canvas's changed tiles to the GPU.
    ///
    /// Separate from [`Renderer::render`] so the mutable canvas borrow ends before
    /// the frame begins, which keeps the overlay callback free to touch the rest
    /// of the app state.
    pub fn upload_canvas(&mut self, canvas: &mut Canvas) {
        self.canvas_renderer.upload_dirty(&self.queue, canvas);
    }

    /// Execute the editor's pending stroke commands on the GPU.
    ///
    /// Submitted separately from [`Renderer::render`] and before it, so the canvas
    /// texture is already up to date by the time the frame reads it. An extra
    /// submit per frame is immaterial next to avoiding one per input sample.
    pub fn apply_stroke(&mut self, ops: &[StrokeOp], dabs: &[openpaint_core::Dab]) {
        if ops.is_empty() {
            return;
        }

        // Upload every dab once, up front. Per-batch uploads would be clobbered:
        // all of a submission's buffer writes are applied before any of its command
        // buffers execute. See StrokeLayer::upload_dabs.
        self.stroke_layer
            .upload_dabs(&self.device, &self.queue, dabs);

        let mut encoder = self.new_stroke_encoder();

        for op in ops {
            match *op {
                StrokeOp::Begin {
                    color_linear_premul,
                    opacity,
                } => {
                    self.recording.clear();
                    self.recording_paint = (color_linear_premul, opacity);
                    // Paint is a uniform written from the queue, so it has the same
                    // ordering hazard: a second stroke in the same frame would
                    // otherwise overwrite the first one's colour before either drew.
                    // Submitting here keeps each stroke's paint with its own draws.
                    self.submit_stroke_encoder(&mut encoder);
                    self.stroke_layer
                        .set_paint(&self.queue, color_linear_premul, opacity);
                    self.stroke_layer.begin_stroke(&mut encoder);
                }
                StrokeOp::Dabs { start, len } => {
                    if start + len <= dabs.len() {
                        self.recording.extend_from_slice(&dabs[start..start + len]);
                        self.stroke_layer.stamp_range(&mut encoder, start, len);
                    }
                }
                StrokeOp::End { bounds } => {
                    // Snapshot *before* baking: this is the pre-stroke image undo
                    // restores. A GPU-to-GPU copy of just the touched rectangle, so
                    // nothing comes back to the CPU on the interactive path.
                    if let Some(rect) = bounds {
                        if !rect.is_empty() && !self.recording.is_empty() {
                            let before = history::snapshot_region(
                                &self.device,
                                &mut encoder,
                                self.canvas_renderer.texture(),
                                rect,
                            );
                            let (color_linear_premul, opacity) = self.recording_paint;
                            self.history.push(Entry {
                                rect,
                                before,
                                dabs: std::mem::take(&mut self.recording),
                                color_linear_premul,
                                opacity,
                            });
                        }
                    }
                    self.stroke_layer
                        .bake(&mut encoder, self.canvas_renderer.target_view());
                }
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Read the canvas back and write it as an sRGB PNG.
    ///
    /// Stalls on the GPU while the readback maps, which is acceptable for an
    /// explicit user action -- the drawing path never reads back.
    pub fn export_png(&self, path: &std::path::Path) -> Result<(), crate::export::ExportError> {
        let size = self.canvas_renderer.texture().size();
        crate::export::export_canvas_png(
            &self.device,
            &self.queue,
            self.canvas_renderer.texture(),
            size.width,
            size.height,
            path,
        )
    }

    /// Undo and redo depths, and snapshot bytes held, for display.
    #[must_use]
    pub fn history_status(&self) -> (usize, usize, usize) {
        (
            self.history.undo_depth(),
            self.history.redo_depth(),
            self.history.bytes_held(),
        )
    }

    /// Undo the most recent stroke by restoring its before-image.
    ///
    /// Returns `true` if anything changed, so the caller knows to request a frame.
    pub fn undo(&mut self) -> bool {
        let Some(entry) = self.history.pop_undo() else {
            return false;
        };
        let mut encoder = self.new_stroke_encoder();
        history::restore_region(
            &mut encoder,
            &entry.before,
            self.canvas_renderer.texture(),
            entry.rect,
        );
        self.queue.submit(std::iter::once(encoder.finish()));
        // The snapshot is still the *before* image, which is exactly what a later
        // undo of the redone stroke needs -- so it carries over untouched.
        self.history.finish_undo(entry);
        true
    }

    /// Redo by replaying the stroke's dabs, rather than storing an after-image.
    pub fn redo(&mut self) -> bool {
        let Some(entry) = self.history.pop_redo() else {
            return false;
        };

        // Paint is a queue write, so it must be ordered before the draws that use
        // it -- set it, then record in a fresh submission.
        self.stroke_layer
            .set_paint(&self.queue, entry.color_linear_premul, entry.opacity);
        self.stroke_layer
            .upload_dabs(&self.device, &self.queue, &entry.dabs);

        let mut encoder = self.new_stroke_encoder();
        self.stroke_layer.begin_stroke(&mut encoder);
        self.stroke_layer
            .stamp_range(&mut encoder, 0, entry.dabs.len());
        self.stroke_layer
            .bake(&mut encoder, self.canvas_renderer.target_view());
        self.queue.submit(std::iter::once(encoder.finish()));

        self.history.finish_redo(entry);
        true
    }

    fn new_stroke_encoder(&self) -> wgpu::CommandEncoder {
        self.device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("stroke-encoder"),
            })
    }

    /// Submit the work recorded so far and hand back a fresh encoder, so queue
    /// writes that follow are ordered after it.
    fn submit_stroke_encoder(&self, encoder: &mut wgpu::CommandEncoder) {
        let finished = std::mem::replace(encoder, self.new_stroke_encoder());
        self.queue.submit(std::iter::once(finished.finish()));
    }

    /// Draw one frame: clear the backdrop, draw the canvas, then the overlay.
    ///
    /// Takes a already-computed [`Placement`] rather than the canvas: the renderer
    /// has no need for the document (the pixels are already in its texture), and
    /// not borrowing it leaves the caller free to hand the overlay mutable access
    /// to editor state.
    pub fn render(
        &mut self,
        placement: Placement,
        overlay: impl FnOnce(Overlay<'_>),
    ) -> Result<(), wgpu::SurfaceError> {
        self.canvas_renderer.set_placement(&self.queue, placement);

        let frame = self.surface.get_current_texture()?;
        let target = frame
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
                    view: &target,
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
            // The in-progress stroke is not in the canvas texture yet, so it is
            // composited on top for the preview. It uses the same placement, so it
            // pans, zooms, and rotates with the canvas.
            if self.stroke_layer.has_paint() {
                self.stroke_layer
                    .draw_preview(&self.queue, &mut pass, placement);
            }
        }

        overlay(Overlay {
            device: &self.device,
            queue: &self.queue,
            encoder: &mut encoder,
            target: &target,
            size_px: [self.config.width, self.config.height],
        });

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }

    #[must_use]
    pub fn window(&self) -> &Arc<Window> {
        &self.window
    }
}
