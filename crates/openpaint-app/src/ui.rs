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

use crate::editor::Tool;
use egui::ViewportId;
use openpaint_core::{Blend, Brush, Layer};
use winit::window::Window;

use crate::editor::DEFAULT_EXTEND;
use crate::renderer::Overlay;
use crate::view::View;
use openpaint_core::Side;

/// Width of the side panel in logical points.
const PANEL_WIDTH: f32 = 280.0;

/// Read-only state the panel displays.
///
/// A struct rather than more parameters: `render` had already grown to the point of
/// needing an `allow(too_many_arguments)` once, and that was a signal rather than a
/// lint to silence.
/// The crop rectangle in screen space, ready to paint.
///
/// Given as points rather than a rect because the canvas can be rotated, so the crop
/// outline is a parallelogram on screen. Physical pixels; the panel converts to egui's
/// logical points itself.
pub struct CropOverlay {
    /// Corners in page order: top-left, top-right, bottom-right, bottom-left.
    pub corners: [[f32; 2]; 4],
    /// The eight edge and corner handles.
    pub handles: [[f32; 2]; 8],
}

/// What the user answered to "you have unsaved changes".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmChoice {
    /// Save, then do the thing.
    SaveFirst,
    /// Do the thing and lose the changes.
    Discard,
    /// Do nothing.
    Cancel,
}

/// What the user answered to an offer of recovered work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryChoice {
    /// Load it.
    Recover,
    /// Throw it away.
    Discard,
}

/// A change the page panel wants made to the document.
///
/// Returned rather than applied, for the same reason as [`LayerAction`]: pages own GPU tiles and
/// history, neither of which the overlay closure can reach mid-frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PageAction {
    /// Work on this page from now on.
    Select(usize),
    /// Add an empty page after the active one, the same size.
    Add,
    /// Delete this page. Undoable, or it would not be offered.
    Delete(usize),
    /// Move a page to a new index.
    Move { from: usize, to: usize },
}

/// A change the layer panel wants made to the stack.
///
/// Returned rather than applied, like every other panel action: layer operations touch GPU
/// tiles and history, neither of which the overlay closure can reach while the frame it is
/// drawing into is still open.
#[derive(Clone, Debug, PartialEq)]
pub enum LayerAction {
    /// Paint on this layer from now on.
    Select(usize),
    /// Add an empty layer above the active one.
    Add,
    /// Delete this layer. Undoable, or it would not be offered at all.
    Delete(usize),
    /// Move a layer to a new index.
    Move { from: usize, to: usize },
    /// Show or hide a layer.
    SetVisible { index: usize, visible: bool },
    /// Freeze or unfreeze a layer's transparency.
    SetLockAlpha { index: usize, lock: bool },
    /// Mask a layer by the layer below it, or stop.
    SetClipBelow { index: usize, clip: bool },
    /// Set a layer's opacity.
    SetOpacity { index: usize, opacity: f32 },
    /// Set a layer's blend mode.
    SetBlend { index: usize, blend: Blend },
}

/// The selection tools the panel offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectTool {
    Lasso,
    Rect,
    /// Click a region of similar colour — the magic wand, and what a bucket uses.
    Wand,
}

/// What the selection controls want done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectAction {
    /// Turn a selection tool on, or off if it is already on.
    Use(SelectTool),
    All,
    None,
    Invert,
    /// Fill the selection with the brush colour.
    Fill,
    /// Clear the selection.
    Clear,
}

/// How the magic wand behaves.
///
/// Lives in the panel rather than the engine because these are preferences, and it is handed back
/// through [`Outcome`] like every other edit the panel makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WandSettings {
    /// Largest per-channel difference from the clicked colour still counted as the same region.
    pub tolerance: u8,
    /// How far to grow the region afterwards, in pixels, to tuck it under anti-aliased ink.
    pub expand: u32,
    /// Fill immediately on click instead of leaving a selection — which is what a bucket is.
    pub fill_on_click: bool,
}

impl Default for WandSettings {
    fn default() -> Self {
        Self {
            // A middling tolerance: high enough to climb a soft edge, low enough not to walk
            // through a grey. Nothing has measured a better starting point, and the control is
            // right there.
            tolerance: 32,
            // Two pixels of growth, which covers the edge ramp a typical soft brush leaves without
            // visibly bleeding past the far side of a thin line.
            expand: 2,
            // A bucket by default, because that is what the tool is reached for.
            fill_on_click: true,
        }
    }
}

