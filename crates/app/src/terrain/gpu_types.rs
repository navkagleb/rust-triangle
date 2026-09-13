use glam::{Mat4, Vec3};

use super::texture_atlas::AtlasSlot;

#[repr(C)]
pub struct GpuTerrainPatch {
    pub atlas_slot: AtlasSlot,
    pub x: u32,
    pub z: u32,
    pub lod: u32,
}

#[repr(C)]
pub struct GpuTerrainConsts {
    pub world_to_clip: Mat4,
    pub sun_dir: Vec3,
    pub height_scale: f32,
    pub active_patch_buffer_index: u32,

    // Debug
    pub wireframe_pass: u32,
    pub display_normals: u32,
}
