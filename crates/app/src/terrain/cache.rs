use imgui_sys::*;
use windows::Win32::Graphics::Direct3D12::D3D12_GPU_DESCRIPTOR_HANDLE;

use super::config::{ATLAS_SLOT_COUNT, ATLAS_SLOT_COUNT_PER_SIDE};
use super::generator::GeneratedPatch;
use super::patch::{PatchCoord, PatchIndex, PatchLayout, PatchPayload};
use super::texture_atlas::AtlasSlot;

pub struct PatchUpload {
    pub payload: PatchPayload,
    pub atlas_slot: AtlasSlot,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PatchAvailability {
    Missing,
    Pending,
    Resident(AtlasSlot),
}

pub struct Cache {
    /// Siblings are contiguous here, which is what makes the hot
    /// `all_children_resident` check a single cache line.
    patch_residency: Vec<PatchResidency>,
    /// Live entries only - bounded by the ATLAS_SLOT_COUNT
    entries: Vec<Entry>,
    available_atlas_slots: Vec<AtlasSlot>,
}

impl Cache {
    pub fn new() -> Self {
        Self {
            patch_residency: vec![PatchResidency::ABSENT; PatchLayout::TOTAL_PATCH_COUNT],
            entries: Vec::with_capacity(ATLAS_SLOT_COUNT as usize),
            available_atlas_slots: Self::create_atlas_slots(),
        }
    }

    pub fn is_resident(&self, patch_index: PatchIndex) -> bool {
        self.patch_residency[patch_index.index()].is_resident()
    }

    /// Full classification, including the atlas slot. Costs one extra read into the
    /// dense entry array, and runs once per selected patch rather than per sibling.
    pub fn availability(&self, patch_index: PatchIndex) -> PatchAvailability {
        let Some(entry_index) = self.patch_residency[patch_index.index()].entry_index() else {
            return PatchAvailability::Missing;
        };

        match self.entries[entry_index].state {
            PatchState::Generated(_) | PatchState::PendingUpload(_) => PatchAvailability::Pending,
            PatchState::Resident(atlas_slot) => PatchAvailability::Resident(atlas_slot),
        }
    }

    pub fn update(
        &mut self,
        current_frame: u64,
        completed_frame: u64,
        needed_patches: &[PatchIndex],
    ) -> Vec<PatchUpload> {
        self.mark_needed(current_frame, needed_patches);

        let generated_count = self.update_states(current_frame, completed_frame);
        self.evict_resident(generated_count, current_frame);

        self.prepare_uploads(current_frame)
    }

    pub fn insert_generated(&mut self, generated: GeneratedPatch) {
        let patch_index = PatchLayout::patch_index(&generated.coord);

        assert_eq!(
            self.patch_residency[patch_index.index()],
            PatchResidency::ABSENT,
            "patch already owns a cache entry"
        );

        let entry_index = self.entries.len() as u32;

        self.entries.push(Entry {
            patch_index,
            coord: generated.coord,
            state: PatchState::Generated(generated.payload),
            last_needed_frame: 0,
        });

        self.patch_residency[patch_index.index()] = PatchResidency::new(entry_index, false);
    }

    pub unsafe fn render_imgui(&self, height_atlas: D3D12_GPU_DESCRIPTOR_HANDLE) {
        unsafe {
            ImGui_Begin(c"TerrainAtlas".as_ptr(), std::ptr::null_mut(), 0);

            let image_pos = ImGui_GetCursorScreenPos();
            let image_size = {
                let size = ImGui_GetContentRegionAvail();
                size.x.min(size.y)
            };

            ImGui_Image(
                ImTextureRef {
                    _TexData: std::ptr::null_mut(),
                    _TexID: height_atlas.ptr,
                },
                ImVec2 {
                    x: image_size,
                    y: image_size,
                },
            );

            let draw_list = ImGui_GetWindowDrawList();
            let slot_size = image_size / ATLAS_SLOT_COUNT_PER_SIDE as f32;

            for slot in &self.available_atlas_slots {
                ImDrawList_AddCircleFilled(
                    draw_list,
                    ImVec2 {
                        x: image_pos.x + slot_size * slot.coords().x as f32 + slot_size * 0.5,
                        y: image_pos.y + slot_size * slot.coords().y as f32 + slot_size * 0.5,
                    },
                    3.0,
                    0xFFFFFFFF,
                    5,
                );
            }

            ImGui_End();
        }
    }

    fn mark_needed(&mut self, frame_index: u64, needed_patches: &[PatchIndex]) {
        for patch_index in needed_patches {
            if let Some(entry_index) = self.patch_residency[patch_index.index()].entry_index() {
                self.entries[entry_index].last_needed_frame = frame_index;
            }
        }
    }

