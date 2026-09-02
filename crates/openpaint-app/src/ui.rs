//! Throwaway egui debug panel for engine work.
//!
//! Deliberately temporary. `docs/DECISIONS.md` §3 keeps the real CSP-like UI as a
//! later, reversible decision, and nothing in `openpaint-core` knows this exists.
//!
//! It is not decoration: a soft round brush **cannot** be tuned to match
//! Photoshop's (docs Q7a) without live sliders. Direction and opacity in particular
//! have correct semantics but were unreachable until now, so their behavior could
//! only be verified by unit test, never felt.
//!
//! # The pen operates this UI, but not through us
//!
//! Pen input arrives through octotablet, which bypasses winit's event stream entirely, so egui
//! never sees a pen event. It still works: Windows synthesises legacy mouse messages from pen
//! input, those reach winit, and egui takes them for ordinary mouse input. So the panel is driven
//! by synthesis while the canvas is driven by the real thing.
//!
//! That is the platform being helpful rather than a decision we made, and it is worth knowing
//! because it is fragile in three directions: consuming pen input ourselves (a hand-rolled
//! `WM_POINTER` path) can switch the synthesis off and kill every button with no warning; no other
//! platform promises it; and synthesised events are coalesced and lag, which is fine for pressing
//! a button and wrong for dragging anything. Tracked in OPEN_QUESTIONS Q14 — which said the
//! opposite until the author picked up a pen and disproved it.
//!
//! To stop strokes landing "through" the panel, [`Ui::blocks_point`] reports the
//! region egui occupies and the caller skips painting there. That check is needed
//! for the pen specifically, because egui's own pointer-capture logic never sees
//! it.

use crate::editor::Tool;
use crate::workspace::{Anchor, Place};
use egui::ViewportId;
use openpaint_core::{Blend, Brush, Curve, Layer, Response, Source};
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
/// A box with eight handles, in screen space, ready to paint.
///
/// One type for the crop rectangle and for the transform box. They are the same drawing and
/// answer the same question -- where is the box, and where can it be grabbed -- so a second copy
/// would only ever drift out of step with the first (§11a.8). Neither can be on screen while the
/// other is, so there is no ambiguity about which one a handle belongs to.
///
/// Given as points rather than as a rect because the canvas can be rotated and the content can be
/// too, so on screen the box is a parallelogram. Physical pixels; the panel converts to egui's
/// logical points itself.
pub struct HandleBox {
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

/// Something a menu asked the application to do.
///
/// Reported rather than performed, like every other panel action: these open dialogs, touch GPU
/// tiles and walk the undo stack, none of which the closure drawing the frame can reach. Every one
/// of them is something a keyboard shortcut already did --- the menu is a second way in, not a
/// second implementation, which is why they all land on the same handlers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    New,
    Open,
    Save,
    SaveAs,
    ExportPng,
    Undo,
    Redo,
    ZoomFit,
    ZoomActual,
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
    /// Copy this layer, pixels and all, above itself.
    Duplicate(usize),
    /// Fold this layer into the one below it.
    MergeDown(usize),
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

/// What the Transform section asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransformAction {
    /// Lift the selection and hold it in the air.
    Begin,
    /// Put it down where it now sits.
    Apply,
    /// Put it back where it came from.
    Cancel,
}

/// A transform in flight, as the panel needs to see and edit it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformState {
    pub transform: openpaint_core::Transform,
    /// Whether a corner drag keeps the two axes equal.
    ///
    /// Held rather than inferred from the two scales being equal. The derived version turned
    /// itself off the moment a non-uniform scale was typed in, so the checkbox could not be
    /// trusted -- and a lock that silently releases is worse than none.
    pub lock_aspect: bool,
    pub kernel: openpaint_core::Kernel,
}

/// What the Brush section asked for, beyond editing the brush in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushAction {
    /// Keep the current colour in the document's palette.
    SaveColor,
    /// Adopt a colour from the palette.
    UseColor([u8; 3]),
    /// Drop a colour from the palette.
    ForgetColor(usize),
    /// Choose an image to use as the dab's shape.
    LoadTip,
    /// Save the brush as it stands, under the name in the box.
    SavePreset,
    /// Adopt a saved brush.
    ApplyPreset(usize),
    /// Forget a saved brush.
    DeletePreset(usize),
}

/// What the Text section asked for, beyond editing the block in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAction {
    /// Add a text layer, ready to type into.
    AddLayer,
    /// Turn the active text layer into an ordinary raster layer. One-way.
    ConvertToRaster,
    /// Make font files available without installing them system-wide.
    LoadFontFile,
}

/// The selection tools the panel offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectTool {
    Lasso,
    Rect,
    /// Click a region of similar colour — the magic wand, and what a bucket uses.
    Wand,
    /// Drag the selected pixels somewhere else.
    Move,
}

/// What the selection controls want done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectAction {
    /// Turn a selection tool on, or off if it is already on.
    Use(SelectTool),
    /// Put the selection tool away, leaving what is selected alone.
    ///
    /// **Not [`SelectAction::None`]**, which deselects. Two different things that sound alike, and
    /// conflating them is how choosing the brush threw away a lasso somebody had just drawn.
    Stop,
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
    pub crop: Option<&'a HandleBox>,
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
    /// The colours saved with this document, in the order they were added.
    pub palette: &'a [[u8; 3]],
    /// The saved brushes, in the order they were added.
    pub presets: &'a [openpaint_core::BrushPreset],
    /// Set when the brush library itself is in trouble -- unreadable, or unwritable.
    pub preset_trouble: Option<&'a str>,
    /// Font families installed on this machine, sorted, for the picker.
    ///
    /// Passed in rather than read here because enumerating them is a font-stack operation and this
    /// panel is throwaway; the list is gathered once at startup and reused.
    pub font_families: &'a [String],
    /// Set when the active text layer is being shown in a font it was not written in.
    pub font_substituted: Option<&'a str>,
    /// Set while a transform is in the air.
    pub transform: Option<TransformState>,
    /// The transform box on the canvas, present exactly when `transform` is.
    pub transform_box: Option<&'a HandleBox>,
    /// The filter a transform would use, whether or not one is in flight.
    pub kernel: openpaint_core::Kernel,
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

