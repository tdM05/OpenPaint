//! A bounded pool of GPU tiles, backed by one 2D **array** texture.
//!
//! This is the allocator the tiled canvas is built on (OPEN_QUESTIONS Q13). It replaces
//! the one-texture-per-page shortcut, and with it the two ceilings that shortcut imposed:
//! a page could be no larger than `max_texture_dimension_2d`, and no larger than a single
//! allocation the driver would accept.
//!
//! # Why an array texture rather than a texture per tile, or a 2D atlas
//!
//! A texture per tile needs a bind group per tile and therefore a draw call per tile —
//! hundreds of draws and hundreds of descriptor switches for one frame of canvas.
//!
//! A 2D atlas (a grid of tiles inside one large texture) needs only one bind group, but
//! adjacent tiles are physical neighbours in the atlas, so any filtered sample near a
//! tile edge bleeds in whatever tile happens to sit beside it. Avoiding that means apron
//! pixels around every tile, which costs memory and has to be kept in sync on every
//! write.
//!
//! An array texture gets the single bind group without the bleed: each layer is its own
//! image, clamped at its own edges. One instanced draw covers every visible tile, with
//! the layer index arriving as per-instance data.
//!
//! # Bounded by construction
//!
//! Capacity is fixed at creation from a byte budget (and clamped by the device's array
//! layer limit), which is what DECISIONS §2 requires: GPU residency must be bounded
//! independently of canvas size, because a Surface shares its graphics memory with the
//! system and a webtoon strip has no natural size limit.
//!
//! # This is a slab, not a cache
//!
//! Deliberately: it hands out and takes back layers and knows nothing about tile
//! coordinates, paper colour, or eviction order. Which tile lives in which layer is
//! [`TileMap`]'s job, and the two are separate because a canvas, a stroke's accumulation
//! buffer, and undo's snapshots all want the same slab with different keys and different
//! lifetimes.

use std::collections::HashMap;

use openpaint_core::tile::{TileCoord, TILE_SIZE};

/// A layer of the pool, owned by whoever allocated it.
///
/// Deliberately **not** `Copy`. A layer index that can be duplicated can be freed twice,
/// which hands the same GPU memory to two owners and produces corruption that looks like
/// a rendering bug. Making the handle move-only pushes that class of mistake into a
/// compile error: to free a slot you have to give it up, and to give it up you have to
/// have it.
#[derive(Debug, PartialEq, Eq)]
pub struct Slot(u32);

impl Slot {
    /// The array layer this slot refers to, for shader-visible instance data.
    #[must_use]
    pub fn layer(&self) -> u32 {
        self.0
    }
}

/// Bytes one tile occupies at `bytes_per_texel`.
#[must_use]
pub fn tile_bytes(bytes_per_texel: u32) -> u64 {
    (TILE_SIZE as u64) * (TILE_SIZE as u64) * u64::from(bytes_per_texel)
}

/// How many layers fit in `budget_bytes`, clamped to what the device allows.
///
/// Returns at least 1: a pool that cannot hold a single tile is useless, and failing
/// loudly at allocation time is better than silently having nowhere to paint.
#[must_use]
pub fn layers_for_budget(device: &wgpu::Device, bytes_per_texel: u32, budget_bytes: u64) -> u32 {
    let per_tile = tile_bytes(bytes_per_texel);
    let by_budget = (budget_bytes / per_tile).max(1);
    let by_device = u64::from(device.limits().max_texture_array_layers);
    u32::try_from(by_budget.min(by_device)).unwrap_or(u32::MAX)
}

pub struct TilePool {
    texture: wgpu::Texture,
    /// The whole array, for sampling in a shader.
    array_view: wgpu::TextureView,
    /// One view per layer, for use as a render target. Built once, because a render
    /// pass needs a single-layer view and creating one per pass per tile would be
    /// churn on the hot path.
    layer_views: Vec<wgpu::TextureView>,
    free: Vec<u32>,
    capacity: u32,
    bytes_per_texel: u32,
}

