//! Undo / redo, tile by tile.
//!
//! # Why it lives on the GPU side
//!
//! The GPU is authoritative for pixels (DECISIONS §4a), so history has to work in GPU
//! resources. That is not ideal layering — history is conceptually a document concern —
//! but the alternative is reading pixels back to the CPU on every stroke, which is exactly
//! the stall the paint path is designed to avoid.
//!
//! # Strict LIFO is what makes this simple
//!
//! Operations are undone in exactly the reverse order they happened, which gives a
//! property worth stating outright: **the page geometry while undoing an operation is
//! always the geometry that operation was recorded against.** Any resize that came after
//! it must already have been undone.
//!
//! That is why nothing here rewrites stored coordinates. An earlier version did — it
//! shifted every rectangle and dab position whenever the page resized — but that was only
//! necessary because resizes were not themselves undoable. Making [`Op::Resize`] an
//! operation removed the need entirely, and deleted that machinery with it.
//!
//! Eviction does not break the invariant: it drops the *oldest* operations, so undo simply
//! cannot reach that far back. Everything still on the stack stays consistent.
//!
//! # A tile is the unit of history
//!
//! Snapshots used to be arbitrary rectangles in a page-sized texture. Now that the canvas
//! is a pool of tiles, a snapshot is a whole tile copied into a snapshot pool of the same
//! shape — GPU-to-GPU, so nothing comes back to the CPU on the interactive path.
//!
//! Uniform-size snapshots also make the memory budget exact rather than estimated: the
//! snapshot pool's capacity *is* the budget, so there is no byte counter to keep in step
//! with what the GPU actually holds.
//!
//! # What each operation stores
//!
//! - [`Op::Stroke`] keeps a **before-image of every tile it touched**, plus the dabs and
//!   paint that produced it. Undo copies the before-images back; redo *replays the dabs*
//!   rather than storing after-images, which halves the memory and costs only a
//!   re-rasterization — the cheap direction now that dabs are stamped on the GPU. A tile
//!   the stroke *created* is recorded as [`TileBefore::Absent`], so undo removes it
//!   instead of restoring paper over it.
//! - [`Op::Resize`] keeps **nothing but the two rectangles**. This is the shape of
//!   DECISIONS §5c: a resize destroys no pixels, so there is nothing to save. The previous
//!   version snapshotted the whole pre-crop canvas, and deleting that is the point.
//! - [`Op::Trim`] owns the tiles it discarded.
//! - [`Op::DeleteLayer`] owns the whole layer: its properties, its position in the stack, and
//!   every tile it held. Deleting a layer destroys pixels, so by the same argument as §5c it
//!   cannot be offered at all unless it is undoable.
//! - [`Op::DeletePage`] does the same for a page, which is a whole stack of layers at once and
//!   therefore the largest thing a single click can destroy.

use openpaint_core::tile::TileCoord;
use openpaint_core::{Dab, Layer, PageResize};

use crate::canvas_renderer::{CanvasRenderer, CANVAS_BYTES_PER_TEXEL, CANVAS_FORMAT};
use crate::tile_pool::{layers_for_budget, Slot, TilePool};
use crate::tile_store::{LayerId, TileKey};

/// How much snapshot memory history may hold before evicting its oldest operations.
///
/// Deliberately modest: the target shares graphics memory with the system (DECISIONS §2),
/// and an app that dies from undo history is worse than one that forgets far-back edits.
pub const BUDGET_BYTES: u64 = 64 * 1024 * 1024;

/// A tile as it was before an operation.
pub enum TileBefore {
    /// The tile existed, and this snapshot layer holds its contents.
    Content(Slot),
    /// The tile did not exist. Undo must **remove** it, not restore paper into it —
    /// otherwise a stroke's first touch of an area would leave a permanent paper tile
    /// behind, consuming residency for nothing.
    Absent,
}