/// What the crop tool should do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropAction {
    Start,
    Apply,
    Cancel,
}

pub struct Status<'a> {
    /// Undo depth, redo depth, snapshot bytes held.
    pub history: (usize, usize, u64),
    pub message: Option<&'a str>,
    pub page_size: (u32, u32),
    /// Present while the crop tool is active.
    pub crop: Option<&'a CropOverlay>,
    /// The crop rectangle, for display.
    pub crop_rect: Option<(i32, i32, u32, u32)>,
    /// Resident canvas tiles and pool capacity.
    pub residency: (u32, u32),
    /// Tiles currently held on the CPU because they did not fit on the GPU.
    pub spilled: usize,
    /// Readbacks and re-uploads so far, for spotting a thrashing budget.
    pub traffic: (u64, u64),
    /// The layer stack, bottom first, as the document holds it.
    pub layers: &'a [Layer],
    /// Index of the layer being painted.
    pub active_layer: usize,
    /// How many pages the document has, and which is active.
    pub pages: (usize, usize),
    /// The tool strokes currently use.
    pub tool: Tool,
    /// Set while an unsaved-changes question is waiting, describing what is about to happen.
    pub confirm: Option<&'static str>,
    /// The brush outline to draw at the pointer, when there is a pointer to draw it at.
    pub brush_cursor: Option<BrushCursor>,
    /// Latency and frame-time readout.
    pub perf: crate::perf::PerfSnapshot,
    /// Set while unsaved work from a previous run is waiting to be accepted or thrown away.
    pub recovery: Option<&'a str>,
    /// What autosave has to report: a line of text, ready to show.
    pub autosave: &'a str,
    /// Selection boundary in screen space, as closed loops.
    pub selection: &'a [Vec<[f32; 2]>],
    /// Which selection tool is active, if any.
    pub select_tool: Option<SelectTool>,
    /// Whether there is a selection to act on.
    pub has_selection: bool,
    /// Wand settings: colour tolerance, how far to grow the region, and whether a click fills.
    pub wand: WandSettings,
}

/// Where to draw the brush outline, and how big.
///
/// The size a mark will be is otherwise invisible until you make one, which makes choosing a
/// radius a guess-and-undo loop. Both fields are in physical pixels, ready to be divided by the
/// scale factor — the same convention as [`CropOverlay`], so the two overlays cannot drift.
#[derive(Clone, Copy, Debug)]
pub struct BrushCursor {
    pub centre: [f32; 2],
    pub radius: f32,
}

/// What the panel wants the app to do, collected during the frame.
///
/// Actions are returned rather than performed, because some of them (extending the
/// page) re-create the GPU resources the current frame is still drawing with.
#[derive(Default)]
pub struct Outcome {
    /// egui wants another frame soon.
    pub wants_repaint: bool,
    pub extend: Option<(Side, u32)>,
    pub crop: Option<CropAction>,
    /// Discard the tiles outside the page, reclaiming their memory.
    pub trim: bool,
    /// At most one layer change per frame, which is all a click can produce.
    pub layer: Option<LayerAction>,
    /// At most one page change per frame.
    pub page: Option<PageAction>,
    /// Switch tool.
    pub tool: Option<Tool>,
    /// The answer to the unsaved-changes question, if one was given.
    pub confirm: Option<ConfirmChoice>,
    /// The answer to the offer of recovered work.
    pub recovery: Option<RecoveryChoice>,
    /// A selection command.
    pub select: Option<SelectAction>,
    /// Wand settings, as the panel currently holds them.
    pub wand: WandSettings,
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
        let mut crop_action = None;
        let mut trim = false;
        let mut layer_action = None;
        let mut page_action = None;
        let mut tool_action = None;
        let mut confirm_choice = None;
        let mut recovery_choice = None;
        let mut select_action = None;
        let mut wand = status.wand;

