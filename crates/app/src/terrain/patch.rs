use glam::Vec2;

use super::config::{PATCH_SIZE_IN_METERS, WORLD_SIZE_IN_METERS};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PatchKey {
    pub x: u32,
    pub z: u32,
    pub lod: u32,
}

impl PatchKey {
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

#[derive(Default)]
pub struct PatchData {
    pub heights: Vec<f32>,
    pub gradients: Vec<Vec2>,
    pub height_range: Vec2,
}
