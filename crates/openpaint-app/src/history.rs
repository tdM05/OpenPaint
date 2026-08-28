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

/// One undoable operation.
pub enum Op {
    Stroke {
        /// Which layer it was painted on. Undo has to put the pixels back where they came
        /// from, and the active layer may well have changed since.
        layer: LayerId,
        /// Every tile the stroke wrote, as it was beforehand.
        before: Vec<(TileCoord, TileBefore)>,
        /// Everything needed to reproduce the stroke for redo.
        dabs: Vec<Dab>,
        color_linear_premul: [f32; 4],
        opacity: f32,
    },
    /// Geometry only. Nothing is saved because nothing is destroyed (DECISIONS §5c).
    Resize { resize: PageResize },
    /// Tiles discarded by Trim, owned by the operation so undo can put them straight back.
    Trim { tiles: Vec<(TileKey, Slot)> },
    /// A deleted layer, kept whole so undo can put it back exactly where it was.
    DeleteLayer {
        /// Where it sat in the stack, so undo restores the order and not just the pixels.
        index: usize,
        layer: Layer,
        tiles: Vec<(TileKey, Slot)>,
    },
}

impl Op {
    /// Snapshot layers this operation holds, for release on eviction.
    fn into_slots(self) -> Vec<Slot> {
        match self {
            Self::Stroke { before, .. } => before
                .into_iter()
                .filter_map(|(_, b)| match b {
                    TileBefore::Content(slot) => Some(slot),
                    TileBefore::Absent => None,
                })
                .collect(),
            Self::Resize { .. } => Vec::new(),
            Self::Trim { tiles } | Self::DeleteLayer { tiles, .. } => {
                tiles.into_iter().map(|(_, slot)| slot).collect()
            }
        }
    }
}

/// The undo/redo stacks and the snapshot pool that bounds them.
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
    pub fn push(&mut self, op: Op) {
        let stale = std::mem::take(&mut self.redo);
        for op in stale {
            self.release(op);
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
        Op::Stroke {
            layer: L0,
            before,
            dabs: Vec::new(),
            color_linear_premul: [0.0; 4],
            opacity: 1.0,
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
