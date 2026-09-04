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
//! To stop strokes landing "through" the UI, the *workspace* is asked which points belong to it
//! --- see [`crate::workspace::Workspace::takes_point`]. That check is needed for the pen
//! specifically, because egui's own pointer-capture logic never sees it, and it is asked of the
//! workspace rather than of a rectangle cached here so there is only one answer to keep true.

use crate::editor::Tool;
use crate::workspace::{Anchor, Place};
use egui::ViewportId;
use openpaint_core::{Blend, Brush, Layer};
use winit::window::Window;

use crate::editor::DEFAULT_EXTEND;
use crate::renderer::Overlay;
use openpaint_core::Side;

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
    /// Bring a picture into the open document as a layer.
    PlaceImage,
    /// The selected pixels to the system clipboard.
    Copy,
    /// The same, and off the layer.
    Cut,
    /// Whatever the system clipboard holds, onto a new layer.
    Paste,
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
    /// Set a layer aside, or bring it back.
    SetLocked { index: usize, locked: bool },
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
    /// The export dialog's settings while it is up, and `None` while it is not.
    ///
    /// `Option` rather than a separate flag beside it: "the dialog is open" and "what it is set
    /// to" are one fact, and two fields would be two chances to disagree about it.
    pub export: Option<crate::export::Choices>,
    /// Selection boundary in screen space, as closed loops.
    pub selection: &'a [Vec<[f32; 2]>],
    /// Which selection tool is active, if any.
    pub select_tool: Option<SelectTool>,
    /// Whether there is a selection to act on.
    pub has_selection: bool,
    /// Wand settings: colour tolerance, how far to grow the region, and whether a click fills.
    pub wand: WandSettings,
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
///
/// Every variant has a producer now. This carried an `expect(dead_code)` while the panels were
/// being written across, worded so that it would stop compiling the moment the last one landed
/// rather than sit there forgiving whatever was left behind. It stopped compiling; the port is
/// finished, and `Picked::TextChanged` -- which never had a producer and never would -- went with
/// it, since a caption's edit arrives as `TextSet` carrying the caption.
#[derive(Clone, Debug, PartialEq)]
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

/// What every panel is handed in a test.
///
/// **One definition, in the file that owns the type.** Two panels' tests both need a status with
/// something in every field, and a second literal beside the first is a second thing to update
/// whenever a field is added -- the one that goes stale being whichever the author was not
/// looking at. The borrowed parts are arguments because a `&'a [Layer]` has to come from
/// somewhere that outlives the call; `crate::screenshot::sample_document` is where the rest of
/// the tests get theirs.
#[cfg(test)]
impl<'a> Status<'a> {
    pub(crate) fn sample(
        layers: &'a [Layer],
        palette: &'a [[u8; 3]],
        presets: &'a [openpaint_core::BrushPreset],
        font_families: &'a [String],
    ) -> Self {
        Self {
            history: (3, 1, 2 * 1024 * 1024),
            message: Some("something happened"),
            page_size: (1200, 1600),
            crop: None,
            crop_rect: None,
            residency: (12, 4),
            spilled: 0,
            traffic: (7, 9),
            layers,
            active_layer: 3,
            pages: (3, 0),
            tool: Tool::Brush,
            confirm: None,
            brush_cursor: None,
            perf: crate::perf::PerfSnapshot::default(),
            recovery: None,
            palette,
            presets,
            preset_trouble: Some("the brush library could not be written"),
            font_families,
            font_substituted: Some("Some Missing Face"),
            transform: None,
            transform_box: None,
            kernel: openpaint_core::Kernel::default(),
            autosave: "saved a moment ago",
            export: None,
            selection: &[],
            select_tool: Some(SelectTool::Wand),
            has_selection: true,
            wand: WandSettings::default(),
        }
    }
}

/// How wide a prompt is, in ems of the theme's body text.
///
/// Typography rather than taste: a line much longer than thirty ems is hard to come back from at
/// the end of, and one much shorter chops a sentence into too many pieces. In ems so it follows
/// the theme's text size — a fixed width would be wrong the moment the type changed, and §5a is
/// explicit that a number like this is never a constant in pixels.
const PROMPT_EMS: f32 = 30.0;