    fn update_states(&mut self, current_frame: u64, completed_frame: u64) -> usize {
        let mut generated_count = 0;

        // Reverse order so `swap_remove` only ever relocates an already-visited entry.
        for entry_index in (0..self.entries.len()).rev() {
            let entry = &self.entries[entry_index];
            let patch_index = entry.patch_index;
            let still_needed = entry.last_needed_frame == current_frame;

            match entry.state {
                PatchState::Generated(_) => {
                    if still_needed {
                        generated_count += 1;
                    } else {
                        self.remove_entry(entry_index);
                    }
                }
                PatchState::PendingUpload(upload) => {
                    if upload.submitted_frame > completed_frame {
                        continue;
                    }

                    if still_needed {
                        self.entries[entry_index].state = PatchState::Resident(upload.atlas_slot);
                        self.patch_residency[patch_index.index()] = PatchResidency::new(entry_index as u32, true);
                    } else {
                        self.remove_entry(entry_index);
                        self.available_atlas_slots.push(upload.atlas_slot);
                    }
                }
                PatchState::Resident(_) => {}
            }
        }

        generated_count
    }

    fn evict_resident(&mut self, required_slots: usize, current_frame: u64) {
        let slots_to_free = required_slots.saturating_sub(self.available_atlas_slots.len());

        if slots_to_free == 0 {
            return;
        }

        let mut candidates: Vec<_> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let PatchState::Resident(atlas_slot) = entry.state else {
                    return None;
                };

                if entry.last_needed_frame == current_frame {
                    return None;
                }

                Some(EvictionCandidate {
                    // Patch indices are stable across `swap_remove`; entry indices are not.
                    patch_index: entry.patch_index,
                    lod: entry.coord.lod,
                    atlas_slot,
                    last_needed_frame: entry.last_needed_frame,
                })
            })
            .collect();

        candidates.sort_by(|a, b| {
            a.last_needed_frame
                .cmp(&b.last_needed_frame)
                // Smaller LOD index is finer, so evict finer patches first
                .then_with(|| a.lod.cmp(&b.lod))
        });

        for candidate in candidates.into_iter().take(slots_to_free) {
            let entry_index = self.patch_residency[candidate.patch_index.index()]
                .entry_index()
                .expect("eviction candidate must still own its entry");

            self.remove_entry(entry_index);
            self.available_atlas_slots.push(candidate.atlas_slot);
        }
    }

    fn prepare_uploads(&mut self, current_frame: u64) -> Vec<PatchUpload> {
        let mut uploads = Vec::new();

        for entry_index in 0..self.entries.len() {
            let entry = &self.entries[entry_index];

            if entry.last_needed_frame != current_frame || !matches!(entry.state, PatchState::Generated(_)) {
                continue;
            }

            // `evict_resident` sizes the pool for exactly this loop, but a selection
            // wider than the atlas can outrun it. Leave the surplus `Generated` for a
            // later frame rather than panicking on an empty pool.
            let Some(atlas_slot) = self.available_atlas_slots.pop() else {
                break;
            };

            let entry = &mut self.entries[entry_index];

            let PatchState::Generated(payload) = &mut entry.state else {
                unreachable!("checked above")
            };
            let payload = std::mem::take(payload);

            entry.state = PatchState::PendingUpload(PatchUploadInfo {
                atlas_slot,
                submitted_frame: current_frame,
            });

            uploads.push(PatchUpload { atlas_slot, payload });
        }

        uploads
    }

    fn remove_entry(&mut self, entry_index: usize) {
        let removed_entry = self.entries.swap_remove(entry_index);
        self.patch_residency[removed_entry.patch_index.index()] = PatchResidency::ABSENT;

        if let Some(moved_entry) = self.entries.get(entry_index) {
            let residency = &mut self.patch_residency[moved_entry.patch_index.index()];
            *residency = PatchResidency::new(entry_index as u32, residency.is_resident());
        }
    }

    fn create_atlas_slots() -> Vec<AtlasSlot> {
        let mut slots = Vec::with_capacity(ATLAS_SLOT_COUNT as usize);

        for y in (0..ATLAS_SLOT_COUNT_PER_SIDE).rev() {
            for x in (0..ATLAS_SLOT_COUNT_PER_SIDE).rev() {
                slots.push(AtlasSlot::new(x, y));
            }
        }

        slots
    }
}

/// Residency of one patch packed into 32 bits: a resident flag plus the
/// index of the `Entry` that owns it. Small enough that four siblings share a cache
/// line, which is the whole point of the table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct PatchResidency(u32);

impl PatchResidency {
    const RESIDENT_BIT: u32 = 1 << 31;
    const ENTRY_MASK: u32 = Self::RESIDENT_BIT - 1;
    const ABSENT: Self = Self(Self::ENTRY_MASK);

    fn new(entry_index: u32, resident: bool) -> Self {
        debug_assert!(entry_index < Self::ENTRY_MASK);
        Self(entry_index | if resident { Self::RESIDENT_BIT } else { 0 })
    }