/// Where an operation's coverage came from.
///
/// A stroke and a fill differ in exactly this and nothing else: both end up as coverage in the
/// accumulation buffer, baked through the same blend. Two `Op` variants would have shared five
/// fields out of six and drifted apart at the first change to any of them, so what varies is a
/// field rather than a variant.
pub enum PaintSource {
    /// Dabs stamped along a stroke.
    Dabs(Vec<Dab>),
    /// A selection mask, loaded straight into the accumulation buffer.
    ///
    /// Kept whole rather than re-derived. The gesture that produced it may be long gone — a flood
    /// fill or an inversion has no gesture at all — so a redo has nothing to recompute *from*, and
    /// the mask is the only honest record.
    Mask(Box<openpaint_core::Selection>),
}

/// One undoable operation.
pub enum Op {
    /// Paint put on a layer, however it got there.
    Paint {
        /// Which layer it was painted on. Undo has to put the pixels back where they came
        /// from, and the active layer may well have changed since.
        layer: LayerId,
        /// Every tile it wrote, as it was beforehand.
        before: Vec<(TileCoord, TileBefore)>,
        /// What produced the coverage, so redo can produce it again.
        source: PaintSource,
        color_linear_premul: [f32; 4],
        opacity: f32,
        /// How it was applied. Redo reproduces the coverage, so without this an erase would
        /// be redone as a black stroke.
        mode: crate::editor::PaintMode,
    },
    /// A selection's pixels picked up and put down somewhere else.
    ///
    /// Its own variant rather than a [`PaintSource`], because it is not paint: it *removes* pixels
    /// from one place as well as adding them to another, and a redo has to do both. `before` covers
    /// the source and the destination together, so the whole gesture is one entry in the stack --
    /// dragging a selection should cost one undo, not two.
    Move {
        layer: LayerId,
        before: Vec<(TileCoord, TileBefore)>,
        /// The pixels themselves, so redo can put them back down. Kept whole for the same reason
        /// `PaintSource::Mask` is: there is no gesture left to recompute them from.
        lifted: Box<openpaint_core::Lifted>,
        /// How they were placed. A whole-pixel move is a copy; anything else resamples, so the
        /// filter has to be recorded with it or a redo could reproduce the pixels differently from
        /// the ones that were undone.
        transform: openpaint_core::Transform,
        kernel: openpaint_core::Kernel,
        /// Where they came from, so redo can clear the source again — and so undo can put the
        /// *outline* back exactly, rather than trying to invert the transform. A non-uniform scale
        /// combined with a rotation has no inverse in this parameterisation, and keeping the
        /// original costs a mask that was already being kept.
        selection: Box<openpaint_core::Selection>,
    },
    /// Geometry only. Nothing is saved because nothing is destroyed (DECISIONS §5c).
    Resize { resize: PageResize },
    /// Tiles discarded by Trim, owned by the operation so undo can put them straight back.
    Trim { tiles: Vec<(TileKey, Slot)> },
    /// A deleted page, kept whole -- its layers and every tile they held.
    DeletePage {
        index: usize,
        page: openpaint_core::Page,
        tiles: Vec<(TileKey, Slot)>,
    },
    /// What a layer is *made of* changed: its text edited, or a text layer converted to raster.
    ///
    /// Holds the content on both sides and **no tiles at all**, which is the payoff of derived
    /// content: the pixels follow from the text, so undo restores the block and re-derives. A
    /// caption costs a string here rather than a snapshot of every tile it covers, and the result
    /// is exact rather than a re-rasterization that has to match.
    Content {
        layer: LayerId,
        /// Where the layer sits in the stack. The content lives in the document, which the shell
        /// owns, so undo reports the change rather than applying it — and the shell needs to know
        /// which layer, by position, exactly as [`Op::DeleteLayer`] does.
        index: usize,
        before: openpaint_core::Content,
        after: openpaint_core::Content,
        /// When this was recorded, so a run of keystrokes coalesces into one undo.
        ///
        /// Without it every character typed would be its own entry, and Ctrl+Z would walk back
        /// through a caption one letter at a time — which is not what anyone means by undo.
        at: std::time::Instant,
    },
    /// Two layers folded into one: the upper one's pixels baked into the lower, the upper removed.
    ///
    /// One entry rather than two, because it is one action — undoing a merge halfway, to a state
    /// where the pixels are combined but both layers still exist, would be a state that never
    /// existed.
    ///
    /// Redo re-runs the composite rather than storing the result. The undo has already put the
    /// upper layer's pixels back on the canvas, so everything the merge needs is there again, and
    /// an after-image would double what a merge costs the snapshot pool.
    MergeLayer {
        /// Where the merged-away layer sat, so undo restores the order and not just the pixels.
        index: usize,
        layer: Layer,
        tiles: Vec<(TileKey, Slot)>,
        /// The layer that received the paint, and what its tiles held beforehand.
        ///
        /// The whole layer rather than its id, because a redo has to run the composite again and
        /// the blend, opacity and clip that decide the result live on it. The renderer does not own
        /// the stack and so cannot look it up — see the module note on the split.
        lower: Layer,
        before: Vec<(TileCoord, TileBefore)>,
    },
    /// A deleted layer, kept whole so undo can put it back exactly where it was.
    DeleteLayer {
        /// Where it sat in the stack, so undo restores the order and not just the pixels.
        index: usize,
        layer: Layer,
        tiles: Vec<(TileKey, Slot)>,
    },
    /// A layer that was made: by Add, or by Duplicate, which is Add with pixels in it.
    ///
    /// **The mirror of [`Op::DeleteLayer`], and it had to exist for the same reason that one
    /// does.** Deleting a layer was undoable and making one was not, so Ctrl+Z after Add did
    /// nothing at all -- which reads as undo being broken rather than as this operation being
    /// outside it. Every change to the shape of the document belongs in one stack; a stack you
    /// have to know the contents of before you can predict what Ctrl+Z does is not one.
    ///
    /// `tiles` is empty when this is pushed and filled when it is *undone*: undoing an addition is
    /// a deletion, and a deletion has to take the pixels with it or a redo would put back an empty
    /// layer. A duplicate is the case that makes this matter.
    AddLayer {
        index: usize,
        layer: Layer,
        tiles: Vec<(TileKey, Slot)>,
    },
    /// A layer moved up or down the stack. Metadata only: nothing is drawn differently, the
    /// composite order changes, and there are no pixels to keep.
    MoveLayer { from: usize, to: usize },
    /// A page that was made. The mirror of [`Op::DeletePage`], for the same reason
    /// [`Op::AddLayer`] mirrors [`Op::DeleteLayer`]: a document with pages has two ways to change
    /// its shape and both belong in one stack.
    ///
    /// `page` and `tiles` are filled when it is undone -- a new page is made empty, but the
    /// undo has to keep whatever was drawn on it before Ctrl+Z was pressed, or a redo would hand
    /// back a blank one.
    AddPage {
        index: usize,
        page: Option<openpaint_core::Page>,
        tiles: Vec<(TileKey, Slot)>,
    },
    /// A page moved in the running order. Metadata only, like [`Op::MoveLayer`].
    MovePage { from: usize, to: usize },
}