/// The prompts that stop everything until they are answered.
///
/// **Drawn like the rest of the UI, because they are part of it.** These were raw `egui::Window`s
/// with raw buttons long after every panel had been ported: they ignored the theme, looked like
/// nothing else on screen, and had to be written into the control atlas through a reporting path
/// of their own. Now they are described in [`crate::prompt`] as [`Control`](crate::panel_ui::Control)s
/// and drawn by [`crate::panel_draw`], which is the same layer, the same theme and the same atlas
/// as every panel.
///
/// **One at a time.** A modal question is modal: two of them stacked would leave the artist
/// answering the top one to find another underneath. Recovered work is asked first because it is
/// asked at start-up, before there is a document to have unsaved changes in; anything asked while
/// it is up waits its turn rather than being lost, since the shell holds it until it is answered.
///
/// They used to sit at the end of the old side panel's branch, and the workspace returned before
/// reaching it — so in the workspace they were never drawn at all, while `Status::recovery` and
/// `Status::confirm` went on refusing every pen stroke (`decide_capture`). A leftover recovery
/// file from any previous crash therefore made the brush do nothing, for good, with nothing on
/// screen to say why. Reported as "most things do not work".
fn prompts(
    ctx: &egui::Context,
    theme: &crate::theme::Theme,
    status: &Status<'_>,
    input: &mut crate::panel_draw::PanelInput,
) -> Answered {
    use crate::prompt::{Answer, Ask};
    let mut answered = Answered::default();
    // **Recovered work first, then the unsaved question, then the export dialog.** The order is
    // the order they become possible: recovery is asked before there is a document to have
    // changes in, and an export cannot be asked for while either is up, because the workspace
    // that offers it is inert. Only one is ever drawn.
    let asked = status
        .recovery
        .map(Ask::Recovered)
        .or_else(|| status.confirm.map(Ask::Unsaved))
        .or_else(|| {
            status.export.as_ref().map(|choices| Ask::Export {
                choices,
                pages: status.pages.0,
                page: status.page_size,
            })
        });
    let Some(ask) = asked else {
        return answered;
    };
    let (answer, changes) = ask_on_screen(ctx, theme, ask, input);
    if let Ask::Export { .. } = ask {
        // Applied to a copy and handed back, the way the transform box and the wand hand back
        // what they now hold: the modal owns no state, and the shell owns exactly one copy of it.
        let mut now = status.export.unwrap_or_default();
        if changes.iter().any(|c| crate::export::apply(&mut now, c)) {
            answered.export_set = Some(now);
        }
    }
    match answer {
        Some(Answer::Recover) => answered.recovery = Some(RecoveryChoice::Recover),
        Some(Answer::DiscardRecovered) => answered.recovery = Some(RecoveryChoice::Discard),
        Some(Answer::SaveFirst) => answered.confirm = Some(ConfirmChoice::SaveFirst),
        Some(Answer::DiscardChanges) => answered.confirm = Some(ConfirmChoice::Discard),
        Some(Answer::Cancel) => answered.confirm = Some(ConfirmChoice::Cancel),
        Some(Answer::Export) => answered.export = Some(ExportChoice::Go),
        Some(Answer::StopExport) => answered.export = Some(ExportChoice::Stop),
        None => {}
    }
    answered
}

/// What a modal produced in one frame.
#[derive(Default)]
struct Answered {
    recovery: Option<RecoveryChoice>,
    confirm: Option<ConfirmChoice>,
    export: Option<ExportChoice>,
    /// The export settings as the dialog now holds them, when one of them moved.
    export_set: Option<crate::export::Choices>,
}

/// What the export dialog was told to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportChoice {
    /// Ask where to put it, then write it.
    Go,
    /// Put the dialog away.
    Stop,
}