impl TilePool {
    /// Create a pool of `capacity` tiles in `format`.
    ///
    /// `bytes_per_texel` is passed rather than derived from the format because wgpu does
    /// not expose a total-ordering of block sizes we can rely on here, and the accounting
    /// has to match what the budget was computed from.
    pub fn new(
        device: &wgpu::Device,
        label: &str,
        format: wgpu::TextureFormat,
        capacity: u32,
        bytes_per_texel: u32,
        usage: wgpu::TextureUsages,
    ) -> Self {
        let capacity = capacity.max(1);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: TILE_SIZE as u32,
                height: TILE_SIZE as u32,
                depth_or_array_layers: capacity,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });

        let array_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some(label),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        let layer_views = (0..capacity)
            .map(|layer| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some(label),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();

        Self {
            texture,
            array_view,
            layer_views,
            // Reversed so the first allocation is layer 0, which makes GPU captures and
            // test expectations readable.
            free: (0..capacity).rev().collect(),
            capacity,
            bytes_per_texel,
        }
    }

    /// Take a free layer, or `None` if the pool is full.
    ///
    /// Full is a real, reachable state — that is the point of a bounded pool — so it is
    /// an `Option` the caller must handle rather than a panic.
    pub fn alloc(&mut self) -> Option<Slot> {
        self.free.pop().map(Slot)
    }

    /// Return a layer to the pool. Its contents are undefined afterwards.
    pub fn free(&mut self, slot: Slot) {
        debug_assert!(
            !self.free.contains(&slot.0),
            "layer {} freed twice; Slot is move-only to prevent exactly this",
            slot.0
        );
        self.free.push(slot.0);
    }

    /// Number of layers currently allocated.
    #[must_use]
    pub fn used(&self) -> u32 {
        self.capacity - self.free.len() as u32
    }

    #[must_use]
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Bytes currently held by allocated layers.
    #[must_use]
    pub fn bytes_used(&self) -> u64 {
        u64::from(self.used()) * tile_bytes(self.bytes_per_texel)
    }

    /// The whole array, for sampling.
    #[must_use]
    pub fn array_view(&self) -> &wgpu::TextureView {
        &self.array_view
    }

    /// One layer as a render target.
    #[must_use]
    pub fn layer_view(&self, slot: &Slot) -> &wgpu::TextureView {
        &self.layer_views[slot.0 as usize]
    }

    /// The backing texture, for copies to and from tiles.
    #[must_use]
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Clear one layer to a colour, as its own render pass.
    ///
    /// A load-op clear rather than a `write_texture` of a filled buffer: no CPU-side
    /// buffer, and no upload across the shared memory bus that the Surface-class target
    /// makes expensive (DECISIONS §2). Fresh canvas tiles are cleared to paper this way.
    pub fn clear_layer(&self, encoder: &mut wgpu::CommandEncoder, slot: &Slot, color: wgpu::Color) {
        encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tile-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.layer_view(slot),
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            })
            .forget_lifetime();
    }

    /// Copy one whole tile from another pool's layer into this one's.
    ///
    /// GPU-to-GPU, which is what keeps undo snapshots off the readback path
    /// (OPEN_QUESTIONS Q13): nothing comes back to the CPU while drawing.
    pub fn copy_layer_from(
        encoder: &mut wgpu::CommandEncoder,
        src: &Self,
        src_slot: &Slot,
        dst: &Self,
        dst_slot: &Slot,
    ) {
        debug_assert_eq!(
            src.texture.format(),
            dst.texture.format(),
            "tile copy between different formats"
        );
        encoder.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: &src.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: src_slot.0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: &dst.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: dst_slot.0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: TILE_SIZE as u32,
                height: TILE_SIZE as u32,
                depth_or_array_layers: 1,
            },
        );
    }
}

/// Which tile coordinate currently lives in which pool layer.
///
/// Separate from [`TilePool`] because the same slab is wanted with different keys and
/// lifetimes: the canvas keeps tiles for the document's life, a stroke's accumulation
/// keeps them until the stroke commits, and undo keeps them until the op is evicted.
#[derive(Default)]
pub struct TileMap {
    slots: HashMap<TileCoord, Slot>,
}

impl TileMap {
    /// The layer holding `coord`, if it is resident.
    #[must_use]
    pub fn slot(&self, coord: TileCoord) -> Option<&Slot> {
        self.slots.get(&coord)
    }

    #[must_use]
    pub fn contains(&self, coord: TileCoord) -> bool {
        self.slots.contains_key(&coord)
    }

    /// Insert a slot for `coord`. Returns any slot it displaced, which the caller must
    /// free — returning it rather than dropping it is what makes a leak impossible to
    /// write by accident.
    #[must_use = "the displaced slot must be returned to the pool"]
    pub fn insert(&mut self, coord: TileCoord, slot: Slot) -> Option<Slot> {
        self.slots.insert(coord, slot)
    }

    /// Take the slot for `coord` out of the map, handing ownership to the caller.
    #[must_use = "the slot must be returned to the pool or handed to another owner"]
    pub fn take(&mut self, coord: TileCoord) -> Option<Slot> {
        self.slots.remove(&coord)
    }

    pub fn iter(&self) -> impl Iterator<Item = (TileCoord, &Slot)> {
        self.slots.iter().map(|(c, s)| (*c, s))
    }

