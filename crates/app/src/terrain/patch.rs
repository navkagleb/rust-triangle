use bitflags::bitflags;
use glam::{IVec2, Vec2};

use super::config::PATCH_TERRAIN_SIZE;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PatchKey {
    pub grid_index: IVec2,
    pub lod_index: u32,
}

impl PatchKey {
    pub fn terrain_origin(&self) -> IVec2 {
        self.grid_index * PATCH_TERRAIN_SIZE as i32
    }

    pub fn terrain_size(&self) -> u32 {
        Self::terrain_size_for_lod(self.lod_index)
    }

    pub fn terrain_size_for_lod(lod_index: u32) -> u32 {
        PATCH_TERRAIN_SIZE * 2_u32.pow(lod_index)
    }

    pub fn terrain_center(&self) -> IVec2 {
        self.terrain_origin() + self.terrain_size() as i32 / 2
    }

    pub fn closest_point(&self, point: Vec2) -> Vec2 {
        let origin = self.terrain_origin().as_vec2();
        let size = self.terrain_size() as f32;

        point.clamp(origin, origin + size)
    }
}

#[derive(Default)]
pub struct PatchData {
    pub heights: Vec<f32>,
    pub gradients: Vec<Vec2>,
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