/// A small editable response curve.
///
/// Drag a point to move it, click empty space to add one, right-click a point to remove it. The
/// ends stay pinned in x, because a curve that does not span the input range has an undefined
/// answer at the edges — and "what happens at full pressure" is not a question a brush may decline.
///
/// Drawn rather than assembled from widgets, for the same reason the crop and selection overlays
/// are: this is direct manipulation of a shape, and a shape is not a stack of rectangles.
fn curve_editor(ui: &mut egui::Ui, label: &str, curve: &mut Curve) -> bool {
    const SIDE: f32 = 140.0;
    ui.label(egui::RichText::new(label).small());
    // An **explicit** id, not the automatic one. egui derives automatic ids from a per-frame
    // counter, so anything that changes what the panel contains -- the wand's sliders appearing,
    // the layer list growing -- renumbers every widget after it. A click is press on one frame and
    // release on another, so a widget whose id moved in between never sees either half, and the
    // editor simply did not respond. Sliders survive that because they are dragged, not clicked.
    let (rect, _) = ui.allocate_exact_size(egui::vec2(SIDE, SIDE), egui::Sense::hover());
    let response = ui.interact(
        rect,
        ui.id().with(("curve-editor", label)),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);

    // Curve space is (0,0) bottom-left to (1,1) top-right; screen y runs the other way.
    let to_screen = |p: (f32, f32)| {
        egui::pos2(
            rect.left() + p.0 * rect.width(),
            rect.bottom() - p.1 * rect.height(),
        )
    };
    let to_curve = |p: egui::Pos2| {
        (
            ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
            ((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0),
        )
    };

    painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
    // The identity, as a reference against which a curve's shape is legible.
    painter.line_segment(
        [to_screen((0.0, 0.0)), to_screen((1.0, 1.0))],
        egui::Stroke::new(1.0_f32, ui.visuals().weak_text_color()),
    );

    let mut points: Vec<(f32, f32)> = curve.points().to_vec();
    let mut changed = false;

    let id = response.id;
    let nearest = |pos: egui::Pos2, points: &[(f32, f32)]| {
        points
            .iter()
            .enumerate()
            .map(|(i, p)| (i, to_screen(*p).distance(pos)))
            .filter(|(_, d)| *d < 10.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };

    // Which point a drag grabbed, latched for the length of the drag. Re-picking the nearest
    // point every frame looks equivalent and is not: drag two points close together and the drag
    // hops to whichever is now nearer, so a point can be dropped somewhere nobody aimed for.
    if response.drag_started() {
        let grabbed = response
            .interact_pointer_pos()
            .and_then(|p| nearest(p, &points));
        ui.memory_mut(|m| m.data.insert_temp(id, grabbed));
    }
    if response.drag_stopped() {
        ui.memory_mut(|m| m.data.remove::<Option<usize>>(id));
    }

    if let Some(pos) = response.interact_pointer_pos() {
        let hit = if response.dragged() {
            ui.memory(|m| m.data.get_temp::<Option<usize>>(id))
                .flatten()
        } else {
            nearest(pos, &points)
        };

        if response.secondary_clicked() {
            // Never below two, and never an end: an end is what defines the range.
            if let Some(i) = hit {
                if points.len() > 2 && i != 0 && i != points.len() - 1 {
                    points.remove(i);
                    changed = true;
                }
            }
        } else if response.dragged() || response.clicked() {
            let target = to_curve(pos);
            match hit {
                Some(i) => {
                    // The ends may move in y but not in x, and interior points stay between their
                    // neighbours -- a curve whose points crossed over would have two answers at one
                    // input.
                    let x = if i == 0 {
                        points[0].0
                    } else if i == points.len() - 1 {
                        points[i].0
                    } else {
                        target
                            .0
                            .clamp(points[i - 1].0 + 0.02, points[i + 1].0 - 0.02)
                    };
                    points[i] = (x, target.1);
                    changed = true;
                }
                None if response.clicked() => {
                    let at = points
                        .iter()
                        .position(|p| p.0 > target.0)
                        .unwrap_or(points.len());
                    if at > 0 && at < points.len() {
                        points.insert(at, target);
                        changed = true;
                    }
                }
                None => {}
            }
        }
    }

    if changed {
        if let Some(next) = Curve::from_points(points.clone()) {
            *curve = next;
        } else {
            // Refused rather than repaired: the clamping above should make this unreachable, and
            // silently sorting the points would hide it if it were not.
            changed = false;
        }
    }

    // The curve itself, sampled.
    let samples: Vec<egui::Pos2> = (0..=48)
        .map(|i| {
            let x = i as f32 / 48.0;
            to_screen((x, curve.at(x)))
        })
        .collect();
    painter.add(egui::Shape::line(
        samples,
        egui::Stroke::new(2.0_f32, ui.visuals().strong_text_color()),
    ));
    for p in curve.points() {
        painter.circle_filled(to_screen(*p), 3.5, ui.visuals().strong_text_color());
    }
    changed
}

/// The controls for one text block. Returns whether anything changed.
///
/// Every edit here invalidates the layer's pixels, so the return value is not a convenience — the
/// caller must re-derive on it, and a control added later that forgets to report would leave the
/// canvas showing the previous wording.
///
/// The words themselves are edited in an ordinary `TextEdit` rather than with a caret drawn on the
/// canvas. Deliberate: this panel is throwaway (DECISIONS §3), a canvas caret is a text editor's
/// worth of work, and `TextEdit` already brings a caret, selection, clipboard and IME — which is
/// what makes Japanese and Korean input work today rather than after the real UI lands.
fn text_editor(
    ui: &mut egui::Ui,
    block: &mut openpaint_core::TextBlock,
    families: &[String],
    substituted: Option<&str>,
) -> bool {
    let mut changed = false;

    if let Some(actual) = substituted {
        // Loud, because the alternative is shipping a page lettered in the wrong face.
        ui.colored_label(
            egui::Color32::from_rgb(220, 160, 60),
            format!(
                "\u{26a0} {:?} is not installed. Showing {actual:?}.",
                block.font.family
            ),
        );
    }

    changed |= ui
        .add(
            egui::TextEdit::multiline(&mut block.text)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("Type the caption"),
        )
        .changed();

    ui.horizontal(|ui| {
        ui.label("Font");
        let current = if block.font.family.is_empty() {
            "(default)".to_owned()
        } else {
            block.font.family.clone()
        };
        egui::ComboBox::from_id_salt("text-font-family")
            .selected_text(current)
            .width(180.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(block.font.family.is_empty(), "(default)")
                    .clicked()
                {
                    block.font.family.clear();
                    changed = true;
                }
                for family in families {
                    if ui
                        .selectable_label(&block.font.family == family, family)
                        .clicked()
                    {
                        block.font.family.clone_from(family);
                        changed = true;
                    }
                }
            });
    });

    ui.horizontal(|ui| {
        let mut bold = block.font.weight >= 600;
        if ui.checkbox(&mut bold, "Bold").changed() {
            block.font.weight = if bold { 700 } else { 400 };
            changed = true;
        }
        changed |= ui.checkbox(&mut block.font.italic, "Italic").changed();
    });

    changed |= ui
        .add(egui::Slider::new(&mut block.size, 6.0..=300.0).text("Size px"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut block.line_height, 0.5..=3.0).text("Line height"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut block.letter_spacing, -10.0..=40.0).text("Letter spacing"))
        .changed();

    ui.horizontal(|ui| {
        ui.label("Align");
        for align in openpaint_core::text::Align::ALL {
            if ui
                .selectable_label(block.align == align, align.label())
                .clicked()
            {
                block.align = align;
                changed = true;
            }
        }
    });

    ui.horizontal(|ui| {
        ui.label("Colour");
        changed |= ui.color_edit_button_srgb(&mut block.color_srgb8).changed();
    });

    ui.horizontal(|ui| {
        // `None` is a single line that grows as it is typed; `Some` is a box that wraps. Two ways
        // of placing text, one field, rather than two kinds of block.
        let mut wraps = block.wrap_width.is_some();
        if ui
            .checkbox(&mut wraps, "Wrap")
            .on_hover_text("Off: one line that grows. On: wraps at the width below.")
            .changed()
        {
            block.wrap_width = wraps.then_some(400.0);
            changed = true;
        }
        if let Some(width) = block.wrap_width.as_mut() {
            changed |= ui
                .add(egui::Slider::new(width, 40.0..=4000.0).text("px"))
                .changed();
        }
    });

    ui.horizontal(|ui| {
        ui.label("Position");
        changed |= ui
            .add(egui::DragValue::new(&mut block.x).speed(1.0))
            .changed();
        changed |= ui
            .add(egui::DragValue::new(&mut block.y).speed(1.0))
            .changed();
    });

    changed
}

/// One brush parameter's modulation: which input drives it, and the curve that maps it.
///
/// Built in a loop over [`Brush::responses_mut`] rather than written out per parameter, so adding a
/// modulatable parameter to the engine makes an editor for it appear rather than needing a second
/// edit here that is easy to forget.
fn response_editor(ui: &mut egui::Ui, label: &str, response: &mut Response) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).small().strong());
        egui::ComboBox::from_id_salt(("modulation-source", label))
            .selected_text(response.source.label())
            .width(96.0)
            .show_ui(ui, |ui| {
                for source in Source::ALL {
                    ui.selectable_value(&mut response.source, source, source.label());
                }
            });
    });
    curve_editor(ui, label, &mut response.curve);
}

