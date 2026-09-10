pub const PATCH_WORKER_COUNT: u32 = 8;

pub const PATCH_SIZE_IN_METERS: u32 = 64;
pub const PATCH_SIZE_IN_PIXELS: u32 = PATCH_SIZE_IN_METERS * 2;

/// Root LOD: the world is 2^WORLD_LOD_INDEX LOD-0 patches per side.
pub const WORLD_LOD_INDEX: u32 = 9;

/// Coarsest LOD that actually renders. Above this, nodes always split
pub const PATCH_LOD_COUNT: u32 = 8;

pub const PATCH_COUNT_PER_SIDE: u32 = 1 << WORLD_LOD_INDEX;
pub const WORLD_SIZE_IN_METERS: u32 = PATCH_SIZE_IN_METERS * PATCH_COUNT_PER_SIDE;

pub const ATLAS_PATCH_SIZE_IN_PIXELS: usize = PATCH_SIZE_IN_PIXELS as usize + 1; // for pixel overlap
pub const ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER: usize = ATLAS_PATCH_SIZE_IN_PIXELS + 2; // for gradient generation
pub const ATLAS_PATCH_COUNT_PER_SIDE: u32 = 32;
pub const ATLAS_SIZE_IN_PIXELS_PER_SIDE: u32 = ATLAS_PATCH_SIZE_IN_PIXELS as u32 * ATLAS_PATCH_COUNT_PER_SIDE;

pub const PATCH_SIDE_QUAD_COUNT: u32 = PATCH_SIZE_IN_PIXELS;
pub const PATCH_SIDE_VERTEX_COUNT: u32 = PATCH_SIZE_IN_PIXELS + 1;
pub const PATCH_INDEX_COUNT: u32 = PATCH_SIDE_QUAD_COUNT.pow(2) * 6;

pub const NOISE_WORLD_SCALE: f64 = WORLD_SIZE_IN_METERS as f64 / 4.0;