        let mut panel_rect = egui::Rect::NOTHING;
        let output = self.ctx.run(input, |ctx| {
            let panel = egui::SidePanel::left("brush-panel")
                .exact_width(PANEL_WIDTH)
                .show(ctx, |ui| {
                    // Scrollable, because the panel already stands taller than a window and
                    // every section below the fold was simply unreachable -- the speed readout
                    // was invisible on a laptop screen. `auto_shrink` off so it fills the panel
                    // instead of collapsing onto its content.
                    egui::ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            ui.heading("Tool");
                            ui.horizontal(|ui| {
                                for tool in Tool::ALL {
                                    if ui
                                        .selectable_label(status.tool == tool, tool.label())
                                        .clicked()
                                    {
                                        tool_action = Some(tool);
                                    }
                                }
                            });
                            ui.label(
                                egui::RichText::new(
                                    "B and E switch tool. Each keeps its own size, because an eraser \
                                     almost never wants the brush's. [ and ] resize; Shift+[ and \
                                     Shift+] rotate the canvas.",
                                )
                                .small()
                                .weak(),
                            );

                            ui.separator();
                            ui.heading("Select");
                            ui.horizontal(|ui| {
                                for (tool, label) in [
                                    (SelectTool::Lasso, "Lasso"),
                                    (SelectTool::Rect, "Rectangle"),
                                    (SelectTool::Wand, "Wand"),
                                ] {
                                    let on = status.select_tool == Some(tool);
                                    if ui.selectable_label(on, label).clicked() {
                                        select_action = Some(SelectAction::Use(tool));
                                    }
                                }
                            });
                            ui.horizontal(|ui| {
                                if ui.button("All").clicked() {
                                    select_action = Some(SelectAction::All);
                                }
                                // Disabled rather than hidden, so the commands do not move around
                                // depending on state.
                                if ui
                                    .add_enabled(status.has_selection, egui::Button::new("None"))
                                    .clicked()
                                {
                                    select_action = Some(SelectAction::None);
                                }
                                if ui
                                    .add_enabled(status.has_selection, egui::Button::new("Invert"))
                                    .clicked()
                                {
                                    select_action = Some(SelectAction::Invert);
                                }
                            });
                            if status.select_tool == Some(SelectTool::Wand) {
                                ui.add(
                                    egui::Slider::new(&mut wand.tolerance, 0..=128)
                                        .text("Tolerance"),
                                );
                                ui.add(egui::Slider::new(&mut wand.expand, 0..=8).text("Expand"));
                                ui.checkbox(&mut wand.fill_on_click, "Fill on click (bucket)");
                                ui.label(
                                    egui::RichText::new(
                                        "Tolerance is how far up an anti-aliased edge still counts as the region; Expand tucks the result under the ink so no pale fringe is left. With Fill off the wand leaves a selection instead.",
                                    )
                                    .small()
                                    .weak(),
                                );
                            }

                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(
                                        status.has_selection,
                                        egui::Button::new("Fill with brush colour"),
                                    )
                                    .clicked()
                                {
                                    select_action = Some(SelectAction::Fill);
                                }
                                if ui
                                    .add_enabled(status.has_selection, egui::Button::new("Clear"))
                                    .clicked()
                                {
                                    select_action = Some(SelectAction::Clear);
                                }
                            });
                            ui.label(
                                egui::RichText::new(
                                    "Ctrl+A selects everything, Ctrl+D deselects, Ctrl+Shift+I inverts, Ctrl+F fills and Delete clears. Fill never erases and Clear always does, so neither depends on which tool is selected.",
                                )
                                .small()
                                .weak(),
                            );

                            ui.separator();
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
                            ui.label(
                                egui::RichText::new(
                                    "Alt+click picks the colour under the pointer, sampled from                                      the composited image rather than from one layer.",
                                )
                                .small()
                                .weak(),
                            );

                            ui.separator();
                            ui.add(
                                egui::Slider::new(
                                    &mut brush.stabilization_ms,
                                    0.0..=openpaint_core::stabilizer::MAX_LAG_MS,
                                )
                                .text("Stabilization (ms)"),
                            );
                            // The control is denominated in its own price. A one-pole filter trails
                            // its input by exactly its time constant, so the setting *is* the
                            // latency it adds -- there is no abstract "strength" to translate, and
                            // no invented maximum to scale against. Latency being the top quality
                            // axis (DECISIONS §4.1), the artist should be spending it in units they
                            // can compare with the Speed readout.
                            ui.label(
                                egui::RichText::new(if brush.stabilization_ms <= 0.0 {
                                    "Off. Smooths pen shake; the cost is lag, in the same \
                                     milliseconds as the stroke time under Speed."
                                } else {
                                    "Smooths pen shake. The line trails the pen by this long, \
                                     on top of the stroke time under Speed."
                                })
                                .small()
                                .weak(),
                            );

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
                                    "Ctrl+Z undoes, Ctrl+Shift+Z or Ctrl+Y redoes. Snapshots cover \
                                     only the tiles a stroke touched.",
                                )
                                .small()
                                .weak(),
                            );
                            ui.separator();
                            ui.heading("Pages");
                            let (page_count, active_page) = status.pages;
                            if ui.button("Add page").clicked() {
                                page_action = Some(PageAction::Add);
                            }
                            for index in 0..page_count {
                                ui.horizontal(|ui| {
                                    let selected = index == active_page;
                                    if ui
                                        .selectable_label(selected, format!("Page {}", index + 1))
                                        .clicked()
                                    {
                                        page_action = Some(PageAction::Select(index));
                                    }
                                    if selected {
                                        if ui
                                            .add_enabled(index > 0, egui::Button::new("Up"))
                                            .clicked()
                                        {
                                            page_action = Some(PageAction::Move {
                                                from: index,
                                                to: index - 1,
                                            });
                                        }
                                        if ui
                                            .add_enabled(index + 1 < page_count, egui::Button::new("Down"))
                                            .clicked()
                                        {
                                            page_action = Some(PageAction::Move {
                                                from: index,
                                                to: index + 1,
                                            });
                                        }
                                        // The last page cannot go: a document must have somewhere to
                                        // draw.
                                        if ui
                                            .add_enabled(page_count > 1, egui::Button::new("Delete"))
                                            .clicked()
                                        {
                                            page_action = Some(PageAction::Delete(index));
                                        }
                                    }
                                });
                            }
                            ui.label(
                                egui::RichText::new(
                                    "A webtoon is one very tall page, a sketchbook is many -- one model \
                                     either way (DECISIONS §5a). Deleting a page is undoable.",
                                )
                                .small()
                                .weak(),
                            );

