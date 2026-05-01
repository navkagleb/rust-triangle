use glam::{Vec2, Vec3, f32};

const MIN_NODE_SIZE: f32 = 1.0;
const LOD_FACTOR: f32 = 8.0;
const CHUNK_GRID_SIZE: usize = 8;

pub struct QuadTreeNode {
    center: Vec2,
    half_size: f32,
    lod_level: u32,
    children: Option<Box<[QuadTreeNode; 4]>>,
}

impl QuadTreeNode {
    fn new_leaf(center: Vec2, half_size: f32, lod_level: u32) -> Self {
        Self {
            center,
            half_size,
            lod_level,
            children: None,
        }
    }

    pub fn center(&self) -> Vec2 {
        self.center
    }

    pub fn half_size(&self) -> f32 {
        self.half_size
    }

    pub fn lod_level(&self) -> u32 {
        self.lod_level
    }
}

pub struct QuadTree {
    root: QuadTreeNode,
}

impl QuadTree {
    pub fn new(size: f32, camera_position: &Vec3) -> Self {
        let half_size = size / 2.0;
        let mut root = QuadTreeNode {
            center: Vec2::new(half_size, half_size),
            half_size,
            lod_level: 0,
            children: None,
        };

        Self::split_recursive(&mut root, camera_position);

        Self { root }
    }

    pub fn collect_leafs(&self) -> Vec<&QuadTreeNode> {
        let mut leafs = Vec::new();
        Self::traverse_node(&self.root, &mut leafs);

        leafs
    }

    fn is_split_needed(node: &QuadTreeNode, camera_position: &Vec3) -> bool {
        if node.half_size <= MIN_NODE_SIZE {
            return false;
        }

        let distance = (camera_position - Vec3::new(node.center.x, 0.0, node.center.y)).length();
        distance < node.half_size * LOD_FACTOR
    }

    fn split_recursive(node: &mut QuadTreeNode, camera_position: &Vec3) {
        if !Self::is_split_needed(node, camera_position) {
            return;
        }

        let child_size = node.half_size / 2.0;
        let child_lod_level = node.lod_level + 1;

        node.children = Some(Box::new([
            QuadTreeNode::new_leaf(
                Vec2::new(node.center.x - child_size, node.center.y - child_size),
                child_size,
                child_lod_level,
            ),
            QuadTreeNode::new_leaf(
                Vec2::new(node.center.x + child_size, node.center.y - child_size),
                child_size,
                child_lod_level,
            ),
            QuadTreeNode::new_leaf(
                Vec2::new(node.center.x + child_size, node.center.y + child_size),
                child_size,
                child_lod_level,
            ),
            QuadTreeNode::new_leaf(
                Vec2::new(node.center.x - child_size, node.center.y + child_size),
                child_size,
                child_lod_level,
            ),
        ]));

        for child in node.children.as_mut().unwrap().iter_mut() {
            Self::split_recursive(child, camera_position);
        }
    }

    fn traverse_node<'a>(node: &'a QuadTreeNode, leafs: &mut Vec<&'a QuadTreeNode>) {
        if node.children.is_none() {
            leafs.push(node);
            return;
        }

        for child in node.children.as_ref().unwrap().iter() {
            Self::traverse_node(child, leafs);
        }
    }
}

pub fn generate_chunk_indices() -> Vec<u32> {
    let mut indices = Vec::new();

    for z in 0..CHUNK_GRID_SIZE {
        for x in 0..CHUNK_GRID_SIZE {
            let tl = (z * (CHUNK_GRID_SIZE + 1) + x) as u32;
            let tr = tl + 1;
            let bl = tl + (CHUNK_GRID_SIZE + 1) as u32;
            let br = bl + 1;

            indices.push(tl);
            indices.push(tr);
            indices.push(bl);

            indices.push(tr);
            indices.push(br);
            indices.push(bl);
        }
    }

    indices
}