impl Op {
    /// Snapshot layers this operation holds, for release on eviction.
    fn into_slots(self) -> Vec<Slot> {
        match self {
            // Move holds the same shape of before-image as Paint, and releasing it matters more:
            // a move snapshots its source and its destination.
            Self::Paint { before, .. } | Self::Move { before, .. } => before
                .into_iter()
                .filter_map(|(_, b)| match b {
                    TileBefore::Content(slot) => Some(slot),
                    TileBefore::Absent => None,
                })
                .collect(),
            // Neither holds a snapshot layer: a resize destroys nothing, and a content change
            // stores the content itself rather than the pixels it produced.
            Self::Resize { .. } | Self::Content { .. } => Vec::new(),
            Self::Trim { tiles }
            | Self::DeleteLayer { tiles, .. }
            | Self::AddLayer { tiles, .. }
            | Self::DeletePage { tiles, .. } => tiles.into_iter().map(|(_, slot)| slot).collect(),
            Self::AddPage { tiles, .. } => tiles.into_iter().map(|(_, slot)| slot).collect(),
            // Nothing but two indices.
            Self::MoveLayer { .. } | Self::MovePage { .. } => Vec::new(),
            // Both halves: a merge holds the layer it consumed *and* the before-image of the layer
            // it was folded into. Releasing one and forgetting the other leaks the pool until the
            // app restarts, which is exactly the shape of bug this function exists to prevent.
            Self::MergeLayer { tiles, before, .. } => tiles
                .into_iter()
                .map(|(_, slot)| slot)
                .chain(before.into_iter().filter_map(|(_, b)| match b {
                    TileBefore::Content(slot) => Some(slot),
                    TileBefore::Absent => None,
                }))
                .collect(),
        }
    }
}