                            ui.separator();
                            ui.heading("Layers");
                            if ui.button("Add layer").clicked() {
                                layer_action = Some(LayerAction::Add);
                            }
                            // Top-down, because that is how every drawing app shows a stack and how
                            // artists talk about it -- the document stores it bottom-first.
                            let count = status.layers.len();
                            for (index, layer) in status.layers.iter().enumerate().rev() {
                                let selected = index == status.active_layer;
                                ui.push_id(layer.id(), |ui| {
                                    ui.horizontal(|ui| {
                                        let mut visible = layer.visible;
                                        if ui.checkbox(&mut visible, "").changed() {
                                            layer_action =
                                                Some(LayerAction::SetVisible { index, visible });
                                        }
                                        if ui.selectable_label(selected, &layer.name).clicked() {
                                            layer_action = Some(LayerAction::Select(index));
                                        }
                                        // In the row, next to visibility, because both are per-layer
                                        // switches an artist flips constantly while colouring.
                                        let mut lock = layer.lock_alpha;
                                        if ui
                                            .toggle_value(&mut lock, "α")
                                            .on_hover_text(
                                                "Lock alpha: paint only where this layer already                                                  has pixels, and never change its transparency.                                                  How colour goes inside line art without a                                                  selection.",
                                            )
                                            .changed()
                                        {
                                            layer_action =
                                                Some(LayerAction::SetLockAlpha { index, lock });
                                        }
                                        let mut clip = layer.clip_below;
                                        if ui
                                            .toggle_value(&mut clip, "⊂")
                                            .on_hover_text(
                                                "Clip to the layer below: this layer only shows                                                  where the one beneath it has pixels, and stays                                                  separately editable. How shading and highlights                                                  sit over flats.",
                                            )
                                            .changed()
                                        {
                                            layer_action =
                                                Some(LayerAction::SetClipBelow { index, clip });
                                        }
                                    });
                                    if selected {
                                        ui.horizontal(|ui| {
                                            let mut opacity = layer.opacity;
                                            if ui
                                                .add(
                                                    egui::Slider::new(&mut opacity, 0.0..=1.0)
                                                        .text("opacity"),
                                                )
                                                .changed()
                                            {
                                                layer_action =
                                                    Some(LayerAction::SetOpacity { index, opacity });
                                            }
                                        });
                                        ui.horizontal(|ui| {
                                            egui::ComboBox::from_id_salt("blend")
                                                .selected_text(layer.blend.label())
                                                .show_ui(ui, |ui| {
                                                    for mode in Blend::ALL {
                                                        if ui
                                                            .selectable_label(
                                                                layer.blend == mode,
                                                                mode.label(),
                                                            )
                                                            .clicked()
                                                        {
                                                            layer_action = Some(LayerAction::SetBlend {
                                                                index,
                                                                blend: mode,
                                                            });
                                                        }
                                                    }
                                                });
                                            if ui
                                                .add_enabled(index + 1 < count, egui::Button::new("Up"))
                                                .clicked()
                                            {
                                                layer_action = Some(LayerAction::Move {
                                                    from: index,
                                                    to: index + 1,
                                                });
                                            }
                                            if ui.add_enabled(index > 0, egui::Button::new("Down")).clicked()
                                            {
                                                layer_action = Some(LayerAction::Move {
                                                    from: index,
                                                    to: index - 1,
                                                });
                                            }
                                            // The last layer cannot go: a page with nowhere to paint is
                                            // a state every caller would have to special-case.
                                            if ui
                                                .add_enabled(count > 1, egui::Button::new("Delete"))
                                                .clicked()
                                            {
                                                layer_action = Some(LayerAction::Delete(index));
                                            }
                                        });
                                    }
                                });
                            }
                            ui.label(
                                egui::RichText::new(
                                    "Multiply darkens what is under it, Screen lightens. Deleting a \
                                     layer is undoable -- otherwise it would not be offered.",
                                )
                                .small()
                                .weak(),
                            );

