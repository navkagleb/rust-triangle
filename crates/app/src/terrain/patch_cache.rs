use std::collections::HashMap;

use imgui_sys::*;
use windows::Win32::Graphics::Direct3D12::D3D12_GPU_DESCRIPTOR_HANDLE;

use super::config::ATLAS_PATCH_COUNT;
use super::patch::{PatchData, PatchKey};
use super::patch_generator::GeneratedPatch;
use super::texture_atlas::AtlasSlot;

pub struct PatchUpload {
    pub atlas_slot: AtlasSlot,
    pub data: PatchData,
}

pub struct ResidentPatch {
    pub patch: PatchKey,
    pub atlas_slot: AtlasSlot,
}

pub struct PatchCacheFrame {
    pub uploads: Vec<PatchUpload>,
    pub resident: Vec<ResidentPatch>,
}

#[derive(PartialEq, Eq)]
pub enum PatchAvailability {
    Missing,
    Pending,
    Resident,
}

pub struct PatchCache {
    entries: HashMap<PatchKey, CacheEntry>,
    available_atlas_slots: Vec<AtlasSlot>,
}

impl PatchCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            available_atlas_slots: Self::create_atlas_slots(),
        }
    }

    pub fn availability(&self, patch: &PatchKey) -> PatchAvailability {
        match self.entries.get(patch).map(|entry| &entry.state) {
            None => PatchAvailability::Missing,
            Some(PatchState::Generated(_) | PatchState::PendingUpload(_)) => PatchAvailability::Pending,
            Some(PatchState::Resident(_)) => PatchAvailability::Resident,
        }
    }

    pub fn update<'a, I>(&mut self, current_frame: u64, completed_frame: u64, needed_patches: I) -> PatchCacheFrame
    where
        I: IntoIterator<Item = &'a PatchKey>,
    {
        self.mark_needed(current_frame, needed_patches);
        let generated_count = self.update_states(current_frame, completed_frame);

        self.evict_resident(generated_count, current_frame);

        let mut cache_frame = PatchCacheFrame {
            uploads: Vec::new(),
            resident: Vec::new(),
        };

        for (&patch, entry) in &mut self.entries {
            match &mut entry.state {
                PatchState::Generated(data) => {
                    if entry.last_needed_frame != current_frame {
                        continue;
                    }

                    let atlas_slot = self.available_atlas_slots.pop().unwrap();
                    let data = std::mem::take(data);

                    entry.state = PatchState::PendingUpload(PatchUploadInfo {
                        atlas_slot,
                        submitted_frame: current_frame,
                    });

                    cache_frame.uploads.push(PatchUpload { atlas_slot, data });
                }

                PatchState::PendingUpload(_) => {}

                PatchState::Resident(atlas_slot) => cache_frame.resident.push(ResidentPatch {
                    patch,
                    atlas_slot: *atlas_slot,
                }),
            }
        }

        cache_frame
    }

    pub fn insert_generated(&mut self, generated: GeneratedPatch) {
        self.entries.insert(
            generated.patch,
            CacheEntry {
                state: PatchState::Generated(generated.data),
                last_needed_frame: 0,
            },
        );
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
            let slot_size = image_size / ATLAS_PATCH_COUNT as f32;

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

    fn mark_needed<'a, I>(&mut self, frame_index: u64, needed_patches: I)
    where
        I: IntoIterator<Item = &'a PatchKey>,
    {
        for patch in needed_patches {
            if let Some(entry) = self.entries.get_mut(patch) {
                entry.last_needed_frame = frame_index;
            }
        }
    }

    fn update_states(&mut self, current_frame: u64, completed_frame: u64) -> usize {
        let mut entries_to_remove = Vec::new();
        let mut generated_count = 0;

        for (&patch, entry) in &mut self.entries {
            match &entry.state {
                PatchState::Generated(_) => {
                    if entry.last_needed_frame == current_frame {
                        generated_count += 1;
                    } else {
                        entries_to_remove.push(patch);
                    }
                }

                PatchState::PendingUpload(upload) => {
                    if upload.submitted_frame <= completed_frame {
                        let atlas_slot = upload.atlas_slot;

                        if entry.last_needed_frame == current_frame {
                            entry.state = PatchState::Resident(atlas_slot)
                        } else {
                            self.available_atlas_slots.push(atlas_slot);
                            entries_to_remove.push(patch);
                        }
                    }
                }

                PatchState::Resident(_) => {}
            }
        }

        for patch in entries_to_remove {
            self.entries.remove(&patch);
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
            .filter_map(|(&patch, entry)| {
                let PatchState::Resident(atlas_slot) = entry.state else {
                    return None;
                };

                if entry.last_needed_frame == current_frame {
                    return None;
                }

                Some(EvictionCandidate {
                    patch,
                    atlas_slot,
                    last_needed_frame: entry.last_needed_frame,
                })
            })
            .collect();

        candidates.sort_by(|a, b| {
            a.last_needed_frame
                .cmp(&b.last_needed_frame)
                // Smaller LOD index is finer, so evict finer patches first
                .then_with(|| a.patch.lod_index.cmp(&b.patch.lod_index))
        });

        for candidate in candidates.into_iter().take(slots_to_free) {
            self.entries.remove(&candidate.patch);
            self.available_atlas_slots.push(candidate.atlas_slot);
        }
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

struct CacheEntry {
    state: PatchState,
    last_needed_frame: u64,
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

struct EvictionCandidate {
    patch: PatchKey,
    atlas_slot: AtlasSlot,
    last_needed_frame: u64,
}