/// The undo/redo stacks and the snapshot pool that bounds them.
/// How long a pause in typing ends one undo entry and begins the next.
///
/// Chosen the way editors choose it: long enough that a word typed at speed is one undo, short
/// enough that stopping to think is a boundary. Not per-keystroke, which would make Ctrl+Z walk
/// back through a caption one letter at a time; not per-focus-change either, which would make a
/// long caption a single all-or-nothing entry.
const TEXT_COALESCE_MS: u128 = 700;

pub struct History {
    pool: TilePool,
    undo: Vec<Op>,
    redo: Vec<Op>,
}

impl History {
    pub fn new(device: &wgpu::Device) -> Self {
        let capacity = layers_for_budget(device, CANVAS_BYTES_PER_TEXEL, BUDGET_BYTES);
        Self {
            pool: TilePool::new(
                device,
                "history-snapshots",
                CANVAS_FORMAT,
                capacity,
                CANVAS_BYTES_PER_TEXEL,
                wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            ),
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// Availability is `depth > 0`; there is deliberately no separate `can_undo`, since one
    /// accessor is harder to let drift than two.
    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    #[must_use]
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// Bytes of snapshot memory currently held.
    #[must_use]
    pub fn bytes_held(&self) -> u64 {
        self.pool.bytes_used()
    }

    /// The snapshot pool, for copies into and out of it.
    #[must_use]
    pub fn pool(&self) -> &TilePool {
        &self.pool
    }

    /// Copy the current contents of `tiles` out of the canvas, for a stroke about to
    /// overwrite them.
    ///
    /// Returns `None` if the snapshot pool cannot hold them all even after evicting
    /// everything — a stroke touching more than the whole budget. Partial snapshots are
    /// deliberately not returned: a half-recorded stroke would undo to a state that never
    /// existed, which is worse than not offering undo for it at all.
    pub fn snapshot_tiles(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        canvas: &CanvasRenderer,
        layer: LayerId,
        tiles: &[TileCoord],
    ) -> Option<Vec<(TileCoord, TileBefore)>> {
        let mut before = Vec::with_capacity(tiles.len());
        for coord in tiles {
            let Some(src) = canvas.slot(layer, *coord) else {
                // Nothing there yet, so the stroke is creating it.
                before.push((*coord, TileBefore::Absent));
                continue;
            };
            let Some(dst) = self.alloc_evicting() else {
                // Give back everything this attempt took, so a refusal costs no memory.
                for (_, b) in before {
                    if let TileBefore::Content(slot) = b {
                        self.pool.free(slot);
                    }
                }
                return None;
            };
            TilePool::copy_layer_from(encoder, canvas.pool(), src, &self.pool, &dst);
            before.push((*coord, TileBefore::Content(dst)));
        }
        Some(before)
    }

    /// Move a tile the canvas is giving up into the snapshot pool, for Trim.
    ///
    /// Returns `None` if there is no room, in which case the caller must not discard the
    /// tile — dropping it would destroy pixels with no way back, which is the one thing
    /// Trim is allowed to do only *undoably*.
    pub fn adopt_tile(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        canvas: &CanvasRenderer,
        key: TileKey,
    ) -> Option<Slot> {
        let src = canvas.slot(key.layer, key.coord)?;
        let dst = self.alloc_evicting()?;
        TilePool::copy_layer_from(encoder, canvas.pool(), src, &self.pool, &dst);
        Some(dst)
    }

    /// Hand a snapshot layer back, for a caller abandoning a partly-built operation.
    /// Give back every snapshot in a before-image that will not be recorded.
    ///
    /// A redo re-does the work but must not record it again, so the snapshots it takes along the
    /// way are dead the moment they are made. Without this a redo leaks a pool layer per tile.
    pub fn release_all(&mut self, before: Vec<(TileCoord, TileBefore)>) {
        for (_, was) in before {
            self.release_snapshot(was);
        }
    }

    /// Give back a snapshot that turned out not to be needed.
    ///
    /// A transform snapshots its source and its destination separately, and they overlap wherever
    /// the move was short. Whichever copy is discarded has to return its pool layer, or a drag
    /// would leak snapshot memory for every tile it touched twice.
    pub fn release_snapshot(&mut self, before: TileBefore) {
        if let TileBefore::Content(slot) = before {
            self.release_slot(slot);
        }
    }

    pub fn release_slot(&mut self, slot: Slot) {
        self.pool.free(slot);
    }

    /// Take a snapshot layer, evicting the oldest history if the pool is full.
    ///
    /// The pool's capacity *is* the budget, so this is where the budget is enforced —
    /// there is no separate byte accounting that could disagree with reality.
    fn alloc_evicting(&mut self) -> Option<Slot> {
        loop {
            if let Some(slot) = self.pool.alloc() {
                return Some(slot);
            }
            // Redo first: a redoable future is worth less than an undoable past, and the
            // user has already moved away from it.
            let victim = if self.redo.is_empty() {
                if self.undo.is_empty() {
                    return None;
                }
                // Oldest-first: recent history is what people reach for.
                self.undo.remove(0)
            } else {
                self.redo.remove(0)
            };
            self.release(victim);
        }
    }

    /// Return an operation's snapshot layers to the pool.
    fn release(&mut self, op: Op) {
        for slot in op.into_slots() {
            self.pool.free(slot);
        }
    }

    /// Record a completed operation.
    ///
    /// Clears the redo stack, as any edit after undoing must: the redone future no longer
    /// follows from the present.
    ///
    /// Consecutive edits to the same layer's content within [`TEXT_COALESCE_MS`] are folded into
    /// the one entry, so typing a caption is a single undo rather than one per keystroke. Folding
    /// keeps the *earlier* `before` and the *later* `after`, which is what makes the merged entry
    /// describe the whole run.
    pub fn push(&mut self, op: Op) {
        let stale = std::mem::take(&mut self.redo);
        for op in stale {
            self.release(op);
        }

        if let Op::Content {
            layer,
            after,
            at,
            index,
            ..
        } = &op
        {
            if let Some(Op::Content {
                layer: prev_layer,
                after: prev_after,
                at: prev_at,
                index: prev_index,
                ..
            }) = self.undo.last_mut()
            {
                let same_layer = prev_layer == layer && prev_index == index;
                let still_typing = at.duration_since(*prev_at).as_millis() < TEXT_COALESCE_MS;
                if same_layer && still_typing {
                    prev_after.clone_from(after);
                    *prev_at = *at;
                    return;
                }
            }
        }

        self.undo.push(op);
    }

    /// Take the most recent operation, for the caller to revert.
    ///
    /// It must be handed back via [`History::finish_undo`] once reverted, so it becomes
    /// redoable — and so its snapshot layers are not leaked.
    pub fn pop_undo(&mut self) -> Option<Op> {
        self.undo.pop()
    }

    /// A reverted operation becomes redoable, keeping whatever it stored — that data is
    /// exactly what a later undo of the redone operation needs again.
    /// The op the next redo will take, so a caller can finish filling it in.
    ///
    /// Only ever used by an undo that had to be completed by somebody else -- see
    /// `Renderer::keep_page`, where the page belongs to the document and the tiles belong to the
    /// renderer, and neither can reach the other's half.
    pub fn newest_redo_mut(&mut self) -> Option<&mut Op> {
        self.redo.last_mut()
    }

    pub fn finish_undo(&mut self, op: Op) {
        self.redo.push(op);
    }

    /// Take the most recent undone operation, for the caller to re-apply.
    pub fn pop_redo(&mut self) -> Option<Op> {
        self.redo.pop()
    }

    /// A re-applied operation becomes undoable again.
    pub fn finish_redo(&mut self, op: Op) {
        self.undo.push(op);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_gpu::try_device;

    fn history(device: &wgpu::Device) -> History {
        History::new(device)
    }

    const L0: LayerId = LayerId(0);

    fn stroke_op(before: Vec<(TileCoord, TileBefore)>) -> Op {
        Op::Paint {
            layer: L0,
            before,
            source: PaintSource::Dabs(Vec::new()),
            color_linear_premul: [0.0; 4],
            opacity: 1.0,
            mode: crate::editor::PaintMode::Normal,
        }
    }

    fn resize() -> Op {
        Op::Resize {
            resize: PageResize {
                old: openpaint_core::PageRect::from_size(100, 100),
                new: openpaint_core::PageRect::from_size(100, 200),
            },
        }
    }

    fn text(words: &str) -> openpaint_core::Content {
        openpaint_core::Content::Text(Box::new(openpaint_core::TextBlock {
            text: words.into(),
            ..openpaint_core::TextBlock::default()
        }))
    }

    fn edit(at: std::time::Instant, before: &str, after: &str) -> Op {
        Op::Content {
            layer: L0,
            index: 0,
            before: text(before),
            after: text(after),
            at,
        }
    }

    /// Typing is one undo, not one per keystroke. Without coalescing, Ctrl+Z would walk back
    /// through a caption a letter at a time, which is not what anyone means by undo.
    #[test]
    fn a_run_of_keystrokes_is_one_undo() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut h = history(&device);
        let start = std::time::Instant::now();
        h.push(edit(start, "", "H"));
        h.push(edit(
            start + std::time::Duration::from_millis(80),
            "H",
            "He",
        ));
        h.push(edit(
            start + std::time::Duration::from_millis(160),
            "He",
            "Hel",
        ));

        assert_eq!(h.undo_depth(), 1, "three keystrokes should be one entry");
        let Some(Op::Content { before, after, .. }) = h.pop_undo() else {
            panic!("the entry should be a content change");
        };
        assert_eq!(
            before,
            text(""),
            "the merged entry keeps the earliest before"
        );
        assert_eq!(after, text("Hel"), "and the latest after");
    }

    /// A pause is a boundary, or a whole caption would be one all-or-nothing entry.
    #[test]
    fn a_pause_starts_a_new_undo_entry() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut h = history(&device);
        let start = std::time::Instant::now();
        h.push(edit(start, "", "Hi"));
        h.push(edit(
            start + std::time::Duration::from_millis(TEXT_COALESCE_MS as u64 + 50),
            "Hi",
            "Hi there",
        ));
        assert_eq!(h.undo_depth(), 2, "a pause should break the run");
    }

    /// Edits to different layers must never merge, however fast they arrive: the merged entry
    /// would restore one layer's words onto another.
    #[test]
    fn edits_to_different_layers_do_not_merge() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut h = history(&device);
        let start = std::time::Instant::now();
        h.push(edit(start, "", "a"));
        h.push(Op::Content {
            layer: LayerId(1),
            index: 1,
            before: text(""),
            after: text("b"),
            at: start + std::time::Duration::from_millis(10),
        });
        assert_eq!(h.undo_depth(), 2);
    }

