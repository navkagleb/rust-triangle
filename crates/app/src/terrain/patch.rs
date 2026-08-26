use bitflags::bitflags;
use glam::{IVec2, UVec2, Vec2};

use super::config::PATCH_TERRAIN_SIZE;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct PatchKey {
    pub(super) grid_index: IVec2,
    pub(super) lod_index: u32,
}

impl PatchKey {
    pub(super) fn terrain_origin(&self) -> IVec2 {
        self.grid_index * PATCH_TERRAIN_SIZE as i32
    }

    pub(super) fn terrain_size(&self) -> u32 {
        PATCH_TERRAIN_SIZE * 2_u32.pow(self.lod_index)
    }

    pub(super) fn terrain_center(&self) -> IVec2 {
        self.terrain_origin() + self.terrain_size() as i32 / 2
    }
}

pub(super) enum PatchState {
    GenerationQueued,
    CpuGenerated { heights: Vec<f32>, gradients: Vec<Vec2> },
    GpuUploadPending { atlas_slot: UVec2, submitted_frame: u64 },
    Resident { atlas_slot: UVec2 },
}

bitflags! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug)]
    pub(super) struct PatchStitchMask: u32 {
        const TOP = 1 << 0;
        const BOTTOM = 1 << 1;
        const LEFT = 1 << 2;
        const RIGHT = 1 << 3;
    }
}