    fn is_resident(self) -> bool {
        self.0 & Self::RESIDENT_BIT != 0
    }

    fn entry_index(self) -> Option<usize> {
        let entry_index = self.0 & Self::ENTRY_MASK;
        (entry_index != Self::ENTRY_MASK).then_some(entry_index as usize)
    }
}

struct Entry {
    /// Back-pointer, so removal can clear `patch_residency` without searching for it.
    patch_index: PatchIndex,
    coord: PatchCoord,
    state: PatchState,
    last_needed_frame: u64,
}

enum PatchState {
    Generated(PatchPayload),
    PendingUpload(PatchUploadInfo),
    Resident(AtlasSlot),
}

/// `Copy` on purpose: `update_states` matches this out of a borrowed `Entry` by value,
/// which is what lets that borrow end before the arms mutate the cache.
#[derive(Clone, Copy)]
struct PatchUploadInfo {
    atlas_slot: AtlasSlot,
    submitted_frame: u64,
}

struct EvictionCandidate {
    patch_index: PatchIndex,
    lod: u32,
    atlas_slot: AtlasSlot,
    last_needed_frame: u64,
}

#[cfg(test)]
mod tests {
    use glam::Vec2;

    use super::*;

    fn generated(x: u32, z: u32, lod: u32) -> GeneratedPatch {
        GeneratedPatch {
            coord: PatchCoord::new(x, z, lod),
            height_range: Vec2::ZERO,
            payload: PatchPayload::default(),
        }
    }

    /// `patch_residency` and `entries` have to stay a bijection. `swap_remove` is the
    /// only thing that can break it, so every test ends here.
    fn assert_consistent(cache: &Cache) {
        for (entry_index, entry) in cache.entries.iter().enumerate() {
            let residency = cache.patch_residency[entry.patch_index.index()];

            assert_eq!(
                residency.entry_index(),
                Some(entry_index),
                "entry {entry_index} is not reachable from its own patch"
            );
            assert_eq!(
                residency.is_resident(),
                matches!(entry.state, PatchState::Resident(_)),
                "residency flag disagrees with entry {entry_index}"
            );
        }

        let reachable = cache
            .patch_residency
            .iter()
            .filter(|residency| residency.entry_index().is_some())
            .count();

        assert_eq!(reachable, cache.entries.len(), "table points at stale entries");
    }

    #[test]
    fn dropping_entries_keeps_the_patch_mapping_in_sync() {
        let mut cache = Cache::new();

        for x in 0..3 {
            cache.insert_generated(generated(x, 0, 0));
        }
        assert_consistent(&cache);

        // Need only the middle patch. Dropping the first entry makes `swap_remove`
        // relocate the survivor, which is exactly the case the fixup exists for.
        let survivor = PatchLayout::patch_index(&PatchCoord::new(1, 0, 0));
        cache.update(1, 0, &[survivor]);

        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.entries[0].coord, PatchCoord::new(1, 0, 0));
        assert_eq!(cache.availability(survivor), PatchAvailability::Pending);
        assert_consistent(&cache);
    }

    #[test]
    fn a_completed_upload_promotes_the_patch_to_resident() {
        let coord = PatchCoord::new(4, 5, 2);
        let patch_index = PatchLayout::patch_index(&coord);

        let mut cache = Cache::new();
        cache.insert_generated(generated(4, 5, 2));

        // Frame 1: needed and generated, so an upload is issued against a fresh slot.
        let uploads = cache.update(1, 0, &[patch_index]);
        let atlas_slot = uploads[0].atlas_slot;

        assert_eq!(uploads.len(), 1);
        assert_eq!(cache.availability(patch_index), PatchAvailability::Pending);
        assert!(!cache.is_resident(patch_index));

        // Frame 2 with the GPU caught up: the copy has landed, the patch goes resident.
        assert!(cache.update(2, 1, &[patch_index]).is_empty());
        assert_eq!(cache.availability(patch_index), PatchAvailability::Resident(atlas_slot));
        assert!(cache.is_resident(patch_index));
        assert_consistent(&cache);
    }

    #[test]
    fn an_abandoned_upload_returns_its_atlas_slot() {
        let patch_index = PatchLayout::patch_index(&PatchCoord::new(7, 7, 1));

        let mut cache = Cache::new();
        cache.insert_generated(generated(7, 7, 1));
        cache.update(1, 0, &[patch_index]);

        let slots_while_uploading = cache.available_atlas_slots.len();

        // Frame 2, and the camera has moved on: the entry goes away, but the slot it
        // was holding has to come back to the pool.
        cache.update(2, 1, &[]);

        assert_eq!(cache.availability(patch_index), PatchAvailability::Missing);
        assert!(cache.entries.is_empty());
        assert_eq!(cache.available_atlas_slots.len(), slots_while_uploading + 1);
        assert_consistent(&cache);
    }
}