    /// A content change holds no snapshot layers, which is what makes an edit cost a string
    /// rather than a picture of every tile the caption covers.
    #[test]
    fn a_content_change_holds_no_tiles() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut h = history(&device);
        let before = h.bytes_held();
        h.push(edit(
            std::time::Instant::now(),
            "",
            "a whole paragraph of words",
        ));
        assert_eq!(
            h.bytes_held(),
            before,
            "a text edit should not hold snapshot memory"
        );
    }

    /// Anything recorded after an undo invalidates the redo stack, content changes included.
    #[test]
    fn a_content_change_clears_the_redo_stack() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut h = history(&device);
        let start = std::time::Instant::now();
        h.push(edit(start, "", "one"));
        let op = h.pop_undo().expect("an entry");
        h.finish_undo(op);
        assert_eq!(h.redo_depth(), 1);

        h.push(edit(
            start + std::time::Duration::from_millis(2000),
            "",
            "another",
        ));
        assert_eq!(h.redo_depth(), 0, "the redone future no longer follows");
    }

    #[test]
    fn depths_track_the_stacks() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut h = history(&device);
        assert_eq!((h.undo_depth(), h.redo_depth()), (0, 0));
        h.push(resize());
        assert_eq!((h.undo_depth(), h.redo_depth()), (1, 0));

        let op = h.pop_undo().expect("one op");
        h.finish_undo(op);
        assert_eq!((h.undo_depth(), h.redo_depth()), (0, 1));

        let op = h.pop_redo().expect("one redo");
        h.finish_redo(op);
        assert_eq!((h.undo_depth(), h.redo_depth()), (1, 0));
    }

    #[test]
    fn a_new_operation_clears_redo() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut h = history(&device);
        h.push(resize());
        let op = h.pop_undo().expect("op");
        h.finish_undo(op);
        assert_eq!(h.redo_depth(), 1);
        h.push(resize());
        assert_eq!(h.redo_depth(), 0, "redo should not survive a new edit");
    }

    /// A resize holds no pixels at all. That is DECISIONS 5c in one assertion: if a crop
    /// ever needs a snapshot again, something has gone back to destroying pixels.
    #[test]
    fn a_resize_costs_no_snapshot_memory() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut h = history(&device);
        for _ in 0..1000 {
            h.push(resize());
        }
        assert_eq!(h.bytes_held(), 0, "a resize should hold no snapshots");
        assert_eq!(h.undo_depth(), 1000, "nothing should have been evicted");
    }

    /// Eviction has to actually return layers, or the pool would fill permanently and no
    /// further stroke could be recorded.
    #[test]
    fn eviction_reclaims_snapshot_layers() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut h = history(&device);
        let capacity = h.pool.capacity();

        // Fill the pool one single-tile op at a time.
        for _ in 0..capacity {
            let slot = h.alloc_evicting().expect("within capacity");
            h.push(stroke_op(vec![((0, 0), TileBefore::Content(slot))]));
        }
        assert_eq!(h.undo_depth(), capacity as usize);
        assert_eq!(h.pool.used(), capacity);

        // One more must succeed by evicting the oldest.
        let slot = h.alloc_evicting().expect("eviction should have made room");
        h.pool.free(slot);
        assert!(
            h.undo_depth() < capacity as usize,
            "nothing was evicted: depth {}",
            h.undo_depth()
        );
    }

    /// Redo is dropped before undo, because the user has already moved away from it while
    /// the undo stack is what they are about to reach for.
    #[test]
    fn eviction_sacrifices_redo_before_undo() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut h = history(&device);
        let capacity = h.pool.capacity();

        let one = |h: &mut History| {
            let slot = h.alloc_evicting().expect("capacity");
            stroke_op(vec![((0, 0), TileBefore::Content(slot))])
        };

        // Half the capacity on the undo stack, half moved over to redo.
        for _ in 0..capacity / 2 {
            let op = one(&mut h);
            h.push(op);
        }
        for _ in 0..capacity / 2 {
            let op = one(&mut h);
            h.undo.push(op);
        }
        for _ in 0..capacity / 2 {
            let op = h.pop_undo().expect("op");
            h.finish_undo(op);
        }
        let undo_before = h.undo_depth();
        assert!(h.redo_depth() > 0 && undo_before > 0);
        assert_eq!(h.pool.used(), h.pool.capacity());

        let slot = h.alloc_evicting().expect("room");
        h.pool.free(slot);
        assert_eq!(h.undo_depth(), undo_before, "undo was sacrificed first");
    }
}