/// What each panel of the workspace shows.
///
/// Everything a described panel needs in order to be drawn.
///
/// **The seam.** What a panel says lives in [`crate::panels`], one module each, and none of it
/// knows what a slider looks like. This is the whole of what it may use to draw: replacing the
/// widget layer later means rewriting `show` below and nothing else, which is what "a panel is a
/// narrow trait" was promising when the plan was made.
///
/// A struct for the same reason `Status` is one: the argument list grew past the lint, and
/// that is a signal rather than a lint to silence. These three always travel together --- the look,
/// which way the controls run, and the gesture the panel is part-way through.
pub(crate) struct Painting<'a> {
    pub(crate) theme: &'a crate::theme::Theme,
    pub(crate) direction: crate::panel_ui::Direction,
    pub(crate) input: &'a mut crate::panel_draw::PanelInput,
    /// Which [`crate::panel_ui::Control::Pick`] has its list open, if any.
    ///
    /// The control's id, and one at a time because one popup is open at a time. Beside `menu` for
    /// the same reason: it is state a panel is part-way through, and it has to survive between
    /// frames.
    pub(crate) pick: &'a mut Option<crate::panel_ui::ControlId>,
    /// How much the Page panel's extend buttons add, in pixels.
    ///
    /// A panel's own setting, kept by the shell between frames like the open menu -- and handed
    /// back so the panel does not hold a second copy. The Page panel held one for a while, and the
    /// consequence was that the slider moved and the buttons went on adding 512.
    pub(crate) extend_by: u32,
    /// What the next saved brush preset will be called.
    ///
    /// Same reason: the shell owns it, so the panel showing it must be shown it. A panel mirroring
    /// it cannot know when the shell clears it, which is exactly what a successful save does.
    pub(crate) preset_name: &'a str,
    /// Which menu is drilled into, if any.
    ///
    /// Only the menu panel uses it, but it lives here for the same reason the rest does: it is
    /// state a panel is part-way through, and it has to survive between frames.
    pub(crate) menu: &'a mut Option<u32>,
    /// For measuring text, which only something holding the fonts can do.
    pub(crate) ctx: &'a egui::Context,
    /// Which colour wheel the artist has chosen. A setting, kept where the other half-finished UI
    /// state is kept.
    pub(crate) wheel_shape: &'a mut crate::colour_wheel::Shape,
    /// Which part of the wheel a drag took hold of, if one has.
    pub(crate) wheel_hold: &'a mut Option<crate::colour_wheel::Region>,
}

impl Painting<'_> {
    /// Draw a list of controls and report what changed.
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        controls: &[crate::panel_ui::Control],
    ) -> Vec<crate::panel_ui::Change> {
        crate::panel_draw::show(ui, controls, self.theme, self.direction, self.input)
    }
}

/// Draw one of the workspace's panels.
///
/// A hand-off now: what each panel shows lives in [`crate::panels`], one module each. This keeps
/// the shape of the call in one place -- what a panel is given, and that it answers with at most
/// one [`Picked`] per frame.
fn workspace_panel(
    panel: crate::layout::PanelId,
    ui: &mut egui::Ui,
    brush: &mut Brush,
    color_srgb: &mut [u8; 3],
    state: &Status<'_>,
    paint: &mut Painting<'_>,
    place: Place,
) -> Option<Picked> {
    ui.spacing_mut().item_spacing.y = 5.0;
    crate::panels::show(panel, ui, brush, color_srgb, state, paint, place)
}

/// What a workspace panel asked for.
#[derive(Clone, Debug, PartialEq)]
// The panels that produce the last dozen of these are being written; `expect` rather than `allow`
// so this stops compiling the moment the last one lands and cannot be left behind.
#[expect(dead_code, reason = "the panels that produce them are being ported")]
pub(crate) enum Picked {
    Paint(Tool),
    Select(SelectTool),
    /// Open the workspace's panel list.
    PanelList,
    /// Drop a menu down under the button that asked for it.
    OpenMenu {
        at: crate::layout::Rect,
        size: (f32, f32),
        side: Anchor,
    },
    /// Put the open menu away.
    CloseMenu,
    /// Open the workspace's own settings.
    Settings,
    /// A layer command, from the described Layers panel.
    Layer(LayerAction),
    /// A selection command, from a menu.
    Selection(SelectAction),
    /// Something the application has to do: open a dialog, walk history, fit the view.
    Command(Command),
    /// A page command: add, delete, reorder, or go to one.
    Page(PageAction),
    /// A transform command: begin one, apply it, put it back.
    Transform(TransformAction),
    /// The transform as the artist has set it: scale, rotation, whether the axes are locked, and
    /// which resampling the apply will use.
    ///
    /// Carried whole rather than as a stream of small edits, because a transform is one thing being
    /// adjusted -- and because two of its numbers move together when the axes are locked, which a
    /// per-field message could not say.
    TransformSet(TransformState),
    /// A crop command: start dragging one, apply it, put it back.
    Crop(CropAction),
    /// Grow the page on one side, by that many pixels.
    Extend(openpaint_core::Side, u32),
    /// How much the extend buttons add, which is a panel's own setting.
    ExtendBy(u32),
    /// Throw away the pixels outside the page, for good.
    Trim,
    /// A text command: add a layer, convert it, load a font.
    Text(TextAction),
    /// The text of the layer being edited changed, so its pixels have to be derived again.
    TextChanged,
    /// The caption as the artist has set it: the words, the face, the size, where it sits.
    ///
    /// Carried whole for the same reason `TransformSet` is: a caption is one thing being adjusted,
    /// and the panel that adjusts it holds a copy while it does. Writing it back through the
    /// `&mut TextBlock` the shell already has is what keeps the undo record where it belongs --
    /// a panel that edited the document directly would be a panel whose edits could not be undone.
    TextSet(openpaint_core::TextBlock),
    /// A brush-library command: save, apply or forget a preset, or a colour, or load a tip.
    Brush(BrushAction),
    /// What the next saved preset will be called, which is a panel's own setting.
    PresetName(String),
    /// The wand's settings, which belong to the tool rather than to the document.
    Wand(WandSettings),
}

