use glam::{IVec2, Mat4};

use super::texture_atlas::AtlasSlot;

#[repr(C)]
pub struct GpuTerrainPatch {
    pub grid_index: IVec2,
    pub atlas_slot: AtlasSlot,
    pub lod_index: u32,
}

#[repr(C)]
pub struct GpuTerrainConsts {
    pub world_to_clip: Mat4,
    pub height_scale: f32,
    pub elapsed_time: f32,
    pub active_patch_buffer_index: u32,

    // Debug
    pub wireframe_pass: u32,
    pub display_normals: u32,
}
