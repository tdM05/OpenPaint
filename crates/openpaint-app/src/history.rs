//! Undo / redo for strokes.
//!
//! # Why it lives on the GPU side
//!
//! The GPU is authoritative for pixels (DECISIONS §4a), so history has to work in
//! GPU resources. That is not ideal layering — history is conceptually a document
//! concern — but the alternative is reading pixels back to the CPU on every stroke,
//! which is exactly the stall we designed the paint path to avoid.
//!
//! # What is stored
//!
//! Per stroke, a **before-image of the region the stroke touched**, plus the dabs
//! and paint that produced it:
//!
//! - **Undo** copies the before-image back over that region. One GPU-to-GPU copy.
//! - **Redo** *replays the dabs* rather than restoring an after-image. That halves
//!   the memory (no second snapshot) and costs only a re-rasterization, which is
//!   the cheap direction now that dabs are stamped on the GPU.
//!
//! Snapshots are bounded to the stroke's rectangle rather than the whole canvas,
//! because most strokes touch a small fraction of it. A full-canvas stroke at 2048²
//! costs ~34 MiB, so there is also a byte budget (see [`BUDGET_BYTES`]) that evicts
//! the oldest entries — losing old undo levels is far better than exhausting a
//! Surface's shared graphics memory.
//!
//! # Not yet
//!
//! History is per-stroke only: navigation isn't undoable (correct — no art app
//! undoes panning), and there is nothing else undoable yet. When layers arrive this
//! grows an operation type; the snapshot-plus-replay shape should still hold.

use openpaint_core::Dab;

use crate::canvas_renderer::CANVAS_FORMAT;

/// How much snapshot memory history may hold before evicting its oldest entries.
///
/// 64 MiB is roughly two full-canvas strokes at 2048², or a great many typical
/// ones. Deliberately modest: the target shares graphics memory with the system
/// (DECISIONS §2), and an app that dies from undo history is worse than one that
/// forgets far-back edits.
pub const BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// A rectangle of canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasRect {
    pub x: u32,
    pub y: u32,
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

    /// The accumulated bounds clipped to a canvas of `w` × `h`, or `None` if the
    /// stroke touched nothing inside it.
    #[must_use]
    pub fn to_rect(self, canvas_w: u32, canvas_h: u32) -> Option<CanvasRect> {
        let (x0, y0, x1, y1) = self.min?;
        let x0 = x0.floor().max(0.0) as u32;
        let y0 = y0.floor().max(0.0) as u32;
        let x1 = (x1.ceil().max(0.0) as u32).min(canvas_w);
        let y1 = (y1.ceil().max(0.0) as u32).min(canvas_h);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(CanvasRect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }
}