/// The two prompts that stop everything until they are answered.
///
/// **Drawn whichever UI is up.** They used to sit at the end of the old side panel's branch, and
/// the workspace returns before it -- so in the workspace they were never drawn at all, while
/// `Status::recovery` and `Status::confirm` went on refusing every pen stroke (`decide_capture`).
/// A leftover recovery file from any previous crash therefore made the brush do nothing, for good,
/// with nothing on screen to say why. Reported as "most things do not work".
///
/// A prompt that blocks input and cannot be seen is the worst thing in this file; it belongs to
/// the application rather than to either UI, and now it is drawn like it.
fn prompts(
    ctx: &egui::Context,
    recovery: Option<&str>,
    confirm: Option<&'static str>,
) -> (Option<RecoveryChoice>, Option<ConfirmChoice>) {
    let mut answers = (None, None);
    // Recovered work gets its own window rather than being folded into the unsaved-changes
    // prompt: the question is different (there is nothing to save yet) and so are the
    // answers. If a third prompt ever appears, that is the point at which these should
    // become one general one -- two is not yet worth the indirection.
    if let Some(what) = recovery {
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
                            answers.0 = Some(RecoveryChoice::Recover);
                        }
                        if ui.button("Discard").clicked() {
                            answers.0 = Some(RecoveryChoice::Discard);
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

    if let Some(what) = confirm {
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
                        answers.1 = Some(ConfirmChoice::SaveFirst);
                    }
                    if ui.button("Discard").clicked() {
                        answers.1 = Some(ConfirmChoice::Discard);
                    }
                    if ui.button("Cancel").clicked() {
                        answers.1 = Some(ConfirmChoice::Cancel);
                    }
                });
                ui.label(
                    egui::RichText::new("Enter saves, Escape cancels.")
                        .small()
                        .weak(),
                );
            });
    }
    answers
}

/// Draw a box with its eight handles.
///
/// Two strokes for the outline and two fills for each handle, dark under light, because the
/// overlay has to stay legible over white paper *and* over black ink without knowing which is
/// underneath it. That is the same reason the selection outline and the brush ring are drawn twice
/// -- it is the convention, and it is a convention because nothing single-coloured works.
fn paint_handle_box(painter: &egui::Painter, overlay: &HandleBox, ppp: f32) {
    let to_point = |p: [f32; 2]| egui::pos2(p[0] / ppp, p[1] / ppp);
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
        let r = egui::Rect::from_center_size(to_point(*h), egui::vec2(9.0, 9.0));
        painter.rect_filled(r, 0.0, egui::Color32::from_black_alpha(160));
        painter.rect_filled(r.shrink(1.5), 0.0, egui::Color32::WHITE);
    }
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
    /// egui wants another frame as soon as possible.
    pub wants_repaint: bool,
    /// egui wants a frame *after* a delay, or `None` if it wants nothing scheduled.
    ///
    /// **Not the same as `wants_repaint` being false.** A timed thing — a panel arming under a
    /// held pen, a tooltip, an animation — asks for a frame at a future moment, and dropping that
    /// request means the moment never arrives. The hold-to-move gesture silently never fired for
    /// exactly this reason: the request was made, reported as a non-zero delay, and thrown away
    /// because only zero was honoured.
    pub repaint_after: Option<std::time::Duration>,
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
    /// The active text layer's block was edited, so its pixels need re-deriving.
    pub text_changed: bool,
    /// A text command.
    pub text: Option<TextAction>,
    /// A brush command.
    pub brush: Option<BrushAction>,
    /// The name in the preset box, as the panel now holds it.
    pub preset_name: String,
    /// A transform command.
    pub transform: Option<TransformAction>,
    /// The transform in flight, as the panel now holds it.
    pub transform_state: Option<TransformState>,
    /// A menu command, at most one per frame.
    pub command: Option<Command>,
}

