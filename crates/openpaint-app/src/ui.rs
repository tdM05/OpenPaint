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

use crate::editor::DEFAULT_EXTEND;
use crate::renderer::Overlay;
use crate::view::View;
use crate::ExtendDir;

/// Width of the side panel in logical points.
const PANEL_WIDTH: f32 = 280.0;

/// Read-only state the panel displays.
///
/// A struct rather than more parameters: `render` had already grown to the point of
/// needing an `allow(too_many_arguments)` once, and that was a signal rather than a
/// lint to silence.
pub struct Status<'a> {
    /// Undo depth, redo depth, snapshot bytes held.
    pub history: (usize, usize, usize),
    pub message: Option<&'a str>,
    pub page_size: (u32, u32),
}

/// What the panel wants the app to do, collected during the frame.
///
/// Actions are returned rather than performed, because some of them (extending the
/// page) re-create the GPU resources the current frame is still drawing with.
#[derive(Default)]
pub struct Outcome {
    /// egui wants another frame soon.
    pub wants_repaint: bool,
    pub extend: Option<(ExtendDir, u32)>,
}

pub struct Ui {
    ctx: egui::Context,
    state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    /// Screen-space rect (physical pixels) egui is currently occupying, so canvas
    /// input can be excluded from it.
    occupied: egui::Rect,
    /// How much an Extend adds. Lives in the UI because it is a user preference; it
    /// will move to settings when those exist (DECISIONS §5a: never a constant).
    extend_amount: u32,
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
            extend_amount: DEFAULT_EXTEND,
        }
    }

    /// Feed a window event to egui. Returns `true` if egui consumed it, in which
    /// case the caller should not treat it as canvas input.
    pub fn on_window_event(&mut self, window: &Window, event: &winit::event::WindowEvent) -> bool {
        self.state.on_window_event(window, event).consumed
    }

    /// Physical pixels of the left edge the panel occupies, so the canvas can be
    /// centered in the area actually visible rather than partly underneath it.
    #[must_use]
    pub fn inset_left_px(&self) -> f32 {
        self.occupied.max.x.max(0.0)
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
    /// The returned [`Outcome`] carries both whether egui wants another frame soon
    /// -- which the caller **must** honor, since painting is demand-driven and egui
    /// is only interactive while frames keep coming -- and any action the panel
    /// requested.
    #[must_use]
    pub fn render(
        &mut self,
        window: &Window,
        gpu: Overlay<'_>,
        brush: &mut Brush,
        view: &View,
        status: Status<'_>,
    ) -> Outcome {
        let Overlay {
            device,
            queue,
            encoder,
            target,
            size_px,
        } = gpu;
        let input = self.state.take_egui_input(window);
        let mut color_srgb = brush.color_srgb8();
        let mut extend = None;
        let mut extend_amount = self.extend_amount;

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
                    ui.separator();
                    ui.heading("View");
                    ui.label(format!(
                        "Zoom {:.0}%    Rotation {:.0} deg",
                        view.scale() * 100.0,
                        view.rotation().to_degrees()
                    ));
                    ui.label(
                        egui::RichText::new(
                            "Wheel zooms at the cursor. Space+drag or middle-drag pans. \
                             [ and ] rotate. 0 fits, 1 goes to 100%.",
                        )
                        .small()
                        .weak(),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Navigation is mouse/keyboard only for now: pen input \
                             bypasses the UI layer (Q14).",
                        )
                        .small()
                        .weak(),
                    );
                    ui.separator();
                    ui.heading("History");
                    let (undo_depth, redo_depth, bytes) = status.history;
                    ui.label(format!(
                        "Undo {undo_depth}   Redo {redo_depth}   ({:.1} MiB)",
                        bytes as f32 / (1024.0 * 1024.0)
                    ));
                    ui.label(
                        egui::RichText::new(
                            "Ctrl+Z undoes, Ctrl+Shift+Z or Ctrl+Y redoes. Snapshots \
                             cover only the area a stroke touched.",
                        )
                        .small()
                        .weak(),
                    );
                    ui.separator();
                    ui.heading("Export");
                    ui.label(
                        egui::RichText::new(
                            "Ctrl+S writes a PNG in the working directory. Not a \
                             save format yet: that waits for the page model (Q6).",
                        )
                        .small()
                        .weak(),
                    );
                    if let Some(msg) = status.message {
                        ui.label(egui::RichText::new(msg).small());
                    }
                    ui.separator();
                    ui.heading("Page");
                    ui.label(format!(
                        "{} x {} px",
                        status.page_size.0, status.page_size.1
                    ));
                    ui.add(
                        egui::Slider::new(&mut extend_amount, 32..=4096)
                            .logarithmic(true)
                            .text("Extend by (px)"),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Extend down").clicked() {
                            extend = Some((ExtendDir::Down, extend_amount));
                        }
                        if ui.button("up").clicked() {
                            extend = Some((ExtendDir::Up, extend_amount));
                        }
                        if ui.button("left").clicked() {
                            extend = Some((ExtendDir::Left, extend_amount));
                        }
                        if ui.button("right").clicked() {
                            extend = Some((ExtendDir::Right, extend_amount));
                        }
                    });
                    ui.label(
                        egui::RichText::new(
                            "All four directions exist in the engine; the real UI \
                             will show only what a mode needs (DECISIONS 5a). This \
                             is a debug panel, so it shows everything.",
                        )
                        .small()
                        .weak(),
                    );
                });
        });

        brush.set_color_srgb8(color_srgb);
        self.extend_amount = extend_amount;

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
            size_in_pixels: size_px,
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
        Outcome {
            wants_repaint: output
                .viewport_output
                .get(&ViewportId::ROOT)
                .is_some_and(|v| v.repaint_delay.is_zero()),
            extend,
        }
    }
}
