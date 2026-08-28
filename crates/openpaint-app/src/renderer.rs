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
use crate::stroke_layer::StrokeLayer;
use crate::view::Placement;

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
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("stroke-encoder"),
            });

        for op in ops {
            match *op {
                StrokeOp::Begin {
                    color_linear_premul,
                    opacity,
                } => {
                    self.stroke_layer.begin_stroke(&mut encoder);
                    self.stroke_layer
                        .set_paint(&self.queue, color_linear_premul, opacity);
                }
                StrokeOp::Dabs { start, len } => {
                    let Some(slice) = dabs.get(start..start + len) else {
                        continue;
                    };
                    self.stroke_layer
                        .add_dabs(&self.device, &self.queue, &mut encoder, slice);
                }
                StrokeOp::End => {
                    self.stroke_layer
                        .bake(&mut encoder, self.canvas_renderer.target_view());
                }
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
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
