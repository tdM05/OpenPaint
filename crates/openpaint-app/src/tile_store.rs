//! Tile residency: which tiles are on the GPU, and where the rest live.
//!
//! This is the second half of OPEN_QUESTIONS Q13. [`crate::tile_pool`] bounded how much the
//! GPU holds; this gives the tiles that don't fit somewhere to go, so document size stops
//! being limited by graphics memory at all.
//!
//! # Why it has to exist before layers
//!
//! A fully-inked A4 page at 300 DPI is 140 tiles. The pool holds ~192. So without spilling,
//! a second layer would already fail — and "you may have one and a half layers" is not a
//! layer stack. Layers are what makes residency a real constraint rather than a theoretical
//! one, which is why this comes first.
//!
//! # The design
//!
//! - **The GPU is authoritative for resident tiles**; the CPU holds the rest (DECISIONS §2).
//!   Spilled tiles are `openpaint_core::tile::Tile`, whose byte layout is exactly the GPU
//!   texture's, so spilling and restoring are memcpys rather than conversions.
//! - **Clean tiles evict for free.** A tile uploaded from the CPU and not painted since is
//!   already backed, so eviction just frees the layer — no readback at all. Panning around a
//!   large document evicts almost entirely clean tiles, which is what keeps the common case
//!   cheap.
//! - **Dirty eviction never stalls a frame.** The copy to a staging buffer goes out in its
//!   own submission and the layer is freed immediately: submissions execute in order, so a
//!   later reuse of that layer cannot overtake the copy. The buffer maps asynchronously and
//!   is drained over following frames. Asking for a tile whose readback is still in flight
//!   forces a resolve, which is rare enough to be worth the simplicity.
//! - **Use ordering is a counter, not a list.** Each resident tile stores when it was last
//!   touched; touching is one store, and the O(n) scan happens only when choosing a victim.
//!   With n ≤ a few hundred that is the right trade.
//!
//! # A victim must be from an earlier frame
//!
//! Not an optimisation — a correctness rule, and the same hazard as DECISIONS §11a.2 in
//! another costume. Eviction submits its copy *before* the caller's still-unsubmitted
//! encoder. If the current frame has already recorded paint into a tile and we evict it now,
//! the copy reads the pre-paint contents and that paint is silently lost when the tile comes
//! back. So victims are restricted to tiles untouched this frame, and running out of those
//! is reported rather than worked around.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use openpaint_core::tile::{Tile, TileCoord, TILE_BYTES, TILE_SIZE};

use crate::canvas_renderer::{CANVAS_BYTES_PER_TEXEL, CANVAS_FORMAT};
use crate::tile_pool::{layers_for_budget, Slot, TilePool};

/// Which layer a tile belongs to.
///
/// Present from the start, with a single layer, so that adding the layer stack does not mean
/// rekeying the whole store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerId(pub u32);

/// A tile's identity: which layer, and where on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub layer: LayerId,
    pub coord: TileCoord,
}

impl TileKey {
    #[must_use]
    pub fn new(layer: LayerId, coord: TileCoord) -> Self {
        Self { layer, coord }
    }
}

/// GPU memory resident tiles may occupy, chosen from what the adapter looks like.
///
/// wgpu exposes no way to ask how much graphics memory exists, so this is a **heuristic, not
/// a measurement**, and it is named as one. An integrated GPU shares system memory with
/// everything else (DECISIONS §2), so it gets the conservative figure; a discrete card can
/// spare more. Either way the array-layer limit caps a single pool at 256 tiles (128 MiB),
/// and that is enough because the composite cache means residency does not scale with layer
/// count.
///
/// This wants to become a user setting once there is somewhere to put it.
#[must_use]
pub fn budget_for(device_type: wgpu::DeviceType) -> u64 {
    match device_type {
        wgpu::DeviceType::DiscreteGpu => 128 * 1024 * 1024,
        // Integrated, virtual, CPU, or unknown: assume memory is shared and precious.
        _ => 64 * 1024 * 1024,
    }
}