/// Copy a canvas region into a fresh rect-sized texture — the before-image.
///
/// Texture-to-texture, so no row-alignment constraints apply and any rectangle
/// works (unlike readback to a buffer, which needs 256-byte rows).
pub fn snapshot_region(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    canvas: &wgpu::Texture,
    rect: CanvasRect,
) -> wgpu::Texture {
    let snapshot = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("history-snapshot"),
        size: wgpu::Extent3d {
            width: rect.w,
            height: rect.h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: CANVAS_FORMAT,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    copy_region(encoder, canvas, rect_origin(rect), &snapshot, zero(), rect);
    snapshot
}

/// Copy a before-image back over the canvas region it came from.
pub fn restore_region(
    encoder: &mut wgpu::CommandEncoder,
    snapshot: &wgpu::Texture,
    canvas: &wgpu::Texture,
    rect: CanvasRect,
) {
    copy_region(encoder, snapshot, zero(), canvas, rect_origin(rect), rect);
}

fn zero() -> wgpu::Origin3d {
    wgpu::Origin3d::ZERO
}

fn rect_origin(rect: CanvasRect) -> wgpu::Origin3d {
    wgpu::Origin3d {
        x: rect.x,
        y: rect.y,
        z: 0,
    }
}

/// The snapshot is always origin-based and rect-sized; the canvas side is offset by
/// the rect. Taking both origins explicitly rather than a direction flag makes it
/// hard to get them the wrong way round, which would be a silent corruption.
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

/// One undoable stroke.
pub struct Entry {
    pub rect: CanvasRect,
    /// The canvas region as it was *before* the stroke.
    pub before: wgpu::Texture,
    /// Everything needed to reproduce the stroke for redo.
    pub dabs: Vec<Dab>,
    pub color_linear_premul: [f32; 4],
    pub opacity: f32,
}

impl Entry {
    fn bytes(&self, bytes_per_texel: usize) -> usize {
        self.rect.bytes(bytes_per_texel)
    }
}

/// The undo/redo stacks and their memory budget.
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
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

    /// Record a completed stroke.
    ///
    /// Clears the redo stack, as any edit after undoing must: the redone future no
    /// longer follows from the present.
    pub fn push(&mut self, entry: Entry) {
        self.redo.clear();
        self.bytes += entry.bytes(self.bytes_per_texel);
        self.undo.push(entry);
        self.enforce_budget();
    }

    /// Take the most recent stroke off the undo stack, for the caller to restore.
    ///
    /// The caller must hand it back via [`History::finish_undo`] once restored, so
    /// it becomes redoable.
    pub fn pop_undo(&mut self) -> Option<Entry> {
        let entry = self.undo.pop()?;
        self.bytes -= entry.bytes(self.bytes_per_texel);
        Some(entry)
    }

    /// A restored entry becomes redoable. Its snapshot is still the *before* image,
    /// which is exactly what a later undo of the redone stroke needs.
    pub fn finish_undo(&mut self, entry: Entry) {
        self.bytes += entry.bytes(self.bytes_per_texel);
        self.redo.push(entry);
    }

    /// Take the most recent undone stroke, for the caller to replay.
    pub fn pop_redo(&mut self) -> Option<Entry> {
        let entry = self.redo.pop()?;
        self.bytes -= entry.bytes(self.bytes_per_texel);
        Some(entry)
    }

    /// A replayed entry becomes undoable again.
    pub fn finish_redo(&mut self, entry: Entry) {
        self.bytes += entry.bytes(self.bytes_per_texel);
        self.undo.push(entry);
        self.enforce_budget();
    }

    /// Drop the oldest undo levels until within budget.
    ///
    /// Oldest-first: recent history is what users reach for. A single stroke larger
    /// than the whole budget is kept anyway — refusing to record it would silently
    /// make that stroke unundoable, which is worse than briefly exceeding the cap.
    fn enforce_budget(&mut self) {
        while self.bytes > BUDGET_BYTES && self.undo.len() > 1 {
            let dropped = self.undo.remove(0);
            self.bytes -= dropped.bytes(self.bytes_per_texel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn bounds_cover_a_dab_and_its_radius() {
        let mut b = BoundsBuilder::default();
        b.add_dab(&dab(100.0, 100.0, 10.0));
        let r = b.to_rect(2048, 2048).expect("non-empty");
        // 10px radius plus a pixel of slack, so 89..111 inclusive.
        assert!(r.x <= 89 && r.y <= 89);
        assert!(r.x + r.w >= 111 && r.y + r.h >= 111);
    }

    #[test]
    fn bounds_grow_to_cover_every_dab() {
        let mut b = BoundsBuilder::default();
        b.add_dab(&dab(100.0, 100.0, 5.0));
        b.add_dab(&dab(500.0, 300.0, 5.0));
        let r = b.to_rect(2048, 2048).expect("non-empty");
        assert!(r.x <= 94);
        assert!(r.x + r.w >= 506);
        assert!(r.y + r.h >= 306);
    }

    /// A stroke that runs off the canvas must clip, or the snapshot copy would
    /// address pixels outside the texture and fail validation.
    #[test]
    fn bounds_clip_to_the_canvas() {
        let mut b = BoundsBuilder::default();
        b.add_dab(&dab(5.0, 5.0, 50.0));
        let r = b.to_rect(256, 256).expect("non-empty");
        assert_eq!(r.x, 0, "did not clip at the left edge");
        assert_eq!(r.y, 0);
        assert!(r.x + r.w <= 256);
        assert!(r.y + r.h <= 256);
    }

    #[test]
    fn bounds_entirely_off_canvas_are_empty() {
        let mut b = BoundsBuilder::default();
        b.add_dab(&dab(-500.0, -500.0, 5.0));
        assert!(b.to_rect(256, 256).is_none());
    }

    #[test]
    fn no_dabs_means_no_rect() {
        assert!(BoundsBuilder::default().to_rect(256, 256).is_none());
    }

    #[test]
    fn clearing_resets_accumulated_bounds() {
        let mut b = BoundsBuilder::default();
        b.add_dab(&dab(1000.0, 1000.0, 5.0));
        b.clear();
        assert!(b.to_rect(2048, 2048).is_none());
    }

    /// A snapshot/restore round trip must return the canvas exactly, and must put
    /// the pixels back **where they came from**. Getting the source and destination
    /// origins the wrong way round would corrupt a different region silently, which
    /// is why the helpers take both origins explicitly rather than a flag.
    #[test]
    fn snapshot_and_restore_round_trips_exactly() {
        use crate::test_gpu::{make_canvas, max_difference, readback, try_device, SIZE};

        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let canvas = make_canvas(&device, &queue);
        let pristine = readback(&device, &queue, &canvas);

        // An off-centre, non-square region, so a transposed or origin-swapped copy
        // cannot accidentally look correct.
        let rect = CanvasRect {
            x: 17,
            y: 40,
            w: 61,
            h: 33,
        };

        let mut encoder = device.create_command_encoder(&Default::default());
        let snapshot = snapshot_region(&device, &mut encoder, &canvas, rect);

        // Scribble over a larger area than the snapshot, then restore only the
        // snapshotted rect: the rest must stay scribbled.
        let scribble = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scribble"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: CANVAS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let scribble_view = scribble.create_view(&Default::default());
        encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scribble-fill"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &scribble_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::RED),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            })
            .forget_lifetime();
        let whole = CanvasRect {
            x: 0,
            y: 0,
            w: SIZE,
            h: SIZE,
        };
        copy_region(&mut encoder, &scribble, zero(), &canvas, zero(), whole);

        restore_region(&mut encoder, &snapshot, &canvas, rect);
        queue.submit(std::iter::once(encoder.finish()));

        let after = readback(&device, &queue, &canvas);

        // Inside the restored rect: identical to pristine.
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                let i = (y * SIZE + x) as usize;
                let (d, _) = max_difference(&[after[i]], &[pristine[i]]);
                assert!(d < 1e-6, "({x},{y}) not restored: {:?}", after[i]);
            }
        }

        // Just outside it: still scribbled, so the copy landed in the right place.
        let outside = ((rect.y + rect.h + 2) * SIZE + rect.x) as usize;
        assert!(
            after[outside][0] > 0.5 && after[outside][1] < 0.1,
            "region outside the rect was restored too: {:?}",
            after[outside]
        );
    }
}
