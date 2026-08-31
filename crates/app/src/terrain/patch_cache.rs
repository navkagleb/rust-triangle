use std::collections::HashMap;

use super::config::ATLAS_PATCH_COUNT;
use super::patch::{PatchData, PatchKey};
use super::patch_generator::GeneratedPatch;
use super::patch_quad_tree::PatchQuadTree;
use super::texture_atlas::AtlasSlot;

pub struct PatchUpload {
    pub atlas_slot: AtlasSlot,
    pub data: PatchData,
}

pub struct ResidentPatch {
    pub patch: PatchKey,
    pub atlas_slot: AtlasSlot,
}

pub struct PatchCache {
    entries: HashMap<PatchKey, PatchState>,
    available_atlas_slots: Vec<AtlasSlot>,
}

impl PatchCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            available_atlas_slots: Self::create_atlas_slots(),
        }
    }

    pub fn is_resident(&self, patch: &PatchKey) -> bool {
        self.entries
            .get(patch)
            .is_some_and(|status| matches!(status, PatchState::Resident(_)))
    }

    pub fn contains(&self, patch: &PatchKey) -> bool {
        self.entries.contains_key(patch)
    }

    pub fn insert_generated(&mut self, generated: GeneratedPatch) {
        self.entries
            .insert(generated.patch, PatchState::Generated(generated.data));
    }

    pub fn prepare_uploads(&mut self, submitted_frame: u64) -> Vec<PatchUpload> {
        let mut uploads = Vec::new();

        for state in &mut self.entries.values_mut() {
            let PatchState::Generated(data) = state else {
                continue;
            };

            let atlas_slot = self.available_atlas_slots.pop().unwrap();
            let data = std::mem::take(data);

            *state = PatchState::PendingUpload(PatchUploadInfo {
                atlas_slot,
                submitted_frame,
            });

            uploads.push(PatchUpload { atlas_slot, data });
        }

        uploads
    }

    pub fn completed_uploads(&mut self, completed_frame: u64) {
        for state in self.entries.values_mut() {
            let PatchState::PendingUpload(upload_info) = state else {
                continue;
            };

            if upload_info.submitted_frame <= completed_frame {
                *state = PatchState::Resident(upload_info.atlas_slot)
            }
        }
    }

    pub fn collect_resident_patches(&self) -> Vec<ResidentPatch> {
        self.entries
            .iter()
            .filter_map(|(&patch, state)| {
                if let PatchState::Resident(atlas_slot) = *state {
                    return Some(ResidentPatch { patch, atlas_slot });
                }

                None
            })
            .collect()
    }

    pub fn evict_outside(&mut self, qtree: &PatchQuadTree) {
        self.entries.retain(|patch, state| {
            if qtree.contains_grid_index(patch.grid_index) {
                return true;
            }

            match state {
                PatchState::Generated(_) => {}
                PatchState::PendingUpload(upload_info) => self.available_atlas_slots.push(upload_info.atlas_slot),
                PatchState::Resident(atlas_slot) => self.available_atlas_slots.push(*atlas_slot),
            }

            false
        });
    }

    fn create_atlas_slots() -> Vec<AtlasSlot> {
        let mut slots = Vec::with_capacity((ATLAS_PATCH_COUNT * ATLAS_PATCH_COUNT) as usize);

        for y in (0..ATLAS_PATCH_COUNT).rev() {
            for x in (0..ATLAS_PATCH_COUNT).rev() {
                slots.push(AtlasSlot::new(x, y));
            }
        }

        slots
    }
}

enum PatchState {
    Generated(PatchData),
    PendingUpload(PatchUploadInfo),
    Resident(AtlasSlot),
}

struct PatchUploadInfo {
    atlas_slot: AtlasSlot,
    submitted_frame: u64,
}