/// Draw one prompt, centred over a dimmed workspace, and report what was pressed.
///
/// The box is sized to what it is about to say rather than to a figure picked here: the words are
/// laid out at the width they will get, and the height comes from where they actually land. A
/// prompt whose sentence is one line longer must grow, not clip — the sentence is the whole point
/// of it.
fn ask_on_screen(
    ctx: &egui::Context,
    theme: &crate::theme::Theme,
    ask: crate::prompt::Ask<'_>,
    input: &mut crate::panel_draw::PanelInput,
) -> (Option<crate::prompt::Answer>, Vec<crate::panel_ui::Change>) {
    use crate::layout::Rect;
    use crate::panel_ui::{extent, place, Direction};
    let m = &theme.metrics;
    let p = theme.palette;
    let screen = ctx.screen_rect();
    let to_egui = |r: Rect| egui::Rect::from_min_size(egui::pos2(r.x, r.y), egui::vec2(r.w, r.h));
    let colour = |c: crate::theme::Color| egui::Color32::from_rgb(c.0[0], c.0[1], c.0[2]);

    let words = ask.words();
    let body = ask.body();
    let answers = ask.answers();
    // Measured by the thing that draws them, so the room made and the room used are one answer.
    let text_of = |c: &crate::panel_ui::Control| crate::panel_draw::text_width(ctx, m.body, c);
    let tall_of =
        |c: &crate::panel_ui::Control, w: f32| crate::panel_draw::wrapped_height(ctx, m.body, c, w);

    // Never wider than the window it is centred in, and never so wide the sentence is hard to
    // read. A small window wins, because a prompt with its buttons off the edge cannot be
    // answered at all.
    let width = (m.body * PROMPT_EMS).min(screen.width() - m.padding * 4.0);
    let inner = (width - m.padding * 2.0).max(0.0);
    // **Measured in a box of no height on purpose.** A row of controls centres itself in the
    // height it is given, so measuring one inside a box as tall as the window measures the
    // window: the buttons came out halfway down the screen and the prompt was drawn tall enough
    // to hold them. Nothing is centred in nothing, so what comes back is the block itself.
    let measure = |controls: &[crate::panel_ui::Control], direction| {
        let origin = Rect::new(0.0, 0.0, inner, 0.0);
        let laid = place(controls, origin, m, direction, &text_of, &tall_of);
        extent(&laid, origin).1
    };
    let words_h = measure(&words, Direction::Column);
    // A question with nothing to set spends nothing on the room for it, including the gap.
    let body_h = if body.is_empty() {
        0.0
    } else {
        measure(&body, Direction::Column) + m.padding
    };
    // **Across if they fit, stacked if they do not.** `Auto` decides once for the whole row, which
    // is exactly the question a set of answers asks: three buttons side by side in a narrow window
    // would each be too small to hit, and three stacked in a wide one would look like a list.
    let answers_h = measure(&answers, Direction::Auto);
    let height = m.header + m.padding * 2.0 + words_h + body_h + m.padding + answers_h;

    let box_rect = Rect::new(
        (screen.width() - width) / 2.0,
        ((screen.height() - height) / 2.0).max(0.0),
        width,
        height.min(screen.height()),
    );

    let layer = egui::LayerId::new(crate::workspace::prompt_order(), egui::Id::new("prompt"));
    let painter = ctx.layer_painter(layer);
    // **The workspace goes quiet behind it.** Toward the theme's own ground rather than to grey:
    // the ground is what the window shows where nothing is, so dimming toward it says "there is
    // nothing here for you" in the palette's own voice. The workspace is inert underneath as well
    // — see `workspace::Attention` — so this says what is true rather than covering for it.
    //
    // **A veil, not a curtain.** The unsaved-changes prompt asks a question *about the artwork*,
    // and hiding the drawing behind a heavy scrim would take away the one thing the artist is
    // deciding on. It has to read as waiting, not as gone — so the page stays plainly legible and
    // the ink on it stays ink.
    painter.rect_filled(
        screen,
        0.0,
        egui::Color32::from_rgba_unmultiplied(p.ground.0[0], p.ground.0[1], p.ground.0[2], 90),
    );
    painter.rect_filled(to_egui(box_rect), m.radius, colour(p.panel));
    painter.rect_stroke(
        to_egui(box_rect),
        m.radius,
        egui::Stroke::new(1.0_f32, colour(p.edge)),
    );
    // A header, drawn the way a panel's is: this is a window of the same UI, not a dialog from
    // somewhere else.
    let header = Rect::new(box_rect.x, box_rect.y, box_rect.w, m.header);
    painter.rect_filled(to_egui(header), m.radius, colour(p.header));
    painter.text(
        egui::pos2(header.x + m.padding, header.y + header.h / 2.0),
        egui::Align2::LEFT_CENTER,
        ask.title(),
        egui::FontId::proportional(m.label),
        colour(p.bright),
    );

    let region = |y: f32, h: f32| Rect::new(box_rect.x + m.padding, y, inner, h);
    let words_at = region(header.y + header.h + m.padding, words_h);
    let body_at = region(
        words_at.y + words_h + m.padding,
        (body_h - m.padding).max(0.0),
    );
    let answers_at = region(words_at.y + words_h + body_h + m.padding, answers_h);

    let draw = |name: &str,
                at: Rect,
                controls: &[crate::panel_ui::Control],
                direction,
                input: &mut crate::panel_draw::PanelInput| {
        let mut ui = egui::Ui::new(
            ctx.clone(),
            layer,
            egui::Id::new(("prompt", name)),
            egui::UiBuilder::new().max_rect(to_egui(at)),
        );
        ui.set_clip_rect(to_egui(at));
        crate::panel_draw::report_panel("prompt");
        crate::panel_draw::show(&mut ui, controls, theme, direction, input)
    };
    // The words hold nothing between frames: there is no slider to latch and the box is sized to
    // fit its own sentences, so there is nothing to scroll either. Somewhere to put the answer is
    // all `show` wants.
    let mut nothing_held = crate::panel_draw::PanelInput::default();
    draw(
        "words",
        words_at,
        &words,
        Direction::Column,
        &mut nothing_held,
    );
    // **The body gets the kept input, not the buttons.** A slider is the only control here that
    // has to be followed between frames -- it latches on the press and keeps the value under a
    // pointer that has slid off the row -- and there is one of those at a time on screen.
    let mut changes = if body.is_empty() {
        Vec::new()
    } else {
        draw("body", body_at, &body, Direction::Column, input)
    };
    let mut spare = crate::panel_draw::PanelInput::default();
    let pressed = draw("answers", answers_at, &answers, Direction::Auto, &mut spare);
    let answer = pressed.iter().find_map(|change| match change {
        crate::panel_ui::Change::Pressed(id) => ask.answer(*id),
        _ => None,
    });
    changes.extend(pressed);
    (answer, changes)
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
    /// What the export dialog was told to do, if it was told anything.
    pub export: Option<ExportChoice>,
    /// The export settings as the dialog now holds them.
    pub export_set: Option<crate::export::Choices>,
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
    /// Where the canvas may draw, in physical pixels.
    ///
    /// The canvas *panel's* rectangle, because the canvas is a panel and can be anywhere (§1c)
    /// — see [`crate::view::View::set_viewport`].
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
    ///
    /// **And by the section that panel is showing**, which is the second half of the key. A
    /// contextual panel shows the brush one moment and the wand the next; one entry between them
    /// would drop the artist halfway down a list they never scrolled and carry a half-typed preset
    /// name into a tolerance. `panels::section_of` decides which section is up, and it is the same
    /// answer the panel itself is drawn from. An ordinary panel is its own section, so its key is
    /// the pair it always was.
    panel_input: std::collections::HashMap<(u32, u32), crate::panel_draw::PanelInput>,
    /// Which menu the menu strip is drilled into, if any.
    menu_open: Option<u32>,
    /// Which control's dropdown is open, if any. Kept between frames, like `menu_open`.
    pick_open: Option<crate::panel_ui::ControlId>,
    /// Which of those were actually drawn on the last frame.
    ///
    /// **Because a field nobody can see does not have the caret.** `editing` is dropped only by
    /// the panel that draws the field, on the way out of it -- so a panel that stops being drawn
    /// keeps its half-typed text, which is right, and used to keep `typing()` saying *true*, which
    /// was not: every unmodified shortcut in the application stayed dead until that panel came
    /// back. Clicking into the brush's name box and then changing tab was enough, and a contextual
    /// panel makes it a press on the tool rail.
    ///
    /// Two facts, kept apart: the field remembers what was typed into it, and the *keyboard* is
    /// only claimed by a field that is on screen.
    drawn: std::collections::HashSet<(u32, u32)>,
    /// The prompt's half-finished gesture, kept for the same reason a panel's is.
    ///
    /// Its own, not one of `panel_input`'s: a prompt belongs to no panel, and filing it under one
    /// would mean the prompt and that panel sharing a latch.
    prompt_input: crate::panel_draw::PanelInput,
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
            canvas_viewport: (0.0, 0.0, 1.0, 1.0),
            extend_amount: DEFAULT_EXTEND,
            preset_name: String::new(),
            panel_input: std::collections::HashMap::new(),
            drawn: std::collections::HashSet::new(),
            prompt_input: crate::panel_draw::PanelInput::default(),
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

    /// The two settings the UI holds that nothing else can see.
    ///
    /// The colour wheel's shape and how much an extend adds are the shell's, not the document's,
    /// so no document state shows them and a screenshot is the only other evidence. Both were
    /// driven and neither was checked.
    #[must_use]
    pub fn settings(&self) -> (crate::colour_wheel::Shape, u32) {
        (self.wheel_shape, self.extend_amount)
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
        self.drawn.iter().any(|key| {
            self.panel_input
                .get(key)
                .is_some_and(|i| i.editing.is_some())
        })
    }

    /// Build the panel, render it over the frame, and apply any edits to `brush`.
    ///
    /// The returned [`Outcome`] carries both whether egui wants another frame soon
    /// -- which the caller **must** honor, since painting is demand-driven and egui
    /// is only interactive while frames keep coming -- and any action the panel
    /// requested.
    #[must_use]
    // Six, and the shape is already the fix: `Status` exists because this grew past the lint once
    // before. Splitting further would mean a struct per call rather than per concern, which is
    // bookkeeping rather than clarity.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        window: &Window,
        gpu: Overlay<'_>,
        brush: &mut Brush,
        text: Option<&mut openpaint_core::TextBlock>,
        status: Status<'_>,
        workspace: &mut crate::workspace::Workspace,
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
        let mut export_choice = None;
        let mut export_set = None;
        let mut select_action = None;
        let mut command: Option<Command> = None;
        let mut wand = status.wand;

        let mut panel_canvas: Option<(f32, f32, f32, f32)> = None;
        // Taken for the duration of the frame because `self.ctx.run` has `self` borrowed, and put
        // back the moment it does not.
        let mut panel_input = std::mem::take(&mut self.panel_input);
        // Rebuilt each frame rather than added to: a panel that has gone is a panel whose field no
        // longer has the keyboard, and that is the whole point of keeping the set.
        let mut drawn: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
        let mut prompt_input = std::mem::take(&mut self.prompt_input);
        let mut menu_open = self.menu_open;
        let mut pick_open = self.pick_open;
        let mut wheel_shape = self.wheel_shape;
        let mut wheel_hold = self.wheel_hold;
        let output = self.ctx.run(input, |ctx| {
            // The panel workspace, when it is switched on. Drawn *instead of* the side panel
            // rather than beside it: they are two answers to the same question, and showing both
            // would put the same controls on screen twice.
            {
                let ws = &mut *workspace;
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
                let mut menu_request: Option<(
                    crate::layout::PanelId,
                    crate::layout::Rect,
                    (f32, f32),
                    Anchor,
                )> = None;
                let mut close_menu = false;
                let mut show_settings = false;
                // Copied out because `ws` is borrowed for the whole of `show`, and the panel that
                // wants the theme runs inside it.
                let theme = ws.theme;
                // **A question being asked owns the pointer.** The workspace reads the pointer
                // out of egui itself, so drawing over it would not stop a press landing on a
                // panel underneath a prompt -- it has to be told.
                let attention = if status.recovery.is_some()
                    || status.confirm.is_some()
                    || status.export.is_some()
                {
                    crate::workspace::Attention::Elsewhere
                } else {
                    crate::workspace::Attention::Workspace
                };
                ws.show(ctx, area, attention, |panel, ui, direction, place| {
                    let key = (panel.0, crate::panels::section_of(panel, &status).0);
                    drawn.insert(key);
                    if let Some(picked) = workspace_panel(
                        panel,
                        ui,
                        brush,
                        &mut color_srgb,
                        &status,
                        &mut Painting {
                            theme: &theme,
                            direction,
                            input: panel_input.entry(key).or_default(),
                            menu: &mut menu_open,
                            pick: &mut pick_open,
                            extend_by: extend_amount,
                            preset_name: &preset_name_now,
                            ctx,
                            wheel_shape: &mut wheel_shape,
                            wheel_hold: &mut wheel_hold,
                        },
                        place,
                    ) {
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
                // **A menu's popup and a menu being open are one fact, and it is kept true in
                // both directions.**
                //
                // A popup dismissed some other way -- a press elsewhere, Escape -- must not leave
                // the menu's button lit claiming to be open.
                if menu_open.is_some()
                    && !ws.popup_is_for(crate::workspace::MENU)
                    && menu_request.is_none()
                {
                    menu_open = None;
                }
                // And a menu that has closed itself -- which is what choosing an item does -- must
                // not leave its popup standing. It did: the frame stayed with nothing drawn in it,
                // so every command reached from a menu left an empty dark box over the artwork
                // until something else happened to close it. Visible in `menu-new.png` for as long
                // as that shot has existed, and the same shape as the empty dropdowns reported
                // before.
                if menu_open.is_none()
                    && ws.popup_is_for(crate::workspace::MENU)
                    && menu_request.is_none()
                {
                    // Closed here rather than by setting `close_menu`: that flag is read a few
                    // lines above this, so setting it now would do nothing at all until something
                    // else happened to set it again. Clippy said so, and it was right -- the fix
                    // was dead the moment it was written, and the scenes passed anyway because
                    // none of them looks at an empty box.
                    ws.close_popup();
                }
                let scale = ctx.pixels_per_point();
                let px =
                    |r: crate::layout::Rect| (r.x * scale, r.y * scale, r.w * scale, r.h * scale);
                // Where the *renderer* draws, which still needs an answer with the canvas panel
                // closed, and the whole surface is the only honest fallback. Where the *pen* may
                // go is a different question, and the workspace is asked it directly.
                // Before the return below, because the workspace path never reaches the old
                // panel's end -- which is exactly how these came to be invisible.
                let answered = prompts(ctx, &theme, &status, &mut prompt_input);
                recovery_choice = answered.recovery.or(recovery_choice);
                confirm_choice = answered.confirm.or(confirm_choice);
                export_choice = answered.export.or(export_choice);
                export_set = answered.export_set.or(export_set);
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
                // **What the application last said, over the artwork.**
                //
                // This used to be drawn at the foot of the side panel, and the side panel was the
                // only place it appeared. Taking that away would have taken with it every "refused
                // out loud" message the design leans on (DECISIONS 6b) -- "That colour is already
                // in the palette", "A document needs at least one page", "Page is already
                // 2048x65536" -- and a refusal nobody is told about is indistinguishable from a
                // button that does not work.
                //
                // Against the canvas rather than against a panel edge, because there is no panel
                // edge any more; the workspace says where the artwork is.
                if let Some(msg) = status.message {
                    let over = ws
                        .canvas_rect()
                        .unwrap_or(crate::layout::Rect::new(0.0, 0.0, area.w, area.h));
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
                    let at = egui::pos2(over.x + 12.0, over.y + over.h - size.y - 12.0);
                    painter.rect_filled(
                        egui::Rect::from_min_size(at, size),
                        4.0,
                        egui::Color32::from_black_alpha(190),
                    );
                    painter.galley(at + pad, text, egui::Color32::WHITE);
                }
            }
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

        // Put back the state the frame borrowed.
        self.panel_input = panel_input;
        self.drawn = drawn;
        self.prompt_input = prompt_input;
        self.menu_open = menu_open;
        self.pick_open = pick_open;
        self.wheel_shape = wheel_shape;
        self.wheel_hold = wheel_hold;
        // **Where the canvas may draw: the canvas panel's rectangle, and nothing else.**
        //
        // The whole surface is the fallback rather than a computed inset, because there is no
        // side panel to subtract any more -- and because a *wrong* rectangle here is worse than
        // a too-large one: it shoves the artwork sideways. The old code derived it from the
        // panel's width, which is why a centred prompt once moved the canvas.
        //
        // Which points the pen may reach is a different question, and the workspace is asked it
        // directly rather than answered from this.
        self.canvas_viewport =
            panel_canvas.unwrap_or((0.0, 0.0, size_px[0] as f32, size_px[1] as f32));

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
            export: export_choice,
            export_set,
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
