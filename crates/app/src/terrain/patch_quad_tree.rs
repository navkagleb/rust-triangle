use glam::{IVec2, Vec2};

use super::config::{PATCH_LOD_COUNT, PATCH_TERRAIN_SIZE};
use super::patch::PatchKey;

#[derive(Clone)]
pub(super) struct PatchQuadNode {
    pub(super) key: PatchKey,
    pub(super) children: Option<Box<[PatchQuadNode; 4]>>,
}

impl PatchQuadNode {
    fn new(grid_index: IVec2, lod_index: u32) -> Self {
        Self {
            key: PatchKey { grid_index, lod_index },
            children: None,
        }
    }
}

pub(super) struct PatchQuadTree {
    pub(super) root: PatchQuadNode,
    pub(super) min_grid_index: IVec2,
    pub(super) max_grid_index: IVec2,
}

pub(super) struct PatchQuadTreeBuilder {
    camera_pos: Vec2,
    render_distance: u32,
    lod_factor: f32,
}

impl PatchQuadTreeBuilder {
    pub(super) fn new(camera_pos: Vec2, render_distance: u32, lod_factor: f32) -> Self {
        Self {
            camera_pos,
            render_distance,
            lod_factor,
        }
    }

    pub(super) fn build(self) -> PatchQuadTree {
        let mut root = self.root_node();
        self.split_recursive(&mut root);

        let grid_render_size = (self.render_distance * 2 / PATCH_TERRAIN_SIZE) as i32;

        PatchQuadTree {
            min_grid_index: root.key.grid_index,
            max_grid_index: root.key.grid_index + grid_render_size,
            root,
        }
    }

    fn root_node(&self) -> PatchQuadNode {
        let root_terrain_size = PATCH_TERRAIN_SIZE * 2_u32.pow(PATCH_LOD_COUNT - 1);
        let root_lod_index = (self.render_distance * 2 / PATCH_TERRAIN_SIZE).ilog2();

        let snapped_camera_terrain_pos =
            (self.camera_pos / root_terrain_size as f32).round().as_ivec2() * root_terrain_size as i32;

        let root_grid_index = (snapped_camera_terrain_pos / PATCH_TERRAIN_SIZE as i32)
            - (self.render_distance / PATCH_TERRAIN_SIZE) as i32;

        PatchQuadNode::new(root_grid_index, root_lod_index)
    }

    fn split_recursive(&self, node: &mut PatchQuadNode) {
        let distance = (self.camera_pos - node.key.terrain_center().as_vec2()).length();
        let split_by_distance = distance >= (node.key.terrain_size() as f32 * 0.5 * self.lod_factor);
        if split_by_distance && node.key.lod_index <= (PATCH_LOD_COUNT - 1) {
            return;
        }

        let child_lod_index = node.key.lod_index - 1;
        let child_offset = 1 << child_lod_index;

        node.children = Some(Box::new([
            PatchQuadNode::new(node.key.grid_index + IVec2::ZERO * child_offset, child_lod_index),
            PatchQuadNode::new(node.key.grid_index + IVec2::X * child_offset, child_lod_index),
            PatchQuadNode::new(node.key.grid_index + IVec2::Y * child_offset, child_lod_index),
            PatchQuadNode::new(node.key.grid_index + IVec2::ONE * child_offset, child_lod_index),
        ]));

        if child_lod_index == 0 {
            return;
        }

        for child in node.children.as_mut().unwrap().iter_mut() {
            self.split_recursive(child);
        }
    }
}
