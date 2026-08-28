//! Undo / redo.
//!
//! # Why it lives on the GPU side
//!
//! The GPU is authoritative for pixels (DECISIONS §4a), so history has to work in
//! GPU resources. That is not ideal layering — history is conceptually a document
//! concern — but the alternative is reading pixels back to the CPU on every stroke,
//! which is exactly the stall the paint path is designed to avoid.
//!
//! # Strict LIFO is what makes this simple
//!
//! Operations are undone in exactly the reverse order they happened, which gives a
//! property worth stating outright: **the page geometry while undoing an operation is
//! always the geometry that operation was recorded against.** Any resize that came
//! after it must already have been undone.
//!
//! That is why nothing here rewrites stored coordinates. An earlier version did — it
//! shifted every rectangle and dab position whenever the page resized — but that was
//! only necessary because resizes were not themselves undoable. Making [`Op::Resize`]
//! an operation removed the need entirely, and deleted that machinery with it.
//!
//! Eviction does not break the invariant: it drops the *oldest* operations, so undo
//! simply cannot reach that far back. Everything still on the stack stays consistent.
//!
//! # What each operation stores
//!
//! - [`Op::Stroke`] keeps a **before-image of the region the stroke touched**, plus
//!   the dabs and paint that produced it. Undo copies the before-image back; redo
//!   *replays the dabs* rather than storing an after-image, which halves the memory
//!   and costs only a re-rasterization — the cheap direction now that dabs are
//!   stamped on the GPU.
//! - [`Op::Resize`] keeps the old and new dimensions and the anchor. **Growing needs
//!   no pixels saved**, because shrinking back is lossless — which is what makes
//!   undoable extends nearly free. **Shrinking does**, so a crop keeps a copy of the
//!   whole pre-crop canvas; that is the only way to give back what it removed.

use openpaint_core::{Dab, PageResize};

use crate::canvas_renderer::CANVAS_FORMAT;

/// How much snapshot memory history may hold before evicting its oldest operations.
///
/// Deliberately modest: the target shares graphics memory with the system (DECISIONS
/// §2), and an app that dies from undo history is worse than one that forgets
/// far-back edits.
pub const BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// A rectangle in **page coordinates**, whose corner may be negative once the page
/// has been extended upward or leftward (see `openpaint_core::canvas`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl CanvasRect {
    #[must_use]
    pub fn bytes(&self, bytes_per_texel: usize) -> usize {
        self.w as usize * self.h as usize * bytes_per_texel
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// This rectangle's corner as a texture origin.
    ///
    /// A texture is always zero-based, so page coordinates are converted by subtracting
    /// the page origin. This is *the* place that conversion happens; keeping it to one
    /// method is what stops page and texture coordinates being mixed up silently.
    #[must_use]
    pub fn texture_origin(&self, page_origin: (i32, i32)) -> wgpu::Origin3d {
        wgpu::Origin3d {
            x: (self.x - page_origin.0).max(0) as u32,
            y: (self.y - page_origin.1).max(0) as u32,
            z: 0,
        }
    }
}

/// Accumulates the area a stroke touches, so only that region is snapshotted.
#[derive(Clone, Copy, Debug, Default)]
pub struct BoundsBuilder {
    min: Option<(f32, f32, f32, f32)>,
}

impl BoundsBuilder {
    pub fn clear(&mut self) {
        self.min = None;
    }

    /// Include a dab, expanded by its radius plus a pixel of antialiasing slack.
    pub fn add_dab(&mut self, d: &Dab) {
        let r = d.radius + 1.0;
        let (x0, y0, x1, y1) = (d.x - r, d.y - r, d.x + r, d.y + r);
        self.min = Some(match self.min {
            None => (x0, y0, x1, y1),
            Some((ax0, ay0, ax1, ay1)) => (ax0.min(x0), ay0.min(y0), ax1.max(x1), ay1.max(y1)),
        });
    }

