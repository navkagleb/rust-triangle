use std::collections::HashSet;

use glam::{IVec2, Vec2};

use super::config::{PATCH_LOD_COUNT, PATCH_TERRAIN_SIZE};
use super::patch::PatchKey;
use super::patch_cache::{PatchAvailability, PatchCache};

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

pub struct PatchQuadTree {
    root: TreeNode,
}

impl PatchQuadTree {
    pub fn build(camera_pos: Vec2, render_distance: u32, lod_factor: f32) -> Self {
        let mut root = Self::create_root(camera_pos, render_distance);
        Self::split_recursive(&mut root, camera_pos, lod_factor);

        Self { root }
    }

    pub fn select(&self, cache: &PatchCache) -> PatchSelection {
        let mut selection = PatchSelection {
            renderable: Vec::new(),
            missing: Vec::new(),
            retained: HashSet::new(),
        };

        Self::select_node_recursive(&self.root, cache, &mut selection);

        selection
    }

    fn create_root(camera_pos: Vec2, render_distance: u32) -> TreeNode {
        let root_terrain_size = PATCH_TERRAIN_SIZE * 2_u32.pow(PATCH_LOD_COUNT - 1);
        let root_lod_index = (render_distance * 2 / PATCH_TERRAIN_SIZE).ilog2();

        let snapped_camera_terrain_pos =
            (camera_pos / root_terrain_size as f32).round().as_ivec2() * root_terrain_size as i32;

        let root_grid_index =
            (snapped_camera_terrain_pos / PATCH_TERRAIN_SIZE as i32) - (render_distance / PATCH_TERRAIN_SIZE) as i32;

        TreeNode::new(root_grid_index, root_lod_index)
    }

    fn split_recursive(node: &mut TreeNode, camera_pos: Vec2, lod_factor: f32) {
        if !Self::should_split(node, camera_pos, lod_factor) {
            return;
        }

        let child_lod_index = node.patch.lod_index - 1;
        let child_offset = 1 << child_lod_index;

        node.children = Some(Box::new([
            TreeNode::new(node.patch.grid_index + IVec2::ZERO * child_offset, child_lod_index),
            TreeNode::new(node.patch.grid_index + IVec2::X * child_offset, child_lod_index),
            TreeNode::new(node.patch.grid_index + IVec2::Y * child_offset, child_lod_index),
            TreeNode::new(node.patch.grid_index + IVec2::ONE * child_offset, child_lod_index),
        ]));

        if child_lod_index == 0 {
            return;
        }

        for child in node.children.as_mut().unwrap().iter_mut() {
            Self::split_recursive(child, camera_pos, lod_factor);
        }
    }

    fn should_split(node: &mut TreeNode, camera_pos: Vec2, lod_factor: f32) -> bool {
        if node.patch.lod_index == 0 {
            return false;
        }

        let distance = camera_pos.distance(node.patch.terrain_center().as_vec2());
        let split_distance = node.patch.terrain_size() as f32 * 0.5 * lod_factor;

        let is_virtual_root = node.patch.lod_index >= PATCH_LOD_COUNT;

        is_virtual_root || distance < split_distance
    }

    fn select_node_recursive(node: &TreeNode, cache: &PatchCache, selection: &mut PatchSelection) {
        let is_renderable = node.patch.lod_index < PATCH_LOD_COUNT;

        if let Some(children) = node.children.as_deref() {
            let all_children_resident = children
                .iter()
                .all(|c| cache.availability(&c.patch) == PatchAvailability::Resident);

            if !is_renderable || all_children_resident {
                for child in children {
                    Self::select_node_recursive(child, cache, selection);
                }

                return;
            }

            for child in children {
                match cache.availability(&child.patch) {
                    PatchAvailability::Missing => {
                        selection.push_missing(child.patch, false);
                    }
                    PatchAvailability::Pending | PatchAvailability::Resident => {
                        selection.retained.insert(child.patch);
                    }
                }
            }
        }

        if !is_renderable {
            return;
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

struct TreeNode {
    patch: PatchKey,
    children: Option<Box<[TreeNode; 4]>>,
}

impl TreeNode {
    fn new(grid_index: IVec2, lod_index: u32) -> Self {
        Self {
            patch: PatchKey { grid_index, lod_index },
            children: None,
        }
    }
}
