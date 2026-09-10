use std::collections::HashSet;

use glam::{Vec2, Vec3};

use super::cache::{Cache, PatchAvailability};
use super::config::{PATCH_COUNT_PER_SIDE, PATCH_LOD_COUNT, PATCH_SIZE_IN_METERS};
use super::patch::PatchKey;

pub struct MissingPatch {
    pub patch: PatchKey,
    pub coverage_required: bool,
}

pub struct PatchSelection {
    pub renderable: Vec<PatchKey>,
    pub missing: Vec<MissingPatch>,
    pub retained: HashSet<PatchKey>, // Needed but not currently rendered
}

impl PatchSelection {
    fn push_missing(&mut self, patch: PatchKey, coverage_required: bool) {
        self.missing.push(MissingPatch {
            patch,
            coverage_required,
        });
    }
}

pub struct QuadTree {
    nodes: Vec<TreeNode>,
}

impl QuadTree {
    pub fn new() -> Self {
        let mut nodes = vec![
            TreeNode {
                patch: PatchKey::new(0, 0, 0),
                height_range: Vec2::new(0.0, 1.0),
            };
            QuadTreeLayout::NODE_COUNT
        ];

        for lod in 0..PATCH_LOD_COUNT {
            let per_side = QuadTreeLayout::side_count(lod as usize) as u32;

            for z in 0..per_side {
                for x in 0..per_side {
                    let patch = PatchKey::new(x, z, lod);

                    nodes[QuadTreeLayout::node_index(&patch)].patch = patch;
                }
            }
        }

        Self { nodes }
    }

    pub fn select(&self, camera_pos: Vec3, height_scale: f32, lod_factor: f32, cache: &Cache) -> PatchSelection {
        let params = SelectParams {
            camera_pos,
            height_scale,
            lod_factor,
            cache,
        };

        let mut selection = PatchSelection {
            renderable: Vec::new(),
            missing: Vec::new(),
            retained: HashSet::new(),
        };

        self.select_recursive(0, &params, &mut selection);

        selection
    }

    pub fn split_distance(lod: u32, lod_factor: f32) -> f32 {
        debug_assert_ne!(lod, 0);
        (PATCH_SIZE_IN_METERS * (1 << lod)) as f32 * 0.5 * lod_factor
    }

    pub fn set_height_range(&mut self, patch: &PatchKey, height_range: Vec2) {
        let node_index = QuadTreeLayout::node_index(patch);
        self.nodes[node_index].height_range = height_range;
    }

    fn should_split(node: &TreeNode, params: &SelectParams) -> bool {
        if node.patch.lod == 0 {
            return false;
        }

        let origin = node.patch.world_origin();
        let size = node.patch.size_in_meters() as f32;

        let min = Vec3::new(origin.x, node.height_range.x * params.height_scale, origin.y);
        let max = Vec3::new(
            origin.x + size,
            node.height_range.y * params.height_scale,
            origin.y + size,
        );

        let closest_point = params.camera_pos.clamp(min, max);
        let split_distance = Self::split_distance(node.patch.lod, params.lod_factor);

        params.camera_pos.distance_squared(closest_point) < split_distance * split_distance
    }

    fn select_recursive(&self, node_index: usize, params: &SelectParams, selection: &mut PatchSelection) {
        let node = &self.nodes[node_index];
        let cache = params.cache;

        if Self::should_split(node, params) {
            let first_child = QuadTreeLayout::first_child_index(node_index, node.patch.lod as usize);

            let all_children_resident = (first_child..first_child + 4)
                .all(|i| cache.availability(&self.nodes[i].patch) == PatchAvailability::Resident);

            if all_children_resident {
                for child_index in first_child..first_child + 4 {
                    self.select_recursive(child_index, params, selection);
                }

                return;
            }

            for child_index in first_child..first_child + 4 {
                let child_patch = self.nodes[child_index].patch;

                match cache.availability(&child_patch) {
                    PatchAvailability::Missing => {
                        selection.push_missing(child_patch, false);
                    }
                    PatchAvailability::Pending | PatchAvailability::Resident => {
                        selection.retained.insert(child_patch);
                    }
                }
            }
        }

        match cache.availability(&node.patch) {
            PatchAvailability::Missing => {
                selection.push_missing(node.patch, true);
            }
            PatchAvailability::Pending => {
                selection.retained.insert(node.patch);
            }
            PatchAvailability::Resident => {
                selection.renderable.push(node.patch);
            }
        }
    }
}

#[derive(Clone, Copy)]
struct TreeNode {
    patch: PatchKey,
    height_range: Vec2,
}

struct SelectParams<'a> {
    camera_pos: Vec3,
    height_scale: f32,
    lod_factor: f32,
    cache: &'a Cache,
}
struct QuadTreeLayout;

impl QuadTreeLayout {
    const OFFSETS: [usize; PATCH_LOD_COUNT as usize] = Self::compute_offsets();
    const NODE_COUNT: usize = Self::OFFSETS[0] + Self::side_count(0).pow(2);

    /// Nodes are laid out coarsest LOD first, so a parent always precedes its children.
    const fn compute_offsets() -> [usize; PATCH_LOD_COUNT as usize] {
        let mut offsets = [0; PATCH_LOD_COUNT as usize];
        let mut lod = PATCH_LOD_COUNT as usize;
        let mut acc = 0;

        while lod > 0 {
            lod -= 1;
            offsets[lod] = acc;
            acc += Self::side_count(lod).pow(2);
        }

        offsets
    }

    const fn side_count(lod: usize) -> usize {
        PATCH_COUNT_PER_SIDE as usize >> lod
    }

    fn node_index(patch: &PatchKey) -> usize {
        Self::OFFSETS[patch.lod as usize] + Self::morton2(patch.x, patch.z) as usize
    }

    /// Morton ordering puts a node's four children contiguously at 4x its local index.
    fn first_child_index(node_index: usize, lod: usize) -> usize {
        debug_assert!(lod > 0);
        Self::OFFSETS[lod - 1] + (node_index - Self::OFFSETS[lod]) * 4
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