    /// Take every slot out, leaving the map empty.
    pub fn drain(&mut self) -> impl Iterator<Item = (TileCoord, Slot)> + '_ {
        self.slots.drain()
    }

    /// Whether any tile is mapped.
    ///
    /// Test-only: it exists to assert a map was fully drained, which is how a leaked pool
    /// layer shows up. Production code asks the pool how many layers are in use instead.
    #[cfg(test)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_gpu::try_device;

    /// f16 RGBA, matching the canvas tile format.
    const BPT: u32 = 8;

    fn pool(device: &wgpu::Device, capacity: u32) -> TilePool {
        TilePool::new(
            device,
            "test-pool",
            wgpu::TextureFormat::Rgba16Float,
            capacity,
            BPT,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
        )
    }

    #[test]
    fn a_tile_is_the_size_we_think_it_is() {
        // 256x256 RGBA f16. If this changes, every budget in the app changes with it,
        // so it is worth stating out loud rather than leaving implied.
        assert_eq!(tile_bytes(BPT), 512 * 1024);
    }

    #[test]
    fn the_budget_decides_the_capacity() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        // 64 MiB of 512 KiB tiles.
        let n = layers_for_budget(&device, BPT, 64 * 1024 * 1024);
        let limit = device.limits().max_texture_array_layers;
        assert_eq!(n, 128.min(limit));
    }

    /// A budget too small for even one tile must still give a usable pool, because
    /// returning zero capacity would make every allocation fail with nowhere to paint.
    #[test]
    fn a_tiny_budget_still_yields_one_layer() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        assert_eq!(layers_for_budget(&device, BPT, 1), 1);
    }

    /// The device's array limit is a hard ceiling, and exceeding it fails texture
    /// creation outright -- so the budget must never win over it.
    #[test]
    fn the_device_limit_caps_the_budget() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let huge = 1024u64 * 1024 * 1024 * 1024;
        assert_eq!(
            layers_for_budget(&device, BPT, huge),
            device.limits().max_texture_array_layers
        );
    }

    #[test]
    fn allocation_is_bounded_by_capacity() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut p = pool(&device, 3);
        let slots: Vec<Slot> = std::iter::from_fn(|| p.alloc()).collect();
        assert_eq!(slots.len(), 3);
        assert_eq!(p.used(), p.capacity());
        assert!(p.alloc().is_none(), "handed out more layers than it has");
        assert_eq!(p.used(), 3);

        // Distinct layers, or two tiles would share memory.
        let mut layers: Vec<u32> = slots.iter().map(Slot::layer).collect();
        layers.sort_unstable();
        layers.dedup();
        assert_eq!(layers.len(), 3, "duplicate layers handed out");
    }

    #[test]
    fn freeing_makes_a_layer_available_again() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut p = pool(&device, 1);
        let s = p.alloc().expect("first alloc");
        assert!(p.alloc().is_none());
        p.free(s);
        assert_eq!(p.used(), 0);
        assert!(p.alloc().is_some(), "freed layer was not reusable");
    }

    #[test]
    fn accounting_tracks_allocation() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut p = pool(&device, 4);
        assert_eq!(p.bytes_used(), 0);
        let s = p.alloc().expect("alloc");
        assert_eq!(p.bytes_used(), 512 * 1024);
        p.free(s);
        assert_eq!(p.bytes_used(), 0);
    }

    /// The map hands ownership back rather than dropping it, so a displaced or removed
    /// tile cannot leak its layer.
    #[test]
    fn the_map_returns_displaced_slots() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut p = pool(&device, 2);
        let mut m = TileMap::default();

        let a = p.alloc().expect("a");
        assert!(m.insert((0, 0), a).is_none());
        assert!(m.contains((0, 0)));

        let b = p.alloc().expect("b");
        let displaced = m.insert((0, 0), b).expect("should have displaced a");
        p.free(displaced);
        assert_eq!(p.used(), 1, "displaced layer was not reclaimed");

        let taken = m.take((0, 0)).expect("take");
        p.free(taken);
        assert!(m.is_empty());
        assert_eq!(p.used(), 0);
    }

    /// Negative tile coordinates are the normal case for a page extended up or left, so
    /// the map has to handle them like any other.
    #[test]
    fn negative_tile_coordinates_are_ordinary_keys() {
        let Some((device, _)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut p = pool(&device, 2);
        let mut m = TileMap::default();
        let s = p.alloc().expect("alloc");
        assert!(m.insert((-3, -7), s).is_none());
        assert!(m.contains((-3, -7)));
        assert!(!m.contains((3, 7)));
        let s = m.take((-3, -7)).expect("take");
        p.free(s);
    }
}
