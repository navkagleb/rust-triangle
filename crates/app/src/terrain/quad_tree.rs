use glam::{Vec2, Vec3};

use super::cache::{Cache, PatchAvailability};
use super::config::{PATCH_LOD_COUNT, PATCH_SIZE_IN_METERS};
use super::patch::{PatchCoord, PatchIndex, PatchLayout};
use super::texture_atlas::AtlasSlot;

pub struct RenderablePatch {
    pub coord: PatchCoord,
    pub atlas_slot: AtlasSlot,
}

pub struct MissingPatch {
    pub coord: PatchCoord,
    pub coverage_required: bool,
}

#[derive(Default)]
pub struct PatchSelection {
    pub renderable: Vec<RenderablePatch>,
    pub missing: Vec<MissingPatch>,
    /// Every patch this frame depends on, renderable ones included. The cache stamps
    /// `last_needed_frame` on these, which is what keeps them out of eviction.
    pub needed: Vec<PatchIndex>,
}

impl PatchSelection {
    pub fn clear(&mut self) {
        self.renderable.clear();
        self.missing.clear();
        self.needed.clear();
    }
}

pub struct QuadTree {
    patches: Vec<TreePatch>,
}

impl QuadTree {
    pub fn new() -> Self {
        let mut patches = vec![
            TreePatch {
                coord: PatchCoord::new(0, 0, PATCH_LOD_COUNT),
                height_range: Vec2::new(0.0, 1.0),
            };
            PatchLayout::TOTAL_PATCH_COUNT
        ];

        for lod in 0..PATCH_LOD_COUNT {
            let per_side = PatchLayout::side_count(lod as usize) as u32;

            for z in 0..per_side {
                for x in 0..per_side {
                    let coord = PatchCoord::new(x, z, lod);

                    patches[PatchLayout::patch_index(&coord).index()].coord = coord;
                }
            }
        }

        Self { patches }
    }

    pub fn select(
        &self,
        camera_pos: Vec3,
        height_scale: f32,
        lod_factor: f32,
        cache: &Cache,
        selection: &mut PatchSelection,
    ) {
        let params = SelectParams {
            camera_pos,
            height_scale,
            lod_factor,
            cache,
        };

        selection.clear();
        self.select_recursive(PatchIndex::ROOT, &params, selection);
    }

    pub fn split_distance(lod: u32, lod_factor: f32) -> f32 {
        debug_assert_ne!(lod, 0);
        (PATCH_SIZE_IN_METERS * (1 << lod)) as f32 * 0.5 * lod_factor
    }

    pub fn set_height_range(&mut self, coord: &PatchCoord, height_range: Vec2) {
        self.patches[PatchLayout::patch_index(coord).index()].height_range = height_range;
    }

    fn should_split(patch: &TreePatch, params: &SelectParams) -> bool {
        if patch.coord.lod == 0 {
            return false;
        }

        let origin = patch.coord.world_origin();
        let size = patch.coord.size_in_meters() as f32;

        let min = Vec3::new(origin.x, patch.height_range.x * params.height_scale, origin.y);
        let max = Vec3::new(
            origin.x + size,
            patch.height_range.y * params.height_scale,
            origin.y + size,
        );

        let closest_point = params.camera_pos.clamp(min, max);
        let split_distance = Self::split_distance(patch.coord.lod, params.lod_factor);

        params.camera_pos.distance_squared(closest_point) < split_distance * split_distance
    }

    fn select_recursive(&self, patch_index: PatchIndex, params: &SelectParams, selection: &mut PatchSelection) {
        let patch = &self.patches[patch_index.index()];
        let cache = params.cache;

        if Self::should_split(patch, params) {
            let first_child = PatchLayout::first_child_index(patch_index, patch.coord.lod as usize);

            // Four contiguous u32s in the cache's residency table - one cache line,
            // no hashing, and no need to touch the entry array at all.
            let all_children_resident = (0..4).all(|offset| cache.is_resident(first_child.sibling(offset)));

            if all_children_resident {
                for offset in 0..4 {
                    self.select_recursive(first_child.sibling(offset), params, selection);
                }

                return;
            }

            for offset in 0..4 {
                let child_index = first_child.sibling(offset);

                match cache.availability(child_index) {
                    PatchAvailability::Missing => {
                        selection.missing.push(MissingPatch {
                            coord: self.patches[child_index.index()].coord,
                            coverage_required: false,
                        });
                    }
                    PatchAvailability::Pending | PatchAvailability::Resident(_) => {
                        selection.needed.push(child_index);
                    }
                }
            }
        }

        match cache.availability(patch_index) {
            PatchAvailability::Missing => {
                selection.missing.push(MissingPatch {
                    coord: patch.coord,
                    coverage_required: true,
                });
            }
            PatchAvailability::Pending => {
                selection.needed.push(patch_index);
            }
            PatchAvailability::Resident(atlas_slot) => {
                selection.needed.push(patch_index);
                selection.renderable.push(RenderablePatch {
                    coord: patch.coord,
                    atlas_slot,
                });
            }
        }
    }
}

#[derive(Clone, Copy)]
struct TreePatch {
    coord: PatchCoord,
    height_range: Vec2,
}

struct SelectParams<'a> {
    camera_pos: Vec3,
    height_scale: f32,
    lod_factor: f32,
    cache: &'a Cache,
}