struct Resident {
    slot: Slot,
    /// Painted since it was last written to the CPU, so eviction owes a readback.
    dirty: bool,
    /// Frame counter value when this tile was last touched.
    used: u64,
}

/// A dirty tile on its way to the CPU.
struct Eviction {
    key: TileKey,
    buffer: wgpu::Buffer,
    mapped: Arc<AtomicBool>,
}

/// What a tile that did not exist yet should start as.
#[derive(Clone, Copy, Debug)]
pub enum Init {
    /// Clear it to a colour through the encoder.
    Clear(wgpu::Color),
    /// Leave it alone: the caller is about to overwrite every texel.
    ///
    /// Not an optimisation. A clear goes through the encoder while a full upload goes through
    /// `Queue::write_texture`, and every queue write in a submission is applied *before* any
    /// of its commands — so a clear here would be reordered *after* the upload and wipe it.
    /// DECISIONS §11a.2; it has now caught this project out three times.
    Untouched,
}

/// Why a tile could not be made resident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressure {
    /// Every resident tile has already been touched this frame, so none can be evicted
    /// safely. The frame's working set is larger than the pool.
    WorkingSetTooLarge,
}

pub struct TileStore {
    pool: TilePool,
    resident: HashMap<TileKey, Resident>,
    /// Tiles not on the GPU. Byte-identical to the texture format.
    spilled: HashMap<TileKey, Tile>,
    /// Evictions whose readback has not landed yet.
    inflight: Vec<Eviction>,
    /// Staging buffers kept for reuse; a tile is 512 KiB, so churning them per eviction
    /// would be the most expensive part of spilling.
    staging: Vec<wgpu::Buffer>,
    /// Increments once per frame. Victim selection compares against it.
    frame: u64,
    /// Times a tile had to be uploaded back from the CPU, for diagnostics.
    restores: u64,
    /// Times a dirty tile was read back, for diagnostics.
    spills: u64,
}

impl TileStore {
    pub fn new(device: &wgpu::Device, budget_bytes: u64) -> Self {
        let capacity = layers_for_budget(device, CANVAS_BYTES_PER_TEXEL, budget_bytes);
        Self {
            pool: TilePool::new(
                device,
                "canvas-tiles",
                CANVAS_FORMAT,
                capacity,
                CANVAS_BYTES_PER_TEXEL,
                // RENDER_ATTACHMENT so a stroke can bake into a tile; COPY_SRC/DST for undo
                // snapshots, export, spilling, and restoring.
                wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
            ),
            resident: HashMap::new(),
            spilled: HashMap::new(),
            inflight: Vec::new(),
            staging: Vec::new(),
            frame: 1,
            restores: 0,
            spills: 0,
        }
    }

    #[must_use]
    pub fn pool(&self) -> &TilePool {
        &self.pool
    }

    /// Resident tiles and capacity.
    #[must_use]
    pub fn residency(&self) -> (u32, u32) {
        (self.pool.used(), self.pool.capacity())
    }

    /// Tiles held on the CPU because they did not fit on the GPU.
    #[must_use]
    pub fn spilled_count(&self) -> usize {
        self.spilled.len() + self.inflight.len()
    }

    /// Readbacks and re-uploads so far, for judging whether the budget is thrashing.
    #[must_use]
    pub fn traffic(&self) -> (u64, u64) {
        (self.spills, self.restores)
    }

