//! Throwaway egui debug panel for engine work.
//!
//! Deliberately temporary. `docs/DECISIONS.md` §3 keeps the real CSP-like UI as a
//! later, reversible decision, and nothing in `openpaint-core` knows this exists.
//!
//! It is not decoration: a soft round brush **cannot** be tuned to match
//! Photoshop's (docs Q7a) without live sliders. Flow and opacity in particular
//! have correct semantics but were unreachable until now, so their behavior could
//! only be verified by unit test, never felt.
//!
//! # Known limitation: the pen cannot operate this UI
//!
//! Pen input arrives through octotablet, which bypasses winit's event stream
//! entirely, so egui never sees it — only the mouse can click these widgets. Fine
//! for a debug panel, but it is a real architectural item for the eventual UI:
//! pen events will need routing into the UI layer as well as the canvas. Tracked
//! in OPEN_QUESTIONS Q14.
//!
//! To stop strokes landing "through" the panel, [`Ui::blocks_point`] reports the
//! region egui occupies and the caller skips painting there. That check is needed
//! for the pen specifically, because egui's own pointer-capture logic never sees
//! it.

use egui::ViewportId;
use openpaint_core::Brush;
use winit::window::Window;

/// Width of the side panel in logical points.
const PANEL_WIDTH: f32 = 280.0;

pub struct Ui {
    ctx: egui::Context,
    state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    /// Screen-space rect (physical pixels) egui is currently occupying, so canvas
    /// input can be excluded from it.
    occupied: egui::Rect,
}

impl Ui {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        window: &Window,
    ) -> Self {
        let ctx = egui::Context::default();
        let state = egui_winit::State::new(
            ctx.clone(),
            ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        // `dithering: true` matters more than it sounds: the surface is 8-bit
        // sRGB, and egui's gradients band visibly without it.
        let renderer = egui_wgpu::Renderer::new(device, surface_format, None, 1, true);
        Self {
            ctx,
            state,
            renderer,
            occupied: egui::Rect::NOTHING,
        }
    }

    /// Feed a window event to egui. Returns `true` if egui consumed it, in which
    /// case the caller should not treat it as canvas input.
    pub fn on_window_event(&mut self, window: &Window, event: &winit::event::WindowEvent) -> bool {
        self.state.on_window_event(window, event).consumed
    }

    /// Whether a point in physical window pixels lies over the panel.
    ///
    /// Used to keep pen strokes from painting underneath the UI. egui's own
    /// pointer handling cannot do this for us because it never sees pen input.
    pub fn blocks_point(&self, x: f64, y: f64) -> bool {
        self.occupied.contains(egui::pos2(x as f32, y as f32))
    }

    /// Build the panel, render it over the frame, and apply any edits to `brush`.
    ///
    /// Returns `true` if egui wants another frame soon (an animation, a hover
    /// fade, a drag in progress). The caller **must** honor it: painting is
    /// demand-driven, and egui is only interactive while frames keep coming.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        window: &Window,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        surface_size: [u32; 2],
        brush: &mut Brush,
    ) -> bool {
        let input = self.state.take_egui_input(window);
        let mut color_srgb = brush.color_srgb8();

        let output = self.ctx.run(input, |ctx| {
            egui::SidePanel::left("brush-panel")
                .exact_width(PANEL_WIDTH)
                .show(ctx, |ui| {
                    ui.heading("Brush");
                    ui.add_space(4.0);

                    ui.add(
                        egui::Slider::new(&mut brush.radius, 0.5..=400.0)
                            .logarithmic(true)
                            .text("Size (radius px)"),
                    );
                    ui.add(
                        egui::Slider::new(&mut brush.hardness, 0.0..=1.0)
                            .text("Hardness")
                            .custom_formatter(|v, _| {
                                // Name the ends, because a bare number gives no
                                // clue which way round it goes.
                                match v {
                                    v if v <= 0.001 => "0.00 soft".to_owned(),
                                    v if v >= 0.999 => "1.00 hard".to_owned(),
                                    v => format!("{v:.2}"),
                                }
                            }),
                    );
                    ui.add(egui::Slider::new(&mut brush.spacing, 0.01..=1.0).text("Spacing"));

                    ui.separator();
                    ui.add(egui::Slider::new(&mut brush.flow, 0.0..=1.0).text("Flow"));
                    ui.add(egui::Slider::new(&mut brush.opacity, 0.0..=1.0).text("Opacity"));
                    ui.label(
                        egui::RichText::new(
                            "Flow = paint per dab. Opacity = ceiling for the whole \
                             stroke. Set flow low and opacity mid to see build-up \
                             stop at the ceiling; lift and stroke again to go darker.",
                        )
                        .small()
                        .weak(),
                    );

                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("Color");
                        ui.color_edit_button_srgb(&mut color_srgb);
                    });

                    ui.separator();
                    if ui.button("Reset to defaults").clicked() {
                        *brush = Brush::default();
                        color_srgb = brush.color_srgb8();
                    }

                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Spacing is a fraction of diameter (Photoshop ~0.25), so \n                             dabs land every {:.2} px.",
                            brush.radius * 2.0 * brush.spacing
                        ))
                        .small()
                        .weak(),
                    );
                });
        });

        brush.set_color_srgb8(color_srgb);

        // Record which pixels egui owns, in physical coordinates, for
        // `blocks_point`. `used_rect` is in logical points.
        let scale = self.ctx.pixels_per_point();
        let used = self.ctx.used_rect();
        self.occupied = egui::Rect::from_min_max(
            egui::pos2(used.min.x * scale, used.min.y * scale),
            egui::pos2(used.max.x * scale, used.max.y * scale),
        );

        self.state
            .handle_platform_output(window, output.platform_output);

        let tris = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        for (id, delta) in &output.textures_delta.set {
            self.renderer.update_texture(device, queue, *id, delta);
        }
        let desc = egui_wgpu::ScreenDescriptor {
            size_in_pixels: surface_size,
            pixels_per_point: output.pixels_per_point,
        };
        self.renderer
            .update_buffers(device, queue, encoder, &tris, &desc);

        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            // Draw over the already-rendered canvas.
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            self.renderer.render(&mut pass, &tris, &desc);
        }

        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }

        // egui asks for the next frame via `repaint_delay`; zero means "as soon as
        // possible". Anything that animates or tracks a drag reports zero while it
        // is active, and idles otherwise, so this does not spin.
        output
            .viewport_output
            .get(&ViewportId::ROOT)
            .is_some_and(|v| v.repaint_delay.is_zero())
    }
}