pub struct Ui {
    ctx: egui::Context,
    state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    /// Screen-space rect (physical pixels) egui is currently occupying, so canvas
    /// input can be excluded from it.
    occupied: egui::Rect,
    /// Where the canvas may draw, in physical pixels.
    ///
    /// The whole surface minus the side panel in the old layout; the canvas *panel's* rectangle
    /// in the workspace. A rectangle either way, because the canvas is a panel and can be
    /// anywhere (§1c) — see [`crate::view::View::set_viewport`].
    canvas_viewport: (f32, f32, f32, f32),
    /// How much an Extend adds. Lives in the UI because it is a user preference; it
    /// will move to settings when those exist (DECISIONS §5a: never a constant).
    extend_amount: u32,
    /// The name being typed into the brush-saving box.
    ///
    /// Held here rather than in the app for the same reason `extend_amount` is: it is a half-typed
    /// widget value, not state the document or the engine has any use for.
    preset_name: String,
    /// Each described panel's half-finished gesture: what it is holding, how far it is scrolled.
    ///
    /// Per panel, because two of them can be on screen at once and a list scrolled in one has
    /// nothing to say about the other. Keyed by panel id rather than by position, so rearranging
    /// the workspace does not shuffle the offsets between panels.
    panel_input: std::collections::HashMap<u32, crate::panel_draw::PanelInput>,
    /// Which menu the menu strip is drilled into, if any.
    menu_open: Option<u32>,
    /// Which control's dropdown is open, if any. Kept between frames, like `menu_open`.
    pick_open: Option<crate::panel_ui::ControlId>,
    /// Which colour wheel the artist chose.
    wheel_shape: crate::colour_wheel::Shape,
    /// Which part of the wheel a drag has hold of.
    wheel_hold: Option<crate::colour_wheel::Region>,
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
            canvas_viewport: (0.0, 0.0, 1.0, 1.0),
            extend_amount: DEFAULT_EXTEND,
            preset_name: String::new(),
            panel_input: std::collections::HashMap::new(),
            menu_open: None,
            pick_open: None,
            wheel_shape: crate::colour_wheel::Shape::default(),
            wheel_hold: None,
        }
    }

    /// Feed a window event to egui. Returns `true` if egui consumed it, in which
    /// case the caller should not treat it as canvas input.
    pub fn on_window_event(&mut self, window: &Window, event: &winit::event::WindowEvent) -> bool {
        self.state.on_window_event(window, event).consumed
    }

    /// How many physical pixels one logical unit is, so the shell can put a window-pixel
    /// position into the units the workspace works in.
    #[must_use]
    pub fn pixels_per_point(&self) -> f32 {
        self.ctx.pixels_per_point()
    }

    /// Physical pixels of the left edge the panel occupies, so the canvas can be
    /// centered in the area actually visible rather than partly underneath it.
    #[must_use]
    pub fn canvas_viewport(&self) -> (f32, f32, f32, f32) {
        self.canvas_viewport
    }

    /// Whether a text field somewhere has the caret.
    ///
    /// **Because a key a field is taking is not a shortcut.** Naming a brush "beeper" selected the
    /// eraser, resized the brush twice and refitted the view, because every letter typed into the
    /// box was also read as a tool key as well. Every text editor in the world answers this
    /// question the same way, and the shell has to be able to ask it: the field lives in the panel
    /// layer and the shortcuts live in the shell, so one of them must know about the other.
    #[must_use]
    pub fn typing(&self) -> bool {
        self.panel_input.values().any(|i| i.editing.is_some())
    }

    /// Whether a point in physical window pixels lies over the old side panel.
    ///
    /// Used to keep pen strokes from painting underneath the UI. egui's own pointer handling
    /// cannot do this for us because it never sees pen input.
    ///
    /// **The workspace answers for itself** — see [`crate::workspace::Workspace::takes_point`].
    /// This used to hold a cached copy of where the canvas was, and the copy is what went wrong.
    pub fn blocks_point(&self, x: f64, y: f64) -> bool {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "window coordinates, which are far inside f32"
        )]
        let (x, y) = (x as f32, y as f32);
        self.occupied.contains(egui::pos2(x, y))
    }

    /// Build the panel, render it over the frame, and apply any edits to `brush`.
    ///
    /// The returned [`Outcome`] carries both whether egui wants another frame soon
    /// -- which the caller **must** honor, since painting is demand-driven and egui
    /// is only interactive while frames keep coming -- and any action the panel
    /// requested.
    #[must_use]
    // Eight, and the shape is already the fix: `Status` exists because this grew past the lint
    // once before. Splitting further would mean a struct per call rather than per concern, which
    // is bookkeeping rather than clarity -- and the workspace argument goes away entirely when
    // the old panel does.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        window: &Window,
        gpu: Overlay<'_>,
        brush: &mut Brush,
        text: Option<&mut openpaint_core::TextBlock>,
        view: &View,
        status: Status<'_>,
        mut workspace: Option<&mut crate::workspace::Workspace>,
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
        let mut text_action = None;
        let mut brush_action = None;
        let mut preset_name = self.preset_name.clone();
        let mut transform_action = None;
        let mut transform_state = status.transform;
        let mut lock_aspect = transform_state.is_some_and(|t| t.lock_aspect);
        let mut kernel = status.kernel;
        let mut text_changed = false;
        let mut text = text;
        let mut tool_action = None;
        let mut confirm_choice = None;
        let mut recovery_choice = None;
        let mut select_action = None;
        let mut command: Option<Command> = None;
        let mut wand = status.wand;

        let mut panel_rect = egui::Rect::NOTHING;
        let mut panel_canvas: Option<(f32, f32, f32, f32)> = None;
        // Taken for the duration of the frame because `self.ctx.run` has `self` borrowed, and put
        // back the moment it does not.
        let mut panel_input = std::mem::take(&mut self.panel_input);
        let mut menu_open = self.menu_open;
        let mut pick_open = self.pick_open;
        let mut wheel_shape = self.wheel_shape;
        let mut wheel_hold = self.wheel_hold;
        let output = self.ctx.run(input, |ctx| {
            // The panel workspace, when it is switched on. Drawn *instead of* the side panel
            // rather than beside it: they are two answers to the same question, and showing both
            // would put the same controls on screen twice.
            if let Some(ws) = workspace.as_deref_mut() {
                let screen = ctx.screen_rect();
                let area = crate::layout::Rect::new(
                    screen.min.x,
                    screen.min.y,
                    screen.width(),
                    screen.height(),
                );
                // Read once, before the closure borrows what they came from.
                let preset_name_now = preset_name.clone();
                let mut show_panel_list = false;
                // **Which panel asked, as well as where.** A popup belongs to the panel that
                // opened it, because the panel is what draws the inside of it: the workspace
                // paints the frame and calls `contents(panel, .., Place::Popup)`. This used to
                // name `MENU` unconditionally, so every dropdown in the application -- blend mode,
                // font family, alignment, every response source -- opened a popup owned by the
                // menu bar, which had nothing to draw in it. An empty box, and no way to choose a
                // blend mode at all. Hardcoding one panel's identity into shared machinery is the
                // §1c mistake in a different costume, and this is the shape that cannot make it.
                let mut menu_request: Option<(crate::layout::PanelId, crate::layout::Rect, (f32, f32), Anchor)> =
                    None;
                let mut close_menu = false;
                let mut show_settings = false;
                // Copied out because `ws` is borrowed for the whole of `show`, and the panel that
                // wants the theme runs inside it.
                let theme = ws.theme;
                ws.show(ctx, area, |panel, ui, direction, place| {
                    if let Some(picked) =
                        workspace_panel(panel, ui, brush, &mut color_srgb, &status,
                            &mut Painting {
                                theme: &theme,
                                direction,
                                input: panel_input.entry(panel.0).or_default(),
                                menu: &mut menu_open,
                                pick: &mut pick_open,
                                extend_by: extend_amount,
                                preset_name: &preset_name_now,
                                ctx,
                                wheel_shape: &mut wheel_shape,
                                wheel_hold: &mut wheel_hold,
                            },
                            place,
                        )
                    {
                        match picked {
                            // Both: choosing a paint tool must also put any selection tool down,
                            // or the pen would still be lassoing while the rail says Brush.
                            Picked::Paint(t) => {
                                tool_action = Some(t);
                                // `Stop`, not `None`: put the tool away and leave the selection
                                // alone. Switching to the brush to paint *inside* a lasso is the
                                // whole colouring workflow (§4q), so deselecting here would undo
                                // the thing the artist just did.
                                select_action = Some(SelectAction::Stop);
                            }
                            Picked::Select(t) => select_action = Some(SelectAction::Use(t)),
                            // Not applied here: `ws` is borrowed for the whole of `show`, so the
                            // toggle is remembered and done the moment that borrow ends.
                            Picked::PanelList => show_panel_list = true,
                            Picked::Layer(a) => layer_action = Some(a),
                            Picked::Selection(a) => select_action = Some(a),
                            Picked::Command(c) => command = Some(c),
                            // Deferred like the panel list, and for the same reason: `ws` is
                            // borrowed for the whole of `show`, and this is asked for from inside
                            // it.
                            Picked::OpenMenu { at, size, side } => {
                                menu_request = Some((panel, at, size, side));
                            }
                            Picked::CloseMenu => close_menu = true,
                            Picked::Settings => show_settings = true,
                            Picked::Page(a) => page_action = Some(a),
                            Picked::Transform(a) => transform_action = Some(a),
                            Picked::TransformSet(t) => {
                                lock_aspect = t.lock_aspect;
                                kernel = t.kernel;
                                transform_state = Some(t);
                            }
                            Picked::Crop(a) => crop_action = Some(a),
                            Picked::Extend(side, by) => extend = Some((side, by)),
                            Picked::ExtendBy(by) => extend_amount = by,
                            Picked::Trim => trim = true,
                            Picked::Text(a) => text_action = Some(a),
                            Picked::TextChanged => text_changed = true,
                            Picked::TextSet(block) => {
                                if let Some(held) = text.as_deref_mut() {
                                    *held = block;
                                }
                                // Same flag the old panel set: `apply_text_edit` takes the block
                                // back, records the undo step and derives the pixels again.
                                text_changed = true;
                            }
                            Picked::Brush(a) => brush_action = Some(a),
                            Picked::PresetName(name) => preset_name = name,
                            Picked::Wand(w) => wand = w,
                        }
                    }
                });
                if show_panel_list {
                    ws.open_panel_list();
                }
                if close_menu {
                    ws.close_popup();
                }
                if show_settings {
                    ws.open_settings();
                }
                if let Some((panel, at, size, side)) = menu_request {
                    ws.open_popup_for(panel, at, size, side, area);
                }
                // A menu whose popup has been dismissed some other way -- a press elsewhere,
                // Escape -- must not leave its button lit claiming to be open.
                if menu_open.is_some() && !ws.popup_is_for(crate::workspace::MENU) && menu_request.is_none() {
                    menu_open = None;
                }
                let scale = ctx.pixels_per_point();
                let px = |r: crate::layout::Rect| (r.x * scale, r.y * scale, r.w * scale, r.h * scale);
                // Where the *renderer* draws, which still needs an answer with the canvas panel
                // closed, and the whole surface is the only honest fallback. Where the *pen* may
                // go is a different question, and the workspace is asked it directly.
                // Before the return below, because the workspace path never reaches the old
                // panel's end -- which is exactly how these came to be invisible.
                let answered = prompts(ctx, status.recovery, status.confirm);
                recovery_choice = answered.0.or(recovery_choice);
                confirm_choice = answered.1.or(confirm_choice);
                if std::env::var_os("OPENPAINT_TRACE_INPUT").is_some() {
                    println!(
                        "ui: screen {:?} scale {scale} canvas {:?}",
                        (screen.width(), screen.height()),
                        ws.canvas_rect()
                    );
                }
                panel_canvas = Some(px(ws.canvas_rect().unwrap_or(crate::layout::Rect::new(
                    0.0,
                    0.0,
                    screen.width(),
                    screen.height(),
                ))));
                // Nothing is "the panel" any more, so the pen must be refused only where a panel
                // actually is -- which is everywhere except the canvas, and everywhere the
                // workspace draws on top of it.
                //
                // **With no canvas panel there is no canvas anywhere.** The workspace fills the
                // window with ground in that case, so the artwork is not on screen at all; falling
                // back to "the whole surface is canvas" let the pen paint on a drawing nobody
                // could see.
                return;
            }

            let panel = egui::SidePanel::left("brush-panel")
                .exact_width(PANEL_WIDTH)
                .show(ctx, |ui| {
                    // Scrollable, because the panel already stands taller than a window and
                    // every section below the fold was simply unreachable -- the speed readout
                    // was invisible on a laptop screen. `auto_shrink` off so it fills the panel
                    // instead of collapsing onto its content.
                    egui::ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        // Off, or a drag on any control inside scrolls the panel instead of
                        // reaching the control. Harmless for sliders, which egui claims first, and
                        // fatal for anything doing its own dragging.
                        .drag_to_scroll(false)
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
                                    (SelectTool::Move, "Move"),
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

                            // The saved brushes come first, because picking one is the common
                            // action and dialling sliders is the rare one. Nobody draws a page with
                            // a single brush, so this is the section that turns a demo into a tool.
                            if let Some(trouble) = status.preset_trouble {
                                ui.colored_label(egui::Color32::from_rgb(220, 120, 60), trouble);
                            }
                            for (i, p) in status.presets.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    if ui
                                        .button(&p.name)
                                        .on_hover_text(format!(
                                            "Size {:.0}, spacing {:.2}{}",
                                            p.brush.radius,
                                            p.brush.spacing,
                                            match &p.tip {
                                                openpaint_core::TipRef::Round => String::new(),
                                                openpaint_core::TipRef::File { path } =>
                                                    format!(", tip {path}"),
                                            }
                                        ))
                                        .clicked()
                                    {
                                        brush_action = Some(BrushAction::ApplyPreset(i));
                                    }
                                    if ui
                                        .small_button("\u{d7}")
                                        .on_hover_text("Forget this brush.")
                                        .clicked()
                                    {
                                        brush_action = Some(BrushAction::DeletePreset(i));
                                    }
                                });
                            }
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut preset_name)
                                        .hint_text("Name")
                                        .desired_width(120.0),
                                );
                                // Nameless presets are refused here rather than saved and puzzled
                                // over later: a row with no label is one nobody can pick on purpose.
                                if ui
                                    .add_enabled(
                                        !preset_name.trim().is_empty(),
                                        egui::Button::new("Save brush"),
                                    )
                                    .on_hover_text(
                                        "Keep these settings under that name. Saving over a name \
                                         you already used updates it.",
                                    )
                                    .on_disabled_hover_text("Give it a name first.")
                                    .clicked()
                                {
                                    brush_action = Some(BrushAction::SavePreset);
                                }
                            });
                            ui.label(
                                egui::RichText::new(
                                    "A brush keeps its size, edge, shape and response curves -- \
                                     but never your colour.",
                                )
                                .small()
                                .weak(),
                            );
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
                            ui.add(
                                egui::Slider::new(&mut brush.roundness, 0.02..=1.0)
                                    .text("Roundness")
                                    .custom_formatter(|v, _| match v {
                                        v if v >= 0.999 => "1.00 round".to_owned(),
                                        v => format!("{v:.2}"),
                                    }),
                            );
                            ui.add(
                                egui::Slider::new(&mut brush.angle, 0.0..=1.0)
                                    .text("Angle (turns)")
                                    .custom_formatter(|v, _| format!("{:.0}°", v * 360.0)),
                            );
                            ui.label(
                                egui::RichText::new(
                                    "A flattened dab is a chisel nib. Point Angle at Direction with a straight curve and it follows the stroke, which is where inked line weight comes from.",
                                )
                                .small()
                                .weak(),
                            );
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
                            ui.label(
                                egui::RichText::new(
                                    "What drives each parameter, and how. Pick an input, then shape the curve: drag a point, click to add one, right-click to remove. A flat curve means the input is ignored. The slider above is what you get at full input.",
                                )
                                .small()
                                .weak(),
                            );
                            // Stacked rather than side by side. Side by side put the right-hand
                            // editor's top-right corner under the scroll bar's hover zone, which
                            // floats over the content -- so the one control at (1, 1) could not be
                            // grabbed. Stacking keeps every editor at the left margin, well clear
                            // of it, and leaves room for larger boxes, which are easier to aim at.
                            for (name, response) in brush.responses_mut() {
                                response_editor(ui, name, response);
                                ui.add_space(2.0);
                            }

                            ui.separator();
                            ui.label(
                                egui::RichText::new("Tip").strong(),
                            );
                            if let Some(stamp) = brush.tip.stamp() {
                                ui.label(format!(
                                    "Bitmap, {}x{}",
                                    stamp.width(),
                                    stamp.height()
                                ));
                                ui.label(
                                    egui::RichText::new(
                                        "A bitmap tip carries its own edge, so hardness and the \
                                         edge profile do not apply to it.",
                                    )
                                    .small()
                                    .weak(),
                                );
                                if ui.button("Back to a round tip").clicked() {
                                    brush.tip = openpaint_core::dab::Tip::default();
                                }
                            } else {
                                ui.label(
                                    egui::RichText::new(
                                        "The dab's edge profile: how coverage falls from the \
                                         solid core out to the rim. Not driven by anything -- its \
                                         axis is distance within the dab, not an input. A \
                                         straight line is the plain ramp; bowing it out makes a \
                                         marker, bowing it in makes an airbrush.",
                                    )
                                    .small()
                                    .weak(),
                                );
                                if let Some(falloff) = brush.tip.falloff_mut() {
                                    curve_editor(ui, "Edge profile", falloff);
                                }
                            }
                            if ui
                                .button("Load brush tip\u{2026}")
                                .on_hover_text(
                                    "A PNG of the mark a single dab makes. Drawn on transparent \
                                     or drawn in black on white -- either way, what looks like \
                                     ink is ink.",
                                )
                                .clicked()
                            {
                                brush_action = Some(BrushAction::LoadTip);
                            }

                            ui.separator();
                            ui.add(egui::Slider::new(&mut brush.flow, 0.0..=1.0).text("Flow"));
                            ui.add(egui::Slider::new(&mut brush.opacity, 0.0..=1.0).text("Opacity"));
                            ui.label(
                                egui::RichText::new(
                                    "Direction = paint per dab. Opacity = ceiling for the whole \
                                     stroke. Set direction low and opacity mid to see build-up \
                                     stop at the ceiling; lift and stroke again to go darker.",
                                )
                                .small()
                                .weak(),
                            );

                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.label("Color");
                                ui.color_edit_button_srgb(&mut color_srgb);
                                if ui
                                    .small_button("+")
                                    .on_hover_text("Keep this colour with the document.")
                                    .clicked()
                                {
                                    brush_action = Some(BrushAction::SaveColor);
                                }
                            });

                            ui.separator();
                            // The swatches. Saved *with the document*, because a comic's palette
                            // is a property of the comic: the skin tone that has to match on page
                            // forty is the one from page one.
                            if !status.palette.is_empty() {
                                ui.horizontal_wrapped(|ui| {
                                    for (i, rgb) in status.palette.iter().enumerate() {
                                        let colour =
                                            egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(18.0, 18.0),
                                            egui::Sense::click(),
                                        );
                                        ui.painter().rect_filled(rect, 2.0_f32, colour);
                                        // A hairline border, so a white swatch on a light panel is
                                        // still a swatch rather than a gap.
                                        ui.painter().rect_stroke(
                                            rect,
                                            2.0_f32,
                                            egui::Stroke::new(
                                                1.0_f32,
                                                egui::Color32::from_black_alpha(90),
                                            ),
                                        );
                                        if response.clicked() {
                                            brush_action = Some(BrushAction::UseColor(*rgb));
                                        }
                                        // Right-click to forget, so the row stays swatches rather
                                        // than swatches interleaved with delete buttons.
                                        if response.secondary_clicked() {
                                            brush_action = Some(BrushAction::ForgetColor(i));
                                        }
                                        response.on_hover_text(format!(
                                            "#{:02X}{:02X}{:02X} \u{2014} click to use, \
                                             right-click to forget",
                                            rgb[0], rgb[1], rgb[2]
                                        ));
                                    }
                                });
                            }
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
                            ui.heading("Transform");
                            if let Some(state) = transform_state.as_mut() {
                                let t = &mut state.transform;
                                ui.label(
                                    egui::RichText::new(
                                        "On the canvas: drag inside to move, a handle to scale, \
                                         just outside to rotate. Enter applies, Esc puts it back.",
                                    )
                                    .small()
                                    .weak(),
                                );
                                let mut uniform = lock_aspect;
                                ui.horizontal(|ui| {
                                    ui.label("Scale");
                                    let mut x = t.scale.0 * 100.0;
                                    if ui
                                        .add(egui::DragValue::new(&mut x).speed(0.5).suffix("%"))
                                        .changed()
                                    {
                                        t.scale.0 = x / 100.0;
                                        if uniform {
                                            t.scale.1 = t.scale.0;
                                        }
                                    }
                                    let mut y = t.scale.1 * 100.0;
                                    if ui
                                        .add(egui::DragValue::new(&mut y).speed(0.5).suffix("%"))
                                        .changed()
                                    {
                                        t.scale.1 = y / 100.0;
                                        if uniform {
                                            t.scale.0 = t.scale.1;
                                        }
                                    }
                                    if ui
                                        .checkbox(&mut uniform, "Lock")
                                        .on_hover_text(
                                            "Keep the two axes equal, here and when dragging a \
                                             corner.",
                                        )
                                        .changed()
                                        && uniform
                                    {
                                        t.scale.1 = t.scale.0;
                                    }
                                });
                                lock_aspect = uniform;
                                let mut degrees = t.rotation.to_degrees();
                                if ui
                                    .add(
                                        egui::Slider::new(&mut degrees, -180.0..=180.0)
                                            .text("Rotation")
                                            .suffix("\u{b0}"),
                                    )
                                    .changed()
                                {
                                    t.rotation = degrees.to_radians();
                                }
                                ui.horizontal(|ui| {
                                    if ui.button("Flip horizontally").clicked() {
                                        t.scale.0 = -t.scale.0;
                                    }
                                    if ui.button("Flip vertically").clicked() {
                                        t.scale.1 = -t.scale.1;
                                    }
                                });
                                ui.horizontal(|ui| {
                                    if ui.button("Apply").clicked() {
                                        transform_action = Some(TransformAction::Apply);
                                    }
                                    if ui.button("Cancel").clicked() {
                                        transform_action = Some(TransformAction::Cancel);
                                    }
                                });
                                state.kernel = kernel;
                                state.lock_aspect = lock_aspect;
                            } else {
                                ui.add_enabled_ui(status.has_selection, |ui| {
                                    if ui
                                        .button("Transform selection")
                                        .on_hover_text(
                                            "Lift the selection so it can be scaled and rotated \
                                             before it lands.",
                                        )
                                        .clicked()
                                    {
                                        transform_action = Some(TransformAction::Begin);
                                    }
                                });
                            }
                            ui.horizontal(|ui| {
                                ui.label("Resampling");
                                egui::ComboBox::from_id_salt("transform-kernel")
                                    .selected_text(kernel.label())
                                    .show_ui(ui, |ui| {
                                        for option in openpaint_core::Kernel::ALL {
                                            ui.selectable_value(
                                                &mut kernel,
                                                option,
                                                option.label(),
                                            );
                                        }
                                    });
                            });
                            if let Some(state) = transform_state.as_mut() {
                                state.kernel = kernel;
                            }

                            ui.separator();
                            ui.heading("Text");
                            if let Some(block) = text.as_deref_mut() {
                                text_changed |= text_editor(
                                    ui,
                                    block,
                                    status.font_families,
                                    status.font_substituted,
                                );
                                ui.separator();
                                if ui
                                    .button("Convert to raster layer")
                                    .on_hover_text(
                                        "Keeps the pixels and stops re-deriving them, so the \
                                         layer can be painted on. The text stops existing \
                                         as text; undo brings it back, retyping does not.",
                                    )
                                    .clicked()
                                {
                                    text_action = Some(TextAction::ConvertToRaster);
                                }
                            } else {
                                ui.label(
                                    egui::RichText::new(
                                        "The active layer is not a text layer. A text layer keeps \
                                         the words rather than the pixels, so a caption stays \
                                         retypeable — and cannot be painted on.",
                                    )
                                    .small()
                                    .weak(),
                                );
                            }
                            ui.horizontal(|ui| {
                                if ui.button("Add text layer").clicked() {
                                    text_action = Some(TextAction::AddLayer);
                                }
                                if ui
                                    .button("Load font file\u{2026}")
                                    .on_hover_text(
                                        "Use a .ttf or .otf without installing it. Available for \
                                         this session.",
                                    )
                                    .clicked()
                                {
                                    text_action = Some(TextAction::LoadFontFile);
                                }
                            });

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
                                    "Navigation is mouse and keyboard for now -- these are \
                                     shortcuts, not gestures.",
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
                                        ui.horizontal(|ui| {
                                            if ui
                                                .button("Duplicate")
                                                .on_hover_text(
                                                    "Copy this layer, pixels and all, above                                                      itself.",
                                                )
                                                .clicked()
                                            {
                                                layer_action = Some(LayerAction::Duplicate(index));
                                            }
                                            // Nothing below means nothing to merge into, and a
                                            // button that explains itself beats one that does
                                            // nothing when pressed.
                                            if ui
                                                .add_enabled(
                                                    index > 0,
                                                    egui::Button::new("Merge down"),
                                                )
                                                .on_hover_text(
                                                    "Fold this layer into the one below, as it                                                      looks now. Undoable.",
                                                )
                                                .on_disabled_hover_text(
                                                    "There is no layer below this one.",
                                                )
                                                .clicked()
                                            {
                                                layer_action = Some(LayerAction::MergeDown(index));
                                            }
                                        });
                                    }
                                });
                            }
                            ui.label(
                                egui::RichText::new(
                                    "Multiply darkens what is under it, Screen lightens. Deleting \
                                     and merging are undoable -- otherwise they would not be \
                                     offered. Duplicating is not, because the way back is to \
                                     delete the copy.",
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
                            // The step-6 readout: how many samples the input path is actually
                            // handing us, and how far the pen moved between them.
                            match status.perf.rate {
                                Some(hz) => {
                                    let verdict = if hz >= 120.0 {
                                        "full pen rate"
                                    } else if hz >= 90.0 {
                                        "some samples lost"
                                    } else {
                                        "about one a frame -- samples are being dropped"
                                    };
                                    ui.label(format!("Pen {hz:.0} samples/s ({verdict})"));
                                }
                                None => {
                                    ui.label("Pen rate -- draw for a second");
                                }
                            }
                            if let Some((mean, peak)) = status.perf.step {
                                // Both, because either alone misleads. Page pixels are what the
                                // brush engine interpolates across and so what decides whether a
                                // curve facets; screen pixels are what the hand actually moved.
                                // Printing only the first made 40 px look alarming when the canvas
                                // was simply zoomed out — the figure was right and unreadable.
                                let zoom = view.scale().max(f32::MIN_POSITIVE);
                                ui.label(format!(
                                    "Step {mean:.1} page px, peak {peak:.1} ({:.1} / {:.1} on \
                                     screen at {:.0}%)",
                                    mean * zoom,
                                    peak * zoom,
                                    zoom * 100.0
                                ));
                            }
                            ui.label(
                                egui::RichText::new(
                                    "A tablet reports around 200 times a second. Near that means \
                                     nothing is being lost; near 60 means we are only seeing one \
                                     sample a frame, which is what makes a fast curve come out \
                                     faceted.",
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
                    crate::workspace::artwork_order(),
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

            let answered = prompts(ctx, status.recovery, status.confirm);
            recovery_choice = answered.0.or(recovery_choice);
            confirm_choice = answered.1.or(confirm_choice);

        });

        // Paint the crop rectangle and the transform box over the canvas. Deliberately painted,
        // not built from widgets. Two reasons, and the second is the one that would still hold if
        // the first were fixed: the handles live in *page* space, so they move with the canvas's
        // own pan, zoom and rotation; and egui only ever sees the pointer through Windows'
        // synthesised mouse events (Q14), which are coalesced and lag the real pen -- fine for
        // pressing a button, wrong for dragging a handle. Input is handled in the app's own path.
        //
        // One call each, through one function: the two boxes are the same drawing, and the day
        // one grew a rotation handle the other would silently not have it.
        for (id, overlay) in [
            ("crop-overlay", status.crop),
            ("transform-box", status.transform_box),
        ] {
            let Some(overlay) = overlay else {
                continue;
            };
            let painter = self.ctx.layer_painter(egui::LayerId::new(
                crate::workspace::artwork_order(),
                egui::Id::new(id),
            ));
            paint_handle_box(&painter, overlay, self.ctx.pixels_per_point());
        }

        // The selection boundary. Two strokes, dark under light, for the same reason the crop
        // outline has two: it has to stay legible over white paper and over black ink without
        // knowing which is there. Static rather than animated -- marching ants are a later
        // refinement, and a still outline is not wrong, only quieter.
        if !status.selection.is_empty() {
            let ppp = self.ctx.pixels_per_point();
            let painter = self.ctx.layer_painter(egui::LayerId::new(
                crate::workspace::artwork_order(),
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
        // crop handles are: it lives in canvas space and has to track the pen without the lag of
        // a synthesised mouse event (Q14).
        if let Some(cursor) = status.brush_cursor {
            let ppp = self.ctx.pixels_per_point();
            let painter = self.ctx.layer_painter(egui::LayerId::new(
                crate::workspace::artwork_order(),
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
        self.preset_name.clone_from(&preset_name);

        // Record which pixels the *panel* owns, in physical coordinates, for `blocks_point` and
        // for the canvas inset.
        //
        // Deliberately the panel's own rect, not `used_rect()`. `used_rect` is the union of
        // everything egui drew, so a centred floating window -- the unsaved-changes prompt --
        // made it span half the screen and shoved the canvas sideways. The inset means "how much
        // of the left edge the panel covers", and only the panel can answer that.
        self.panel_input = panel_input;
        self.menu_open = menu_open;
        self.pick_open = pick_open;
        self.wheel_shape = wheel_shape;
        self.wheel_hold = wheel_hold;
        let scale = self.ctx.pixels_per_point();
        let used = panel_rect;
        self.occupied = egui::Rect::from_min_max(
            egui::pos2(used.min.x * scale, used.min.y * scale),
            egui::pos2(used.max.x * scale, used.max.y * scale),
        );
        self.canvas_viewport = panel_canvas.unwrap_or((
            self.occupied.max.x.max(0.0),
            0.0,
            (size_px[0] as f32 - self.occupied.max.x).max(1.0),
            size_px[1] as f32,
        ));
        // In the workspace the canvas is a panel, so everything that is *not* it belongs to the
        // UI. Expressed as the complement rather than as a list of panel rectangles: one
        // answer cannot drift from another, and a new panel needs no bookkeeping here.

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
            repaint_after: output
                .viewport_output
                .get(&ViewportId::ROOT)
                .map(|v| v.repaint_delay)
                // Zero is "now", which `wants_repaint` already carries; `MAX` is egui's way of
                // saying "nothing scheduled". Neither is a deadline.
                .filter(|d| !d.is_zero() && *d < std::time::Duration::MAX),
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
            text_changed,
            text: text_action,
            brush: brush_action,
            preset_name,
            transform: transform_action,
            transform_state,
            command,
        }
    }
}
