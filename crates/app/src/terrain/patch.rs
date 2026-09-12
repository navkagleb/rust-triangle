use glam::Vec2;

use super::config::{PATCH_COUNT_PER_SIDE, PATCH_LOD_COUNT, PATCH_SIZE_IN_METERS, WORLD_SIZE_IN_METERS};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PatchCoord {
    pub x: u32,
    pub z: u32,
    pub lod: u32,
}

impl PatchCoord {
    pub fn new(x: u32, z: u32, lod: u32) -> Self {
        Self { x, z, lod }
    }

    pub fn world_origin(&self) -> Vec2 {
        let size = self.size_in_meters() as f32;
        let half_world = (WORLD_SIZE_IN_METERS / 2) as f32;

        Vec2::new(self.x as f32, self.z as f32) * size - half_world
    }

    pub fn size_in_meters(&self) -> u32 {
        PATCH_SIZE_IN_METERS * (1 << self.lod)
    }

    pub fn world_center(&self) -> Vec2 {
        self.world_origin() + self.size_in_meters() as f32 * 0.5
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PatchIndex(u32);

impl PatchIndex {
    pub const ROOT: Self = Self(0);

    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub fn sibling(self, offset: u32) -> Self {
        debug_assert!(offset < 4);
        Self(self.0 + offset)
    }
}

/// The rule that flattens the whole patch pyramid into one dense index space, shared
/// by the quad tree and the cache so neither has to hash a [`PatchCoord`].
pub struct PatchLayout;

impl PatchLayout {
    pub const TOTAL_PATCH_COUNT: usize = *Self::PYRAMID_OFFSETS.last().unwrap();

    /// One slot per pyramid level, plus a final slot holding the whole pyramid's size.
    const PYRAMID_OFFSET_LEN: usize = PATCH_LOD_COUNT as usize + 1;
    const PYRAMID_OFFSETS: [usize; Self::PYRAMID_OFFSET_LEN] = Self::compute_pyramid_offsets();

    pub fn patch_index(coord: &PatchCoord) -> PatchIndex {
        let lod_offset = Self::PYRAMID_OFFSETS[coord.lod as usize] as u32;
        let local_index = Self::morton2(coord.x, coord.z);

        PatchIndex(lod_offset + local_index)
    }

    pub const fn side_count(lod: usize) -> usize {
        PATCH_COUNT_PER_SIDE as usize >> lod
    }

    /// Morton ordering puts a patch's four children contiguously at 4x its local index.
    pub fn first_child_index(patch_index: PatchIndex, lod: usize) -> PatchIndex {
        debug_assert!(lod > 0);

        let lod_offset = Self::PYRAMID_OFFSETS[lod];
        let local_index = patch_index.index() - lod_offset;

        let child_lod_offset = Self::PYRAMID_OFFSETS[lod - 1] as u32;
        let child_local_index = local_index as u32 * 4;

        PatchIndex(child_lod_offset + child_local_index)
    }

    const fn compute_pyramid_offsets() -> [usize; Self::PYRAMID_OFFSET_LEN] {
        let mut offsets = [0; Self::PYRAMID_OFFSET_LEN];
        let mut lod = PATCH_LOD_COUNT as usize;
        let mut level_start = 0;

        while lod > 0 {
            lod -= 1;
            offsets[lod] = level_start;
            level_start += Self::side_count(lod).pow(2);
        }

        *offsets.last_mut().unwrap() = level_start;

        offsets
    }

    fn morton2(x: u32, y: u32) -> u32 {
        let mut result = 0;

        for i in 0..16 {
            result |= ((x >> i) & 1) << (2 * i);
            result |= ((y >> i) & 1) << (2 * i + 1);
        }

        result
    }
}

#[derive(Default)]
pub struct PatchPayload {
    pub heights: Vec<f32>,
    pub gradients: Vec<Vec2>,
}
