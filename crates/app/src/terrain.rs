use anyhow::Result;
use glam::{Vec2, Vec3, f32};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::d3d12_utils::{ConstBuffer, D3D12BufferExt};

const CHUNK_QUAD_COUNT: usize = 8;

#[repr(C)]
pub struct GpuTerrainNode {
    pub center: Vec2,
    pub half_size: f32,
    pub lod_index: u32,
}

#[repr(C)]
pub struct GpuTerrainConsts {
    pub terrain_size: f32,
    pub world_scale: f32,
    pub height_scale: f32,
}

pub struct TerrainDataUi {
    pub height_map_size: i32,
    pub height_map_scale: f32,
}

pub struct TerrainData {
    pub size: f32,
    pub height_map_size: f32,
    pub lod_factor: f32,
    pub height_scale: f32,

    pub chunk_ibv: D3D12_INDEX_BUFFER_VIEW,
    pub chunk_index_count: usize,

    pub const_buffer: ConstBuffer<GpuTerrainConsts>,
    pub node_buffer: ID3D12Resource,
    pub height_map_texture: Option<ID3D12Resource>,
    _chunk_index_buffer: ID3D12Resource,
}

impl TerrainData {
    pub fn new(device: &ID3D12Device, node_buffer_cpu_srv: D3D12_CPU_DESCRIPTOR_HANDLE) -> Result<Self> {
        let chunk_indices = generate_chunk_indices();
        let chunk_index_buffer =
            ID3D12Resource::new_buffer(device, D3D12_HEAP_TYPE_UPLOAD, size_of_val(chunk_indices.as_slice()))?;

        chunk_index_buffer.map_and_write(chunk_indices.as_slice())?;

        let node_size = size_of::<GpuTerrainNode>();
        let node_count = 1024;
        let node_buffer = ID3D12Resource::new_buffer(device, D3D12_HEAP_TYPE_UPLOAD, node_count * node_size)?;

        unsafe {
            device.CreateShaderResourceView(
                &node_buffer,
                Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: DXGI_FORMAT_UNKNOWN,
                    ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Buffer: D3D12_BUFFER_SRV {
                            FirstElement: 0,
                            NumElements: node_count as u32,
                            StructureByteStride: node_size as u32,
                            Flags: D3D12_BUFFER_SRV_FLAG_NONE,
                        },
                    },
                }),
                node_buffer_cpu_srv,
            );
        }

        let size = 256.0;

        Ok(Self {
            size,
            height_map_size: size * 2.0,
            lod_factor: 5.0,
            height_scale: 15.0,
            const_buffer: ConstBuffer::new(device)?,
            chunk_ibv: D3D12_INDEX_BUFFER_VIEW {
                BufferLocation: unsafe { chunk_index_buffer.GetGPUVirtualAddress() },
                SizeInBytes: size_of_val(chunk_indices.as_slice()) as u32,
                Format: DXGI_FORMAT_R32_UINT,
            },
            chunk_index_count: chunk_indices.len(),
            node_buffer,
            height_map_texture: None,
            _chunk_index_buffer: chunk_index_buffer,
        })
    }

    pub fn collect_nodes(&self, camera_position: &Vec3) -> Vec<GpuTerrainNode> {
        let min_node_size = (self.size / self.height_map_size) * CHUNK_QUAD_COUNT as f32;
        let qtree = QuadTree::new(self.size, min_node_size, self.lod_factor, camera_position);

        qtree
            .collect_leafs()
            .iter()
            .map(|n| GpuTerrainNode {
                center: n.center,
                half_size: n.half_size,
                lod_index: n.lod_index,
            })
            .collect()
    }
}

struct QuadTreeNode {
    center: Vec2,
    half_size: f32,
    lod_index: u32,
    children: Option<Box<[QuadTreeNode; 4]>>,
}

impl QuadTreeNode {
    fn new(center: Vec2, half_size: f32, lod_index: u32) -> Self {
        Self {
            center,
            half_size,
            lod_index,
            children: None,
        }
    }
}

struct QuadTree {
    root: QuadTreeNode,
}

struct QuadSplitParams<'a> {
    camera_position: &'a Vec3,
    min_node_size: f32,
    lod_factor: f32,
}

impl QuadTree {
    fn new(size: f32, min_node_size: f32, lod_factor: f32, camera_position: &Vec3) -> Self {
        let half_size = size / 2.0;
        let max_depth = (size / CHUNK_QUAD_COUNT as f32).log2() as u32;
        let mut root = QuadTreeNode::new(Vec2::new(half_size, half_size), half_size, max_depth);

        Self::split_recursive(
            &mut root,
            &QuadSplitParams {
                camera_position,
                min_node_size,
                lod_factor,
            },
        );

        Self { root }
    }

    fn collect_leafs(&self) -> Vec<&QuadTreeNode> {
        let mut leafs = Vec::new();
        Self::traverse_node(&self.root, &mut leafs);

        leafs
    }

    fn split_recursive(node: &mut QuadTreeNode, params: &QuadSplitParams) {
        if node.half_size <= params.min_node_size {
            return;
        }

        let distance = (params.camera_position - Vec3::new(node.center.x, 0.0, node.center.y)).length();
        if distance >= node.half_size * params.lod_factor {
            return;
        }

        let child_size = node.half_size / 2.0;
        let child_lod_level = node.lod_index - 1;

        node.children = Some(Box::new([
            QuadTreeNode::new(
                Vec2::new(node.center.x - child_size, node.center.y - child_size),
                child_size,
                child_lod_level,
            ),
            QuadTreeNode::new(
                Vec2::new(node.center.x + child_size, node.center.y - child_size),
                child_size,
                child_lod_level,
            ),
            QuadTreeNode::new(
                Vec2::new(node.center.x + child_size, node.center.y + child_size),
                child_size,
                child_lod_level,
            ),
            QuadTreeNode::new(
                Vec2::new(node.center.x - child_size, node.center.y + child_size),
                child_size,
                child_lod_level,
            ),
        ]));

        for child in node.children.as_mut().unwrap().iter_mut() {
            Self::split_recursive(child, params);
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

fn generate_chunk_indices() -> Vec<u32> {
    let mut indices = Vec::new();

    for z in 0..CHUNK_QUAD_COUNT {
        for x in 0..CHUNK_QUAD_COUNT {
            let tl = (z * (CHUNK_QUAD_COUNT + 1) + x) as u32;
            let tr = tl + 1;
            let bl = tl + (CHUNK_QUAD_COUNT + 1) as u32;
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

pub fn generate_mips(data: Vec<f32>, size: usize) -> Vec<Vec<f32>> {
    let mut mips = vec![data];
    let mut current_size = size;

    while current_size > 1 {
        current_size /= 2;

        let prev = mips.last().unwrap();
        let mip: Vec<f32> = (0..current_size * current_size)
            .map(|i| {
                let x = (i % current_size) * 2;
                let y = (i / current_size) * 2;
                let prev_size = current_size * 2;

                let s00 = prev[y * prev_size + x];
                let s10 = prev[y * prev_size + x + 1];
                let s01 = prev[(y + 1) * prev_size + x];
                let s11 = prev[(y + 1) * prev_size + x + 1];

                (s00 + s10 + s01 + s11) / 4.0
            })
            .collect();

        mips.push(mip);
    }

    mips
}