    /// The accumulated bounds clipped to the page, or `None` if the stroke touched
    /// nothing inside it.
    ///
    /// Clipped against the page's actual rectangle rather than `0..w`, because the
    /// origin can be negative.
    #[must_use]
    pub fn to_rect(self, canvas: &openpaint_core::Canvas) -> Option<CanvasRect> {
        let (x0f, y0f, x1f, y1f) = self.min?;
        let (ox, oy) = canvas.origin();
        let (ex, ey) = canvas.end();

        let x0 = (x0f.floor() as i32).max(ox);
        let y0 = (y0f.floor() as i32).max(oy);
        let x1 = (x1f.ceil() as i32).min(ex);
        let y1 = (y1f.ceil() as i32).min(ey);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(CanvasRect {
            x: x0,
            y: y0,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        })
    }
}

/// One undoable operation.
pub enum Op {
    Stroke {
        rect: CanvasRect,
        /// The canvas region as it was *before* the stroke.
        before: wgpu::Texture,
        /// Everything needed to reproduce the stroke for redo.
        dabs: Vec<Dab>,
        color_linear_premul: [f32; 4],
        opacity: f32,
    },
    Resize {
        resize: PageResize,
        /// The whole pre-resize canvas, kept only when the resize **lost** pixels.
        before: Option<wgpu::Texture>,
    },
}

impl Op {
    fn bytes(&self, bytes_per_texel: usize) -> usize {
        match self {
            Self::Stroke { rect, .. } => rect.bytes(bytes_per_texel),
            Self::Resize { resize, before } => {
                if before.is_some() {
                    resize.old_w as usize * resize.old_h as usize * bytes_per_texel
                } else {
                    0
                }
            }
        }
    }
}

/// The undo/redo stacks and their memory budget.
pub struct History {
    undo: Vec<Op>,
    redo: Vec<Op>,
    bytes: usize,
    bytes_per_texel: usize,
}

impl History {
    #[must_use]
    pub fn new(bytes_per_texel: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            bytes: 0,
            bytes_per_texel,
        }
    }

    /// Availability is `depth > 0`; there is deliberately no separate `can_undo`,
    /// since one accessor is harder to let drift than two.
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
    pub fn bytes_held(&self) -> usize {
        self.bytes
    }

    /// Record a completed operation.
    ///
    /// Clears the redo stack, as any edit after undoing must: the redone future no
    /// longer follows from the present.
    pub fn push(&mut self, op: Op) {
        self.redo.clear();
        self.bytes += op.bytes(self.bytes_per_texel);
        self.undo.push(op);
        self.enforce_budget();
    }

    /// Take the most recent operation, for the caller to revert.
    ///
    /// It must be handed back via [`History::finish_undo`] once reverted, so it
    /// becomes redoable.
    pub fn pop_undo(&mut self) -> Option<Op> {
        let op = self.undo.pop()?;
        self.bytes -= op.bytes(self.bytes_per_texel);
        Some(op)
    }

    /// A reverted operation becomes redoable, keeping whatever it stored — that data
    /// is exactly what a later undo of the redone operation needs again.
    pub fn finish_undo(&mut self, op: Op) {
        self.bytes += op.bytes(self.bytes_per_texel);
        self.redo.push(op);
    }

    /// Take the most recent undone operation, for the caller to re-apply.
    pub fn pop_redo(&mut self) -> Option<Op> {
        let op = self.redo.pop()?;
        self.bytes -= op.bytes(self.bytes_per_texel);
        Some(op)
    }

    /// A re-applied operation becomes undoable again.
    pub fn finish_redo(&mut self, op: Op) {
        self.bytes += op.bytes(self.bytes_per_texel);
        self.undo.push(op);
        self.enforce_budget();
    }

    /// Drop the oldest operations until within budget.
    ///
    /// Oldest-first: recent history is what users reach for. A single operation larger
    /// than the whole budget is kept anyway — refusing to record it would silently
    /// make that edit unundoable, which is worse than briefly exceeding the cap.
    fn enforce_budget(&mut self) {
        while self.bytes > BUDGET_BYTES && self.undo.len() > 1 {
            let dropped = self.undo.remove(0);
            self.bytes -= dropped.bytes(self.bytes_per_texel);
        }
    }
}