                            ui.separator();
                            ui.heading("Canvas memory");
                            let (used, capacity) = status.residency;
                            ui.label(format!(
                                "GPU {used} / {capacity} tiles ({:.0} of {:.0} MiB)",
                                used as f32 * 0.5,
                                capacity as f32 * 0.5
                            ));
                            if status.spilled > 0 {
                                let (out, back) = status.traffic;
                                ui.label(format!(
                                    "CPU {} tiles ({:.0} MiB), {out} out / {back} back",
                                    status.spilled,
                                    status.spilled as f32 * 0.5
                                ));
                            }
                            if ui.button("Trim to canvas").clicked() {
                                trim = true;
                            }
                            ui.label(
                                egui::RichText::new(
                                    "Cropping keeps the pixels outside the page, so nothing is lost \
                                     by accident. Trim discards them for good -- undoably.",
                                )
                                .small()
                                .weak(),
                            );
                            ui.separator();
                            ui.heading("Speed");
                            // Shown in the app, not only logged, because the number has to be visible at
                            // the moment something feels wrong -- that is when it is worth reading, and a
                            // log read afterwards cannot tell you what you were doing at the time.
                            match status.perf.input {
                                Some((mean, peak)) => {
                                    ui.label(format!("Stroke {mean:.1} ms, peak {peak:.1}"));
                                }
                                None => {
                                    ui.label("Stroke -- draw something");
                                }
                            }
                            if let Some((mean, peak)) = status.perf.frame {
                                ui.label(format!("Frame {mean:.1} ms, peak {peak:.1}"));
                            }
                            ui.label(
                                egui::RichText::new(
                                    "Stroke time is from the sample reaching us to the frame being \
                                     presented. It leaves out the tablet, the driver and the display, \
                                     so the real figure is higher.",
                                )
                                .small()
                                .weak(),
                            );
                            ui.label(status.autosave);
                            ui.separator();
                            ui.heading("Export");
                            ui.label(
                                egui::RichText::new(
                                    "Ctrl+E writes a PNG in the working directory. Ctrl+S \
                                     saves the document itself, Ctrl+Shift+S under a new \
                                     name, Ctrl+O opens one and Ctrl+N starts one.",
                                )
                                .small()
                                .weak(),
                            );
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
                                    extend = Some((Side::Bottom, extend_amount));
                                }
                                if ui.button("up").clicked() {
                                    extend = Some((Side::Top, extend_amount));
                                }
                                if ui.button("left").clicked() {
                                    extend = Some((Side::Left, extend_amount));
                                }
                                if ui.button("right").clicked() {
                                    extend = Some((Side::Right, extend_amount));
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
                            ui.separator();
                            match status.crop_rect {
                                None => {
                                    if ui.button("Crop / resize by dragging").clicked() {
                                        crop_action = Some(CropAction::Start);
                                    }
                                }
                                Some((x, y, w, h)) => {
                                    ui.label(format!("Crop to {w} x {h} at ({x}, {y})"));
                                    ui.horizontal(|ui| {
                                        if ui.button("Apply").clicked() {
                                            crop_action = Some(CropAction::Apply);
                                        }
                                        if ui.button("Cancel").clicked() {
                                            crop_action = Some(CropAction::Cancel);
                                        }
                                    });
                                    ui.label(
                                        egui::RichText::new(
                                            "Drag an edge or corner; drag inside to move it. Dragging \
                                             outward extends the page. Enter applies, Escape cancels.",
                                        )
                                        .small()
                                        .weak(),
                                    );
                                }
                            }
                        });
                });
            panel_rect = panel.response.rect;

            // The status bar. It used to be a label at the bottom of this panel, under Export,
            // below several hundred pixels of other controls -- so it scrolled off screen and was
            // reported, fairly, as "what status line? I don't see it". Feedback nobody can see is
            // not feedback.
            //
            // Along the bottom of the *canvas*, where every application puts one, and drawn rather
            // than laid out so it cannot push the canvas around the way the confirm window once
            // did.
            if let Some(msg) = status.message {
                let screen = ctx.screen_rect();
                let left = panel_rect.right();
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("status-bar"),
                ));
                let text = painter.layout_no_wrap(
                    msg.to_owned(),
                    egui::FontId::proportional(13.0),
                    egui::Color32::from_white_alpha(230),
                );
                let pad = egui::vec2(10.0, 5.0);
                let size = text.size() + pad * 2.0;
                let at = egui::pos2(left + 12.0, screen.bottom() - size.y - 12.0);
                let box_rect = egui::Rect::from_min_size(at, size);
                painter.rect_filled(box_rect, 4.0, egui::Color32::from_black_alpha(190));
                painter.galley(at + pad, text, egui::Color32::WHITE);
            }


            // Recovered work gets its own window rather than being folded into the unsaved-changes
            // prompt: the question is different (there is nothing to save yet) and so are the
            // answers. If a third prompt ever appears, that is the point at which these should
            // become one general one -- two is not yet worth the indirection.
            if let Some(what) = status.recovery {
                egui::Window::new("Recovered work")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label("OpenPaint closed with unsaved changes.");
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(what).strong());
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            // Recover first and leftmost: it is the answer that loses nothing, and
                            // the one the artist almost always wants.
                            if ui.button("Recover").clicked() {
                                recovery_choice = Some(RecoveryChoice::Recover);
                            }
                            if ui.button("Discard").clicked() {
                                recovery_choice = Some(RecoveryChoice::Discard);
                            }
                        });
                        ui.label(
                            egui::RichText::new(
                                "Recovering opens it as unsaved work pointed at the original                                  file, so nothing is overwritten until you save.",
                            )
                            .small()
                            .weak(),
                        );
                    });
            }

            if let Some(what) = status.confirm {
                egui::Window::new("Unsaved changes")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(format!(
                            "This document has changes that are not in a file. Save before you {what}?"
                        ));
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            // Save first, and leftmost, because it is the answer that loses nothing.
                            if ui.button("Save").clicked() {
                                confirm_choice = Some(ConfirmChoice::SaveFirst);
                            }
                            if ui.button("Discard").clicked() {
                                confirm_choice = Some(ConfirmChoice::Discard);
                            }
                            if ui.button("Cancel").clicked() {
                                confirm_choice = Some(ConfirmChoice::Cancel);
                            }
                        });
                        ui.label(
                            egui::RichText::new("Enter saves, Escape cancels.")
                                .small()
                                .weak(),
                        );
                    });
            }
        });

        // Paint the crop outline over the canvas. Deliberately painted, not built from
        // widgets: egui never sees pen input (Q14), so widget handles would be
        // mouse-only. Input is handled in the app's own path instead.
        if let Some(overlay) = status.crop {
            let ppp = self.ctx.pixels_per_point();
            let to_point = |p: [f32; 2]| egui::pos2(p[0] / ppp, p[1] / ppp);
            let painter = self.ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("crop-overlay"),
            ));

            // Outline: two strokes, dark under light, so it stays visible over both
            // white paper and dark artwork.
            let pts: Vec<egui::Pos2> = overlay.corners.iter().copied().map(to_point).collect();
            for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 0)] {
                painter.line_segment(
                    [pts[a], pts[b]],
                    egui::Stroke::new(3.0_f32, egui::Color32::from_black_alpha(160)),
                );
                painter.line_segment(
                    [pts[a], pts[b]],
                    egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
                );
            }

            for h in &overlay.handles {
                let c = to_point(*h);
                let r = egui::Rect::from_center_size(c, egui::vec2(9.0, 9.0));
                painter.rect_filled(r, 0.0, egui::Color32::from_black_alpha(160));
                painter.rect_filled(r.shrink(1.5), 0.0, egui::Color32::WHITE);
            }
        }

        // The selection boundary. Two strokes, dark under light, for the same reason the crop
        // outline has two: it has to stay legible over white paper and over black ink without
        // knowing which is there. Static rather than animated -- marching ants are a later
        // refinement, and a still outline is not wrong, only quieter.
        if !status.selection.is_empty() {
            let ppp = self.ctx.pixels_per_point();
            let painter = self.ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("selection-overlay"),
            ));
            for path in status.selection {
                if path.len() < 2 {
                    continue;
                }
                // Closed: the loop's last point joins its first, and the dash phase has to run
                // across that join like any other.
                let mut points: Vec<egui::Pos2> = path
                    .iter()
                    .map(|p| egui::pos2(p[0] / ppp, p[1] / ppp))
                    .collect();
                points.push(points[0]);

                // Dark underneath, white dashes on top: the marching-ants convention, and it is a
                // convention because nothing else reads unambiguously over both white paper and
                // black ink.
                //
                // Dashed rather than solid because a solid line at the page border -- exactly where
                // "select all" and "invert" put one -- is indistinguishable from the page border
                // itself. Dashed *along the path*, not per segment: dashing each segment
                // separately turned a curved lasso into a spray of dots, because a curve's
                // segments are one to three pixels long and a dash pattern is a property of a path.
                painter.add(egui::Shape::line(
                    points.clone(),
                    egui::Stroke::new(2.0_f32, egui::Color32::from_black_alpha(190)),
                ));
                painter.extend(egui::Shape::dashed_line(
                    &points,
                    egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
                    5.0_f32,
                    5.0_f32,
                ));
            }
        }

        // The brush outline, drawn after the crop overlay so that on the one frame where both
        // could exist the crop wins the pixels. Painted, not a widget, for the same reason the
        // crop handles are: egui never sees pen input (Q14).
        if let Some(cursor) = status.brush_cursor {
            let ppp = self.ctx.pixels_per_point();
            let painter = self.ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("brush-cursor"),
            ));
            let centre = egui::pos2(cursor.centre[0] / ppp, cursor.centre[1] / ppp);
            // Floored at a size that is still a visible ring. Zoomed far out, a small brush is a
            // fraction of a pixel across, and an honest circle would simply vanish -- leaving the
            // artist with no pointer at all, which is worse than a slightly optimistic one.
            let radius = (cursor.radius / ppp).max(2.0);

            // Two rings, dark under light, for the same reason the crop outline has two: it has
            // to stay legible over white paper and over black ink without knowing which is there.
            painter.circle_stroke(
                centre,
                radius,
                egui::Stroke::new(2.0_f32, egui::Color32::from_black_alpha(130)),
            );
            painter.circle_stroke(
                centre,
                radius,
                egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(235)),
            );
        }

        brush.set_color_srgb8(color_srgb);
        self.extend_amount = extend_amount;

        // Record which pixels the *panel* owns, in physical coordinates, for `blocks_point` and
        // for the canvas inset.
        //
        // Deliberately the panel's own rect, not `used_rect()`. `used_rect` is the union of
        // everything egui drew, so a centred floating window -- the unsaved-changes prompt --
        // made it span half the screen and shoved the canvas sideways. The inset means "how much
        // of the left edge the panel covers", and only the panel can answer that.
        let scale = self.ctx.pixels_per_point();
        let used = panel_rect;
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
            crop: crop_action,
            trim,
            layer: layer_action,
            page: page_action,
            tool: tool_action,
            confirm: confirm_choice,
            recovery: recovery_choice,
            select: select_action,
            wand,
        }
    }
}
