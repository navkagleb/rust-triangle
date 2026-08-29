pub(super) const PATCH_GEN_THREAD_COUNT: usize = 8;

pub(super) const PATCH_LOD_COUNT: u32 = 6;
pub(super) const PATCH_PIXEL_SIZE: u32 = 128;
pub(super) const PATCH_TERRAIN_SIZE: u32 = PATCH_PIXEL_SIZE / 2;

pub(super) const ATLAS_PATCH_PIXEL_SIZE: usize = PATCH_PIXEL_SIZE as usize + 1; // for pixel overlap
pub(super) const ATLAS_PATCH_PIXEL_SIZE_WITH_BORDER: usize = ATLAS_PATCH_PIXEL_SIZE + 2; // for gradient generation
pub(super) const ATLAS_PATCH_COUNT: u32 = 32;
pub(super) const ATLAS_SIZE: u32 = ATLAS_PATCH_PIXEL_SIZE as u32 * ATLAS_PATCH_COUNT;
pub(super) const INDIRECTION_SLOT_COUNT: u32 = 512;

pub(super) const PATCH_SIDE_QUAD_COUNT: u32 = PATCH_PIXEL_SIZE;
pub(super) const PATCH_SIDE_VERTEX_COUNT: u32 = PATCH_PIXEL_SIZE + 1;
pub(super) const PATCH_INDEX_COUNT: u32 = PATCH_SIDE_QUAD_COUNT.pow(2) * 6;

pub(super) const NOISE_SCALE: f64 = 4.0;
pub(super) const NOISE_WORLD_SCALE: f64 = 2048.0;
