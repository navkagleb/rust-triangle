use glam::{IVec2, Vec2};

use super::config::{PATCH_LOD_COUNT, PATCH_TERRAIN_SIZE};
use super::patch::PatchKey;

pub struct MissingPatch {
    pub patch: PatchKey,
    pub coverage_required: bool,
}

pub struct PatchSelection {
    pub renderable: Vec<PatchKey>,
    pub missing: Vec<MissingPatch>,
}

pub struct PatchQuadTree {
    root: PatchQuadNode,
    min_grid_index: IVec2,
    max_grid_index: IVec2,
}

impl PatchQuadTree {
    pub fn build(camera_pos: Vec2, render_distance: u32, lod_factor: f32) -> Self {
        let mut root = Self::create_root(camera_pos, render_distance);

        Self::split_recursive(&mut root, camera_pos, lod_factor);

        let grid_size = (render_distance * 2 / PATCH_TERRAIN_SIZE) as i32;

        Self {
            min_grid_index: root.patch.grid_index,
            max_grid_index: root.patch.grid_index + grid_size,
            root,
        }
    }

    pub fn select<R, M>(&self, is_resident: R, is_missing: M) -> PatchSelection
    where
        R: Fn(PatchKey) -> bool,
        M: Fn(PatchKey) -> bool,
    {
        let mut selection = PatchSelection {
            renderable: Vec::new(),
            missing: Vec::new(),
        };

        Self::select_node_recursive(&self.root, &is_resident, &is_missing, &mut selection);

        selection
    }

    pub fn contains_grid_index(&self, grid_index: IVec2) -> bool {
        grid_index.cmpge(self.min_grid_index).all() && grid_index.cmple(self.max_grid_index).all()
    }

    fn create_root(camera_pos: Vec2, render_distance: u32) -> PatchQuadNode {
        let root_terrain_size = PATCH_TERRAIN_SIZE * 2_u32.pow(PATCH_LOD_COUNT - 1);
        let root_lod_index = (render_distance * 2 / PATCH_TERRAIN_SIZE).ilog2();

        let snapped_camera_terrain_pos =
            (camera_pos / root_terrain_size as f32).round().as_ivec2() * root_terrain_size as i32;

        let root_grid_index =
            (snapped_camera_terrain_pos / PATCH_TERRAIN_SIZE as i32) - (render_distance / PATCH_TERRAIN_SIZE) as i32;

        PatchQuadNode::new(root_grid_index, root_lod_index)
    }

    fn split_recursive(node: &mut PatchQuadNode, camera_pos: Vec2, lod_factor: f32) {
        if !Self::should_split(node, camera_pos, lod_factor) {
            return;
        }

        let child_lod_index = node.patch.lod_index - 1;
        let child_offset = 1 << child_lod_index;

        node.children = Some(Box::new([
            PatchQuadNode::new(node.patch.grid_index + IVec2::ZERO * child_offset, child_lod_index),
            PatchQuadNode::new(node.patch.grid_index + IVec2::X * child_offset, child_lod_index),
            PatchQuadNode::new(node.patch.grid_index + IVec2::Y * child_offset, child_lod_index),
            PatchQuadNode::new(node.patch.grid_index + IVec2::ONE * child_offset, child_lod_index),
        ]));

        if child_lod_index == 0 {
            return;
        }

        for child in node.children.as_mut().unwrap().iter_mut() {
            Self::split_recursive(child, camera_pos, lod_factor);
        }
    }

    fn should_split(node: &mut PatchQuadNode, camera_pos: Vec2, lod_factor: f32) -> bool {
        if node.patch.lod_index == 0 {
            return false;
        }

        let distance = camera_pos.distance(node.patch.terrain_center().as_vec2());
        let split_distance = node.patch.terrain_size() as f32 * 0.5 * lod_factor;

        let is_virtual_root = node.patch.lod_index >= PATCH_LOD_COUNT;

        is_virtual_root || distance < split_distance
    }

    fn select_node_recursive<R, M>(
        node: &PatchQuadNode,
        is_resident: &R,
        is_missing: &M,
        selection: &mut PatchSelection,
    ) where
        R: Fn(PatchKey) -> bool,
        M: Fn(PatchKey) -> bool,
    {
        let is_renderable = node.patch.lod_index < PATCH_LOD_COUNT;

        if let Some(children) = node.children.as_deref() {
            let all_children_resident = children.iter().all(|c| is_resident(c.patch));

            if !is_renderable || all_children_resident {
                for child in children {
                    Self::select_node_recursive(child, is_resident, is_missing, selection);
                }

                return;
            }

            // The parent remains as fallback while finer patches load
            for child in children {
                if is_missing(child.patch) {
                    selection.missing.push(MissingPatch {
                        patch: child.patch,
                        coverage_required: false,
                    });
                }
            }
        }

        if !is_renderable {
            return;
        }

        if is_resident(node.patch) {
            selection.renderable.push(node.patch);
        } else if is_missing(node.patch) {
            selection.missing.push(MissingPatch {
                patch: node.patch,
                coverage_required: true,
            });
        }
    }
}

struct PatchQuadNode {
    patch: PatchKey,
    children: Option<Box<[PatchQuadNode; 4]>>,
}

impl PatchQuadNode {
    fn new(grid_index: IVec2, lod_index: u32) -> Self {
        Self {
            patch: PatchKey { grid_index, lod_index },
            children: None,
        }
    }
}