/// Copy a canvas region into a fresh rect-sized texture — the before-image.
///
/// Texture-to-texture, so no row-alignment constraints apply and any rectangle works
/// (unlike readback to a buffer, which needs 256-byte rows).
pub fn snapshot_region(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    canvas: &wgpu::Texture,
    page_origin: (i32, i32),
    rect: CanvasRect,
) -> wgpu::Texture {
    let snapshot = new_snapshot(device, rect.w, rect.h);
    copy_region(
        encoder,
        canvas,
        rect.texture_origin(page_origin),
        &snapshot,
        zero(),
        rect,
    );
    snapshot
}

/// Copy a before-image back over the canvas region it came from.
pub fn restore_region(
    encoder: &mut wgpu::CommandEncoder,
    snapshot: &wgpu::Texture,
    canvas: &wgpu::Texture,
    page_origin: (i32, i32),
    rect: CanvasRect,
) {
    copy_region(
        encoder,
        snapshot,
        zero(),
        canvas,
        rect.texture_origin(page_origin),
        rect,
    );
}

/// A snapshot texture of the given size.
pub fn new_snapshot(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("history-snapshot"),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: CANVAS_FORMAT,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn zero() -> wgpu::Origin3d {
    wgpu::Origin3d::ZERO
}

/// The snapshot is always origin-based and rect-sized; the canvas side is offset by
/// the rect. Taking both origins explicitly rather than a direction flag makes it hard
/// to get them the wrong way round, which would be a silent corruption.
fn copy_region(
    encoder: &mut wgpu::CommandEncoder,
    src: &wgpu::Texture,
    src_origin: wgpu::Origin3d,
    dst: &wgpu::Texture,
    dst_origin: wgpu::Origin3d,
    rect: CanvasRect,
) {
    encoder.copy_texture_to_texture(
        wgpu::ImageCopyTexture {
            texture: src,
            mip_level: 0,
            origin: src_origin,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyTexture {
            texture: dst,
            mip_level: 0,
            origin: dst_origin,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d {
            width: rect.w,
            height: rect.h,
            depth_or_array_layers: 1,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaint_core::{Anchor, Canvas};

    /// One past a rect's bottom-right corner. A test helper rather than API, so
    /// callers don't mix i32 and u32 at a comparison -- which is where sign errors
    /// creep in -- without adding a method nothing in production needs.
    fn rect_end(r: &CanvasRect) -> (i32, i32) {
        (r.x + r.w as i32, r.y + r.h as i32)
    }

    fn dab(x: f32, y: f32, radius: f32) -> Dab {
        Dab {
            x,
            y,
            radius,
            hardness: 0.5,
            flow: 1.0,
            color_linear_premul: [0.0, 0.0, 0.0, 1.0],
        }
    }

    fn grow() -> Op {
        Op::Resize {
            resize: PageResize {
                old_w: 100,
                old_h: 100,
                new_w: 100,
                new_h: 200,
                anchor: Anchor::TOP_LEFT,
            },
            before: None,
        }
    }

    #[test]
    fn bounds_cover_a_dab_and_its_radius() {
        let mut b = BoundsBuilder::default();
        b.add_dab(&dab(100.0, 100.0, 10.0));
        let r = b
            .to_rect(&openpaint_core::Canvas::new(2048, 2048))
            .expect("non-empty");
        assert!(r.x <= 89 && r.y <= 89);
        assert_eq!(
            rect_end(&r).0.min(111),
            111,
            "right edge short: {:?}",
            rect_end(&r)
        );
        assert_eq!(
            rect_end(&r).1.min(111),
            111,
            "bottom edge short: {:?}",
            rect_end(&r)
        );
    }

    #[test]
    fn bounds_grow_to_cover_every_dab() {
        let mut b = BoundsBuilder::default();
        b.add_dab(&dab(100.0, 100.0, 5.0));
        b.add_dab(&dab(500.0, 300.0, 5.0));
        let r = b
            .to_rect(&openpaint_core::Canvas::new(2048, 2048))
            .expect("non-empty");
        assert!(r.x <= 94);
        assert!(rect_end(&r).0 >= 506);
        assert!(rect_end(&r).1 >= 306);
    }

    /// A stroke running off the canvas must clip, or the snapshot copy would address
    /// pixels outside the texture and fail validation.
    #[test]
    fn bounds_clip_to_the_canvas() {
        let mut b = BoundsBuilder::default();
        b.add_dab(&dab(5.0, 5.0, 50.0));
        let r = b
            .to_rect(&openpaint_core::Canvas::new(256, 256))
            .expect("non-empty");
        assert_eq!(r.x, 0, "did not clip at the left edge");
        assert_eq!(r.y, 0);
        assert!(rect_end(&r).0 <= 256);
        assert!(rect_end(&r).1 <= 256);
    }

    #[test]
    fn bounds_entirely_off_canvas_are_empty() {
        let mut b = BoundsBuilder::default();
        b.add_dab(&dab(-500.0, -500.0, 5.0));
        assert!(b.to_rect(&Canvas::new(256, 256)).is_none());
    }

    #[test]
    fn no_dabs_means_no_rect() {
        assert!(BoundsBuilder::default()
            .to_rect(&Canvas::new(256, 256))
            .is_none());
    }

    #[test]
    fn clearing_resets_accumulated_bounds() {
        let mut b = BoundsBuilder::default();
        b.add_dab(&dab(1000.0, 1000.0, 5.0));
        b.clear();
        assert!(b
            .to_rect(&openpaint_core::Canvas::new(2048, 2048))
            .is_none());
    }

    /// A grow costs no snapshot memory. That is what makes undoable extends nearly
    /// free, and why they can be recorded unconditionally.
    #[test]
    fn a_grow_costs_no_snapshot_memory() {
        let mut h = History::new(8);
        h.push(grow());
        assert_eq!(h.undo_depth(), 1);
        assert_eq!(h.bytes_held(), 0, "a grow should not need pixels saved");
    }

    /// A crop is charged for the whole pre-crop canvas, because giving back what it
    /// removed is the only way to undo it.
    #[test]
    fn a_crop_is_charged_for_the_old_canvas() {
        let Some((device, _q)) = crate::test_gpu::try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut h = History::new(8);
        h.push(Op::Resize {
            resize: PageResize {
                old_w: 100,
                old_h: 50,
                new_w: 40,
                new_h: 20,
                anchor: Anchor::TOP_LEFT,
            },
            before: Some(new_snapshot(&device, 100, 50)),
        });
        assert_eq!(h.bytes_held(), 100 * 50 * 8);
    }

    #[test]
    fn pushing_after_an_undo_clears_the_redo_stack() {
        let mut h = History::new(8);
        h.push(grow());
        let op = h.pop_undo().expect("op");
        h.finish_undo(op);
        assert_eq!(h.redo_depth(), 1);

        h.push(grow());
        assert_eq!(h.redo_depth(), 0, "a new edit must invalidate redo");
    }

    /// Round-tripping through both stacks must leave the accounting where it started.
    #[test]
    fn byte_accounting_survives_undo_and_redo() {
        let mut h = History::new(8);
        h.push(grow());
        let before = h.bytes_held();

        let op = h.pop_undo().expect("op");
        h.finish_undo(op);
        let op = h.pop_redo().expect("op");
        h.finish_redo(op);

        assert_eq!(h.bytes_held(), before);
        assert_eq!(h.undo_depth(), 1);
        assert_eq!(h.redo_depth(), 0);
    }

    /// Undo must not be able to pop past the bottom of the stack.
    #[test]
    fn popping_an_empty_history_yields_nothing() {
        let mut h = History::new(8);
        assert!(h.pop_undo().is_none());
        assert!(h.pop_redo().is_none());
    }
}