    /// Every tile the store knows about, resident or not.
    pub fn keys(&self) -> impl Iterator<Item = TileKey> + '_ {
        self.resident
            .keys()
            .chain(self.spilled.keys())
            .chain(self.inflight.iter().map(|e| &e.key))
            .copied()
    }

    #[must_use]
    pub fn contains(&self, key: TileKey) -> bool {
        self.resident.contains_key(&key)
            || self.spilled.contains_key(&key)
            || self.inflight.iter().any(|e| e.key == key)
    }

    /// The pool layer holding `key`, if it is resident. Does **not** count as a use.
    #[must_use]
    pub fn slot(&self, key: TileKey) -> Option<&Slot> {
        self.resident.get(&key).map(|r| &r.slot)
    }

    /// Begin a frame: drain finished readbacks and advance the counter victim selection uses.
    pub fn begin_frame(&mut self) {
        self.drain_inflight();
        self.frame += 1;
    }

    /// Note that a tile's contents changed on the GPU, so eviction owes a readback.
    pub fn mark_dirty(&mut self, key: TileKey) {
        if let Some(r) = self.resident.get_mut(&key) {
            r.dirty = true;
            r.used = self.frame;
        }
    }

    /// Make `key` resident, restoring it from the CPU or creating it, and return its layer.
    ///
    /// `fresh` says what a tile that did not exist at all should start as. Layer tiles want
    /// transparent; the paper belongs to the compositor, not to the pixels.
    pub fn ensure(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        key: TileKey,
        fresh: Init,
    ) -> Result<u32, Pressure> {
        if let Some(r) = self.resident.get_mut(&key) {
            r.used = self.frame;
            return Ok(r.slot.layer());
        }

        // A readback still in flight has to land before the tile can come back, or the
        // restore would upload whatever the CPU copy held before this eviction.
        if self.inflight.iter().any(|e| e.key == key) {
            self.resolve(device, key);
        }

        let slot = self.take_slot(device, queue)?;
        let layer = slot.layer();

        match self.spilled.remove(&key) {
            Some(tile) => {
                // Restoring must be ordered against the commands that will read this layer,
                // and `write_texture` is applied before every command in the submission --
                // which is exactly the order wanted here, since the caller has recorded
                // nothing for this layer yet.
                self.upload(queue, &slot, tile.bytes());
                self.restores += 1;
            }
            None => match fresh {
                Init::Clear(color) => self.pool.clear_layer(encoder, &slot, color),
                Init::Untouched => {}
            },
        }

        self.resident.insert(
            key,
            Resident {
                slot,
                dirty: false,
                used: self.frame,
            },
        );
        Ok(layer)
    }

    /// Take a tile out of the store entirely, giving up its layer.
    ///
    /// Used when undo removes a tile a stroke created, and when Trim discards out-of-page
    /// content. Any CPU copy goes too — this is a deletion, not an eviction.
    pub fn remove(&mut self, key: TileKey) -> Option<Slot> {
        self.spilled.remove(&key);
        self.inflight.retain(|e| e.key != key);
        self.resident.remove(&key).map(|r| r.slot)
    }

    /// Put a layer back under `key`, e.g. when undoing a Trim. Returns any displaced slot.
    #[must_use = "the displaced slot must be returned to a pool"]
    pub fn insert(&mut self, key: TileKey, slot: Slot) -> Option<Slot> {
        self.spilled.remove(&key);
        let displaced = self.resident.insert(
            key,
            Resident {
                slot,
                // It came from outside the store, so the CPU has no copy of it.
                dirty: true,
                used: self.frame,
            },
        );
        displaced.map(|r| r.slot)
    }

    /// Allocate a bare layer, for a caller that owns the contents itself.
    pub fn alloc_bare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Slot> {
        self.take_slot(device, queue).ok()
    }

    /// Return a layer to the pool.
    pub fn release(&mut self, slot: Slot) {
        self.pool.free(slot);
    }

    /// Take a free pool layer, evicting to make room if necessary.
    fn take_slot(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Slot, Pressure> {
        loop {
            if let Some(slot) = self.pool.alloc() {
                return Ok(slot);
            }
            self.evict_one(device, queue)?;
        }
    }

    /// Spill the least recently used tile that this frame has not touched.
    fn evict_one(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<(), Pressure> {
        let victim = self
            .resident
            .iter()
            // Untouched this frame -- see the module note on why this is correctness.
            .filter(|(_, r)| r.used < self.frame)
            .min_by_key(|(k, r)| (r.used, **k))
            .map(|(k, _)| *k)
            .ok_or(Pressure::WorkingSetTooLarge)?;

        let Resident { slot, dirty, .. } = self
            .resident
            .remove(&victim)
            .expect("just selected from the map");

        if dirty {
            self.spill(device, queue, victim, &slot);
        }
        // A clean tile is already backed on the CPU, so its layer is simply free.
        self.pool.free(slot);
        Ok(())
    }

    /// Start a readback of a dirty tile and free its layer.
    fn spill(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, key: TileKey, slot: &Slot) {
        let buffer = self.staging.pop().unwrap_or_else(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tile-spill-staging"),
                size: TILE_BYTES as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        });

        // Its own submission, deliberately: it must be ordered *before* whatever the caller
        // records next into this layer, and submissions run in order.
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tile-spill"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: self.pool.texture(),
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: slot.layer(),
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    // A tile row is 256 × 8 = 2048 bytes, already a multiple of the 256-byte
                    // copy alignment, so there is no padding to skip.
                    bytes_per_row: Some(TILE_SIZE as u32 * CANVAS_BYTES_PER_TEXEL),
                    rows_per_image: Some(TILE_SIZE as u32),
                },
            },
            wgpu::Extent3d {
                width: TILE_SIZE as u32,
                height: TILE_SIZE as u32,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));

        let mapped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&mapped);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if result.is_ok() {
                    flag.store(true, Ordering::Release);
                }
            });

        self.spills += 1;
        self.inflight.push(Eviction {
            key,
            buffer,
            mapped,
        });
    }

    /// Move any finished readbacks into the CPU store.
    ///
    /// Never blocks: a readback that is not ready yet is simply looked at again next frame.
    fn drain_inflight(&mut self) {
        let mut i = 0;
        while i < self.inflight.len() {
            if self.inflight[i].mapped.load(Ordering::Acquire) {
                let e = self.inflight.swap_remove(i);
                self.land(e);
            } else {
                i += 1;
            }
        }
    }

    /// Block until `key`'s readback lands, because something needs it now.
    fn resolve(&mut self, device: &wgpu::Device, key: TileKey) {
        // The callback only runs while the device is polled, so poll until it has.
        while self.inflight.iter().any(|e| e.key == key) {
            device.poll(wgpu::Maintain::Wait);
            let before = self.inflight.len();
            self.drain_inflight();
            if self.inflight.len() == before {
                // Nothing landed despite a blocking poll: the mapping failed, and looping
                // again would hang. Drop it rather than spin -- the tile reverts to whatever
                // the CPU last held, which is the same outcome as a lost eviction.
                self.inflight.retain(|e| e.key != key);
                return;
            }
        }
    }

    /// Copy a mapped staging buffer into the CPU store and recycle the buffer.
    fn land(&mut self, e: Eviction) {
        {
            let view = e.buffer.slice(..).get_mapped_range();
            if let Some(tile) = Tile::from_bytes(&view) {
                self.spilled.insert(e.key, tile);
            } else {
                debug_assert!(false, "spilled tile was the wrong size");
            }
        }
        e.buffer.unmap();
        self.staging.push(e.buffer);
    }

    /// Put a tile straight into the CPU side, without touching the GPU.
    ///
    /// How a document is loaded. Uploading everything would try to put a whole sketchbook into
    /// graphics memory at once; dropping it here instead means residency pulls in only what the
    /// viewport asks for, so opening a large document is fast and bounded for free.
    ///
    /// Marked clean, because the CPU copy *is* the authority until something paints on it.
    pub fn preload(&mut self, key: TileKey, tile: Tile) {
        if let Some(old) = self.resident.remove(&key) {
            self.pool.free(old.slot);
        }
        self.inflight.retain(|e| e.key != key);
        self.spilled.insert(key, tile);
    }

    /// Forget everything. For loading a document over the top of the current one.
    pub fn clear(&mut self) {
        for (_, r) in std::mem::take(&mut self.resident) {
            self.pool.free(r.slot);
        }
        self.spilled.clear();
        // Let the readbacks land into a map nobody will read rather than cancelling them, which
        // is not a thing wgpu offers.
        self.inflight.clear();
    }

    /// Every tile, resident or spilled, as CPU bytes.
    ///
    /// For saving. Resident tiles are read back in **one** batched copy rather than one per tile:
    /// a fully-inked page is a hundred-plus tiles, and a hundred separate map-and-wait round
    /// trips would make saving feel broken. Spilled tiles are already here and cost nothing.
    ///
    /// Stalls on the GPU, which is fine for an explicit save -- the drawing path never does this.
    pub fn snapshot_all(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Vec<(TileKey, Tile)> {
        self.settle(device);

        let spilled: Vec<(TileKey, Tile)> =
            self.spilled.iter().map(|(k, t)| (*k, t.clone())).collect();
        let resident: Vec<TileKey> = self.resident.keys().copied().collect();
        let mut out = self.read_back(device, queue, resident);
        out.extend(spilled);
        out
    }

    /// Read specific tiles, wherever they currently live.
    ///
    /// The targeted counterpart of [`snapshot_all`], for the eyedropper: sampling one pixel needs
    /// one tile per layer, and reading the entire working set — up to the whole budget — to answer
    /// a single click would be absurd. Missing keys are simply absent from the result, which is the
    /// right answer for a coordinate no layer has painted.
    ///
    /// [`snapshot_all`]: TileStore::snapshot_all
    pub fn snapshot_some(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        keys: &[TileKey],
    ) -> Vec<(TileKey, Tile)> {
        self.settle(device);

        let mut out = Vec::new();
        let mut resident = Vec::new();
        for key in keys {
            if let Some(tile) = self.spilled.get(key) {
                out.push((*key, tile.clone()));
            } else if self.resident.contains_key(key) {
                resident.push(*key);
            }
        }
        out.extend(self.read_back(device, queue, resident));
        out
    }

    /// Land every readback still in flight.
    ///
    /// Anything in flight has to be resolved before its tile is read, or the copy would be stale.
    fn settle(&mut self, device: &wgpu::Device) {
        self.drain_inflight();
        let pending: Vec<TileKey> = self.inflight.iter().map(|e| e.key).collect();
        for key in pending {
            self.resolve(device, key);
        }
    }

    /// Copy resident tiles off the GPU.
    ///
    /// One buffer and one submission for the whole batch, whether that batch is the entire working
    /// set (a save) or a handful (a colour sample). Sharing the path means the alignment reasoning
    /// below is written once and cannot drift between the two callers.
    fn read_back(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        keys: Vec<TileKey>,
    ) -> Vec<(TileKey, Tile)> {
        let resident: Vec<(TileKey, u32)> = keys
            .into_iter()
            .filter_map(|k| self.resident.get(&k).map(|r| (k, r.slot.layer())))
            .collect();
        let mut out = Vec::new();
        if resident.is_empty() {
            return out;
        }

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tile-save-readback"),
            size: (TILE_BYTES * resident.len()) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tile-save"),
        });
        for (i, (_, layer)) in resident.iter().enumerate() {
            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: self.pool.texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: *layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &buffer,
                    layout: wgpu::ImageDataLayout {
                        // A tile is 512 KiB and a row 2048 bytes, both multiples of the 256-byte
                        // copy alignment, so tiles pack end to end with no padding.
                        offset: (TILE_BYTES * i) as u64,
                        bytes_per_row: Some(TILE_SIZE as u32 * CANVAS_BYTES_PER_TEXEL),
                        rows_per_image: Some(TILE_SIZE as u32),
                    },
                },
                wgpu::Extent3d {
                    width: TILE_SIZE as u32,
                    height: TILE_SIZE as u32,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit(std::iter::once(encoder.finish()));
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::Maintain::Wait);

        let view = buffer.slice(..).get_mapped_range();
        for (i, (key, _)) in resident.iter().enumerate() {
            let start = TILE_BYTES * i;
            if let Some(tile) = Tile::from_bytes(&view[start..start + TILE_BYTES]) {
                out.push((*key, tile));
            } else {
                debug_assert!(false, "a tile read back the wrong size");
            }
        }
        out
    }

    /// Upload tile bytes into a pool layer.
    fn upload(&self, queue: &wgpu::Queue, slot: &Slot, bytes: &[u8]) {
        debug_assert_eq!(bytes.len(), TILE_BYTES);
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: self.pool.texture(),
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: slot.layer(),
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(TILE_SIZE as u32 * CANVAS_BYTES_PER_TEXEL),
                rows_per_image: Some(TILE_SIZE as u32),
            },
            wgpu::Extent3d {
                width: TILE_SIZE as u32,
                height: TILE_SIZE as u32,
                depth_or_array_layers: 1,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_gpu::try_device;

    const TRANSPARENT: Init = Init::Clear(wgpu::Color::TRANSPARENT);
    const L0: LayerId = LayerId(0);

    fn key(x: i32, y: i32) -> TileKey {
        TileKey::new(L0, (x, y))
    }

    /// A store with room for exactly `tiles` tiles, so eviction is reachable in a test.
    fn store(device: &wgpu::Device, tiles: u64) -> TileStore {
        TileStore::new(device, tiles * (TILE_BYTES as u64))
    }

    fn encoder(device: &wgpu::Device) -> wgpu::CommandEncoder {
        device.create_command_encoder(&Default::default())
    }

    #[test]
    fn the_budget_heuristic_is_gentler_on_shared_memory() {
        assert!(
            budget_for(wgpu::DeviceType::IntegratedGpu) < budget_for(wgpu::DeviceType::DiscreteGpu)
        );
        // Unknown hardware must get the cautious figure, not the generous one.
        assert_eq!(
            budget_for(wgpu::DeviceType::Other),
            budget_for(wgpu::DeviceType::IntegratedGpu)
        );
    }

    #[test]
    fn a_tile_stays_resident_once_created() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut s = store(device, 4);
        let mut enc = encoder(device);
        let layer = s
            .ensure(device, queue, &mut enc, key(0, 0), TRANSPARENT)
            .expect("room");
        assert_eq!(
            s.ensure(device, queue, &mut enc, key(0, 0), TRANSPARENT),
            Ok(layer),
            "a second ensure should return the same layer"
        );
        assert_eq!(s.residency().0, 1);
    }

    /// A clean tile costs nothing to evict, which is the case that keeps panning cheap.
    #[test]
    fn a_clean_tile_evicts_without_a_readback() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut s = store(device, 1);

        let mut enc = encoder(device);
        s.ensure(device, queue, &mut enc, key(0, 0), TRANSPARENT)
            .expect("first");
        queue.submit(std::iter::once(enc.finish()));

        // A later frame, so the first tile is an eligible victim.
        s.begin_frame();
        let mut enc = encoder(device);
        s.ensure(device, queue, &mut enc, key(1, 0), TRANSPARENT)
            .expect("evicts the first");
        queue.submit(std::iter::once(enc.finish()));

        assert_eq!(s.residency().0, 1, "pool should still hold one tile");
        assert_eq!(s.traffic().0, 0, "a clean tile should not be read back");
        assert!(!s.contains(key(0, 0)), "a clean eviction is a forget");
    }

    /// The property the whole module exists for: paint survives being pushed off the GPU and
    /// pulled back. If this is wrong, drawing on a large document quietly loses work.
    #[test]
    fn a_dirty_tile_survives_a_round_trip_through_the_cpu() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut s = store(device, 1);

        // Paint tile (0,0) a known colour by clearing it to red, and mark it dirty.
        let mut enc = encoder(device);
        s.ensure(
            device,
            queue,
            &mut enc,
            key(0, 0),
            Init::Clear(wgpu::Color::RED),
        )
        .expect("first");
        queue.submit(std::iter::once(enc.finish()));
        s.mark_dirty(key(0, 0));

        // Next frame: a second tile forces the first out, which now owes a readback.
        s.begin_frame();
        let mut enc = encoder(device);
        s.ensure(device, queue, &mut enc, key(1, 0), TRANSPARENT)
            .expect("evicts the first");
        queue.submit(std::iter::once(enc.finish()));
        assert_eq!(s.traffic().0, 1, "a dirty tile must be read back");
        assert!(s.contains(key(0, 0)), "the tile should still be known");

        // Next frame: ask for it again. It must come back with its pixels.
        s.begin_frame();
        let mut enc = encoder(device);
        let layer = s
            .ensure(device, queue, &mut enc, key(0, 0), TRANSPARENT)
            .expect("restored");
        queue.submit(std::iter::once(enc.finish()));
        assert_eq!(s.traffic().1, 1, "it should have been uploaded back");

        let texel = read_texel(device, queue, s.pool(), layer);
        assert!(
            texel[0] > 0.9 && texel[1] < 0.1 && texel[2] < 0.1,
            "the tile came back wrong: {texel:?}"
        );
    }

    /// Restoring must not resurrect a stale CPU copy: if a readback is still in flight when
    /// the tile is asked for again, it has to land first.
    #[test]
    fn a_restore_waits_for_an_in_flight_readback() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut s = store(device, 1);

        let mut enc = encoder(device);
        s.ensure(
            device,
            queue,
            &mut enc,
            key(0, 0),
            Init::Clear(wgpu::Color::GREEN),
        )
        .expect("first");
        queue.submit(std::iter::once(enc.finish()));
        s.mark_dirty(key(0, 0));

        s.begin_frame();
        let mut enc = encoder(device);
        s.ensure(device, queue, &mut enc, key(1, 0), TRANSPARENT)
            .expect("evict");
        queue.submit(std::iter::once(enc.finish()));

        // Deliberately *without* a begin_frame, so the readback has had no chance to be
        // drained: `ensure` must resolve it itself.
        let mut enc = encoder(device);
        // (1,0) was touched this frame, so it cannot be the victim; make room by removing it.
        if let Some(slot) = s.remove(key(1, 0)) {
            s.release(slot);
        }
        let layer = s
            .ensure(device, queue, &mut enc, key(0, 0), TRANSPARENT)
            .expect("restored");
        queue.submit(std::iter::once(enc.finish()));

        let texel = read_texel(device, queue, s.pool(), layer);
        assert!(
            texel[1] > 0.9 && texel[0] < 0.1,
            "restored from a stale copy: {texel:?}"
        );
    }

    /// A tile the current frame has already painted into must never be chosen as a victim:
    /// its paint is still in an unsubmitted encoder, so the eviction copy would miss it.
    /// Running out of eligible victims is reported instead.
    #[test]
    fn tiles_touched_this_frame_are_never_evicted() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut s = store(device, 2);
        let mut enc = encoder(device);

        s.ensure(device, queue, &mut enc, key(0, 0), TRANSPARENT)
            .expect("first");
        s.ensure(device, queue, &mut enc, key(1, 0), TRANSPARENT)
            .expect("second");

        // Both were touched this frame, so a third has nowhere to go.
        assert_eq!(
            s.ensure(device, queue, &mut enc, key(2, 0), TRANSPARENT),
            Err(Pressure::WorkingSetTooLarge)
        );
        assert!(
            s.contains(key(0, 0)) && s.contains(key(1, 0)),
            "an in-frame tile was evicted"
        );

        // A frame later, both are eligible and the third fits.
        queue.submit(std::iter::once(enc.finish()));
        s.begin_frame();
        let mut enc = encoder(device);
        s.ensure(device, queue, &mut enc, key(2, 0), TRANSPARENT)
            .expect("should fit once the frame moved on");
    }

    /// Least recently used, not arbitrary: the tile touched longest ago goes first.
    #[test]
    fn the_least_recently_used_tile_goes_first() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut s = store(device, 2);

        let mut enc = encoder(device);
        s.ensure(device, queue, &mut enc, key(0, 0), TRANSPARENT)
            .expect("a");
        s.ensure(device, queue, &mut enc, key(1, 0), TRANSPARENT)
            .expect("b");
        queue.submit(std::iter::once(enc.finish()));

        // Touch (0,0) again so (1,0) is the older of the two.
        s.begin_frame();
        let mut enc = encoder(device);
        s.ensure(device, queue, &mut enc, key(0, 0), TRANSPARENT)
            .expect("touch a");
        queue.submit(std::iter::once(enc.finish()));

        s.begin_frame();
        let mut enc = encoder(device);
        s.ensure(device, queue, &mut enc, key(2, 0), TRANSPARENT)
            .expect("c");
        queue.submit(std::iter::once(enc.finish()));

        assert!(s.slot(key(0, 0)).is_some(), "the recently used tile went");
        assert!(
            s.slot(key(1, 0)).is_none(),
            "the oldest tile should have gone"
        );
    }

    /// Layers share one pool, so the key has to keep their tiles apart. Same coordinate,
    /// different layer, must be a different tile.
    #[test]
    fn the_same_coordinate_in_two_layers_is_two_tiles() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut s = store(device, 4);
        let mut enc = encoder(device);
        let a = s
            .ensure(
                device,
                queue,
                &mut enc,
                TileKey::new(LayerId(0), (0, 0)),
                TRANSPARENT,
            )
            .expect("layer 0");
        let b = s
            .ensure(
                device,
                queue,
                &mut enc,
                TileKey::new(LayerId(1), (0, 0)),
                TRANSPARENT,
            )
            .expect("layer 1");
        assert_ne!(a, b, "two layers shared one pool layer");
        assert_eq!(s.residency().0, 2);
    }

    /// Removing is a deletion, not an eviction: no CPU copy may survive it, or a tile undo
    /// deleted would reappear the next time it was asked for.
    #[test]
    fn removing_a_tile_forgets_its_cpu_copy_too() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let mut s = store(device, 1);

        let mut enc = encoder(device);
        s.ensure(
            device,
            queue,
            &mut enc,
            key(0, 0),
            Init::Clear(wgpu::Color::RED),
        )
        .expect("first");
        queue.submit(std::iter::once(enc.finish()));
        s.mark_dirty(key(0, 0));

        // Spill it, then delete it.
        s.begin_frame();
        let mut enc = encoder(device);
        s.ensure(device, queue, &mut enc, key(1, 0), TRANSPARENT)
            .expect("evict");
        queue.submit(std::iter::once(enc.finish()));
        assert!(s.contains(key(0, 0)));

        assert!(s.remove(key(0, 0)).is_none(), "it was not resident");
        assert!(!s.contains(key(0, 0)), "the CPU copy outlived the delete");
    }

    /// Read the first texel of a pool layer back, for checking a round trip.
    fn read_texel(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pool: &TilePool,
        layer: u32,
    ) -> [f32; 4] {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("texel-readback"),
            size: TILE_BYTES as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: pool.texture(),
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(TILE_SIZE as u32 * CANVAS_BYTES_PER_TEXEL),
                    rows_per_image: Some(TILE_SIZE as u32),
                },
            },
            wgpu::Extent3d {
                width: TILE_SIZE as u32,
                height: TILE_SIZE as u32,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(enc.finish()));
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::Maintain::Wait);
        let view = buffer.slice(..).get_mapped_range();
        let halves: &[half::f16] = bytemuck::cast_slice(&view);
        [
            halves[0].to_f32(),
            halves[1].to_f32(),
            halves[2].to_f32(),
            halves[3].to_f32(),
        ]
    }
}
