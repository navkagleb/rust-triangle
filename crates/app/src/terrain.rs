use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use bitflags::bitflags;
use glam::{IVec2, Mat4, Vec2, Vec3, f32};
use noise::utils::{NoiseMapBuilder, PlaneMapBuilder};
use noise::{Fbm, MultiFractal, Perlin};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::d3d12_utils::*;
use crate::terrain::TileState::Uploading;
use crate::{BACK_BUFFER_FORMAT, DEPTH_BUFFER_FORMAT, GpuResource};

pub const RENDER_DISTANCE: f32 = 1024.0;
pub const TILE_PIXEL_SIZE: f32 = 128.0;
pub const TILE_PIXEL_TO_WORLD_SCALE: f32 = 0.5;
pub const TILE_WORLD_SIZE: f32 = TILE_PIXEL_SIZE * TILE_PIXEL_TO_WORLD_SCALE;
pub const MAX_TILE_COUNT: u32 = (RENDER_DISTANCE * 2.0 / TILE_WORLD_SIZE) as u32;
pub const ATLAS_MAP_SIZE: u32 = (RENDER_DISTANCE * 2.0 / TILE_PIXEL_TO_WORLD_SCALE) as u32;

const CELL_QUAD_COUNT: usize = 8;
const TILE_GENERATOR_WORKER_COUNT: usize = 16;

bitflags! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug)]
    pub struct StitchMask: u32 {
        const TOP = 1 << 0;
        const BOTTOM = 1 << 1;
        const LEFT = 1 << 2;
        const RIGHT = 1 << 3;
        const TOP_LEFT = 1 << 4;
        const TOP_RIGHT = 1 << 5;
        const BOTTOM_LEFT = 1 << 6;
        const BOTTOM_RIGHT = 1 << 7;
    }
}

#[repr(C)]
pub struct GpuTerrainNode {
    pub center: Vec2,
    pub half_size: f32,
    pub lod_index: u32,
    pub stitch_mask: StitchMask,
}

#[repr(C)]
pub struct GpuTerrainConsts {
    pub world_to_clip: Mat4,
    pub world_center_tile: Vec2,
    pub world_scale: f32,
    pub height_scale: f32,
    pub wireframe_pass: u32,
    pub stitching_enabled: u32,
}

pub struct TerrainData {
    pub lod_factor: f32,
    pub height_scale: f32,
    pub world_scale: f32,

    pub cell_ibv: D3D12_INDEX_BUFFER_VIEW,
    pub cell_index_count: usize,

    pub solid_const_buffer: ConstBuffer<GpuTerrainConsts>,
    pub wireframe_const_buffer: ConstBuffer<GpuTerrainConsts>,

    node_buffer: ID3D12Resource,
    _cell_index_buffer: ID3D12Resource,

    height_atlas: ID3D12Resource,
    height_upload_buffer: ID3D12Resource,
    height_mapped_ptr: *mut u8,

    pub solid_vertex_pso: ID3D12PipelineState,
    pub solid_mesh_pso: ID3D12PipelineState,
    pub wireframe_vertex_pso: ID3D12PipelineState,
    pub wireframe_mesh_pso: ID3D12PipelineState,

    tile_cache: HashMap<IVec2, TileState>,
    tile_gen_pool: TileGenPool,

    pub world_center_tile: IVec2,

    pub requested_count: u32,
    pub generated_count: u32,
    pub uploading_count: u32,
    pub resident_count: u32,
}

impl TerrainData {
    pub fn new(
        device: &ID3D12Device4,
        resource_heap: &ID3D12DescriptorHeap,
        root_signature: &ID3D12RootSignature,
    ) -> Result<Self> {
        let cell_indices = {
            let mut indices = Vec::with_capacity(CELL_QUAD_COUNT * CELL_QUAD_COUNT * 6);

            for z in 0..CELL_QUAD_COUNT {
                for x in 0..CELL_QUAD_COUNT {
                    let tl = (z * (CELL_QUAD_COUNT + 1) + x) as u32;
                    let tr = tl + 1;
                    let bl = tl + (CELL_QUAD_COUNT + 1) as u32;
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
        };
        let cell_index_buffer =
            ID3D12Resource::new_buffer(device, D3D12_HEAP_TYPE_UPLOAD, size_of_val(cell_indices.as_slice()))?;

        cell_index_buffer.map_and_write(cell_indices.as_slice())?;

        let max_node_count = (RENDER_DISTANCE * 2.0 / TILE_WORLD_SIZE).powi(2) as usize;
        let node_buffer = ID3D12Resource::new_buffer(
            device,
            D3D12_HEAP_TYPE_UPLOAD,
            max_node_count * size_of::<GpuTerrainNode>(),
        )?;

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
                            NumElements: max_node_count as u32,
                            StructureByteStride: size_of::<GpuTerrainNode>() as u32,
                            Flags: D3D12_BUFFER_SRV_FLAG_NONE,
                        },
                    },
                }),
                resource_heap.get_cpu_handle(device, GpuResource::TerrainNodeBuffer as u32),
            );

            device.CreateShaderResourceView(
                &cell_index_buffer,
                Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: DXGI_FORMAT_R32_UINT,
                    ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Buffer: D3D12_BUFFER_SRV {
                            FirstElement: 0,
                            NumElements: cell_indices.len() as u32,
                            StructureByteStride: 0,
                            Flags: D3D12_BUFFER_SRV_FLAG_NONE,
                        },
                    },
                }),
                resource_heap.get_cpu_handle(device, GpuResource::TerrainCellIndexBuffer as u32),
            );
        }

        let vs_blob = std::fs::read(std::path::Path::new("target/dxil/terrain.vs.dxil"))?;
        let ms_blob = std::fs::read(std::path::Path::new("target/dxil/terrain.ms.dxil"))?;
        let ps_blob = std::fs::read(std::path::Path::new("target/dxil/terrain.ps.dxil"))?;

        let depth_stencil_state = D3D12_DEPTH_STENCIL_DESC {
            DepthEnable: true.into(),
            DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ALL,
            DepthFunc: D3D12_COMPARISON_FUNC_GREATER,
            ..Default::default()
        };

        let rtv_fmts = {
            let mut fmts = [DXGI_FORMAT_UNKNOWN; 8];
            fmts[0] = BACK_BUFFER_FORMAT;
            fmts
        };

        let create_rasterizer_state = |fill_mode: D3D12_FILL_MODE| -> D3D12_RASTERIZER_DESC {
            let mut state = D3D12_RASTERIZER_DESC {
                FillMode: fill_mode,
                CullMode: D3D12_CULL_MODE_NONE,
                FrontCounterClockwise: false.into(),
                ..Default::default()
            };

            if fill_mode == D3D12_FILL_MODE_WIREFRAME {
                state.DepthBias = 1000;
                state.SlopeScaledDepthBias = 1.0;
            }

            state
        };

        let create_vertex_pso =
            |rasterizer_state: D3D12_RASTERIZER_DESC| -> windows::core::Result<ID3D12PipelineState> {
                unsafe {
                    device.CreateGraphicsPipelineState::<ID3D12PipelineState>(&D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                        pRootSignature: std::mem::ManuallyDrop::new(std::mem::transmute_copy(root_signature)),
                        VS: D3D12_SHADER_BYTECODE::from_slice(&vs_blob),
                        PS: D3D12_SHADER_BYTECODE::from_slice(&ps_blob),
                        BlendState: D3D12_BLEND_DESC {
                            RenderTarget: {
                                let mut render_targets = [D3D12_RENDER_TARGET_BLEND_DESC::default(); 8];
                                render_targets[0].RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8;
                                render_targets
                            },
                            ..Default::default()
                        },
                        SampleMask: u32::MAX,
                        RasterizerState: rasterizer_state,
                        DepthStencilState: depth_stencil_state,
                        PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                        NumRenderTargets: 1,
                        RTVFormats: rtv_fmts,
                        DSVFormat: DEPTH_BUFFER_FORMAT,
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        ..Default::default()
                    })
                }
            };

        let create_mesh_pso = |rasterizer_state: D3D12_RASTERIZER_DESC| -> windows::core::Result<ID3D12PipelineState> {
            let mut stream = MeshPipelineStream {
                root_signature: PsoSubobject::new(unsafe { std::mem::transmute_copy(root_signature) }),
                ms: PsoSubobject::new(D3D12_SHADER_BYTECODE::from_slice(&ms_blob)),
                ps: PsoSubobject::new(D3D12_SHADER_BYTECODE::from_slice(&ps_blob)),
                rasterizer: PsoSubobject::new(rasterizer_state),
                depth_stencil: PsoSubobject::new(depth_stencil_state),
                rtv_formats: PsoSubobject::new(D3D12_RT_FORMAT_ARRAY {
                    RTFormats: rtv_fmts,
                    NumRenderTargets: 1,
                }),
                dsv_format: PsoSubobject::new(DEPTH_BUFFER_FORMAT),
                sample_desc: PsoSubobject::new(DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }),
            };

            unsafe {
                device.CreatePipelineState::<ID3D12PipelineState>(&D3D12_PIPELINE_STATE_STREAM_DESC {
                    SizeInBytes: size_of::<MeshPipelineStream>(),
                    pPipelineStateSubobjectStream: &mut stream as *mut _ as *mut _,
                })
            }
        };

        let solid_state = create_rasterizer_state(D3D12_FILL_MODE_SOLID);
        let wireframe_state = create_rasterizer_state(D3D12_FILL_MODE_WIREFRAME);

        let get_texture_size = |texture: &ID3D12Resource| -> usize {
            let desc = unsafe { texture.GetDesc() };
            let mut size = 0;

            unsafe {
                device.GetCopyableFootprints(&desc, 0, desc.MipLevels as u32, 0, None, None, None, Some(&mut size));
            }

            size as usize
        };

        let height_format = DXGI_FORMAT_R32_FLOAT;
        let height_atlas = ID3D12Resource::new_texture(device, height_format, ATLAS_MAP_SIZE, ATLAS_MAP_SIZE, 1)?;
        let height_upload_buffer =
            ID3D12Resource::new_buffer(device, D3D12_HEAP_TYPE_UPLOAD, get_texture_size(&height_atlas))?;

        unsafe {
            device.CreateShaderResourceView(
                &height_atlas,
                Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: height_format,
                    ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2D: D3D12_TEX2D_SRV {
                            MostDetailedMip: 0,
                            MipLevels: 1,
                            PlaneSlice: 0,
                            ResourceMinLODClamp: 0.0,
                        },
                    },
                }),
                resource_heap.get_cpu_handle(device, GpuResource::TerrainHeightAtlas as u32),
            );
        }

        Ok(Self {
            lod_factor: 4.0,
            height_scale: 15.0,
            world_scale: 1.0,
            solid_const_buffer: ConstBuffer::new(device)?,
            wireframe_const_buffer: ConstBuffer::new(device)?,
            cell_ibv: D3D12_INDEX_BUFFER_VIEW {
                BufferLocation: unsafe { cell_index_buffer.GetGPUVirtualAddress() },
                SizeInBytes: size_of_val(cell_indices.as_slice()) as u32,
                Format: DXGI_FORMAT_R32_UINT,
            },
            cell_index_count: cell_indices.len(),
            node_buffer,
            _cell_index_buffer: cell_index_buffer,

            height_mapped_ptr: height_upload_buffer.map::<u8>()?,
            height_upload_buffer,
            height_atlas,

            solid_vertex_pso: create_vertex_pso(solid_state)?,
            wireframe_vertex_pso: create_vertex_pso(wireframe_state)?,
            solid_mesh_pso: create_mesh_pso(solid_state)?,
            wireframe_mesh_pso: create_mesh_pso(wireframe_state)?,

            tile_cache: HashMap::new(),
            tile_gen_pool: TileGenPool::new(),

            world_center_tile: IVec2::ZERO,
            requested_count: 0,
            generated_count: 0,
            uploading_count: 0,
            resident_count: 0,
        })
    }

    pub fn collect_nodes(&mut self, camera_position: &Vec3) -> Result<Vec<GpuTerrainNode>> {
        self.requested_count = 0;
        self.generated_count = 0;
        self.uploading_count = 0;
        self.resident_count = 0;

        for state in self.tile_cache.values() {
            match state {
                TileState::Requested => self.requested_count += 1,
                TileState::Generated(_) => self.generated_count += 1,
                TileState::Uploading(_) => self.uploading_count += 1,
                TileState::Resident => self.resident_count += 1,
            }
        }

        self.world_center_tile = IVec2::new(
            (camera_position.x / TILE_WORLD_SIZE).round() as i32,
            (camera_position.z / TILE_WORLD_SIZE).round() as i32,
        );

        let qtree = QuadTree::new(
            self.world_center_tile.as_vec2() * TILE_WORLD_SIZE,
            RENDER_DISTANCE,
            camera_position,
            self.lod_factor,
        );

        let leafs = qtree.collect_leafs();

        let mut missing_tiles: Vec<IVec2> = leafs
            .iter()
            .flat_map(|leaf| {
                let tile_count = (leaf.size() / TILE_WORLD_SIZE) as u32;

                assert!((leaf.center.x - leaf.half_size) as i32 % TILE_WORLD_SIZE as i32 == 0);
                assert!((leaf.center.y - leaf.half_size) as i32 % TILE_WORLD_SIZE as i32 == 0);

                let leaf_start_tile = IVec2::new(
                    ((leaf.center.x - leaf.half_size) / TILE_WORLD_SIZE) as i32,
                    ((leaf.center.y - leaf.half_size) / TILE_WORLD_SIZE) as i32,
                );

                (0..tile_count)
                    .flat_map(move |x| (0..tile_count).map(move |y| leaf_start_tile + IVec2::new(x as i32, y as i32)))
            })
            .filter(|world_tile| !self.tile_cache.contains_key(world_tile))
            .collect();

        missing_tiles.sort_unstable_by(|a, b| {
            let distance_a = (camera_position - Vec3::new(a.x as f32, 0.0, a.y as f32)).length_squared();
            let distance_b = (camera_position - Vec3::new(b.x as f32, 0.0, b.y as f32)).length_squared();

            distance_a.total_cmp(&distance_b)
        });

        for result in self.tile_gen_pool.drain_results() {
            self.tile_cache
                .insert(result.world_tile, TileState::Generated(result.height_map));
        }

        for tile in missing_tiles {
            self.tile_gen_pool.submit_task(tile);
            self.tile_cache.insert(tile, TileState::Requested);
        }

        let is_neighbor_coarser = |node: &QuadTreeNode, direction: Vec2| -> bool {
            let probe = node.center + direction * node.half_size * 2.0;
            let neighbor_lod_index = leafs
                .iter()
                .find(|n| (n.center - probe).length() < n.half_size)
                .map(|n| n.lod_index)
                .unwrap_or(node.lod_index);

            neighbor_lod_index > node.lod_index
        };

        let gpu_nodes = leafs
            .iter()
            .map(|n| {
                let directions = [
                    (StitchMask::TOP, Vec2::new(0.0, -1.0)),
                    (StitchMask::BOTTOM, Vec2::new(0.0, 1.0)),
                    (StitchMask::LEFT, Vec2::new(-1.0, 0.0)),
                    (StitchMask::RIGHT, Vec2::new(1.0, 0.0)),
                    (StitchMask::TOP_LEFT, Vec2::new(-1.0, -1.0)),
                    (StitchMask::TOP_RIGHT, Vec2::new(1.0, -1.0)),
                    (StitchMask::BOTTOM_LEFT, Vec2::new(-1.0, 1.0)),
                    (StitchMask::BOTTOM_RIGHT, Vec2::new(1.0, 1.0)),
                ];

                let mut stitch_mask = StitchMask::empty();

                for (flag, direction) in directions.iter() {
                    if is_neighbor_coarser(n, *direction) {
                        stitch_mask.insert(*flag);
                    }
                }

                GpuTerrainNode {
                    center: n.center,
                    half_size: n.half_size,
                    lod_index: n.lod_index,
                    stitch_mask,
                }
            })
            .collect::<Vec<_>>();

        // TODO: Should be used storage per frame"
        self.node_buffer.map_and_write(gpu_nodes.as_slice())?;

        Ok(gpu_nodes)
    }

    pub fn upload_tiles_to_gpu(
        &mut self,
        device: &ID3D12Device,
        cmd_list: &ID3D12GraphicsCommandList,
        cpu_frame_index: u64,
        gpu_frame_index: u64,
    ) -> Result<()> {
        let mut atlas_layout = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        unsafe {
            device.GetCopyableFootprints(
                &self.height_atlas.GetDesc(),
                0,
                1,
                0,
                Some(&mut atlas_layout),
                None,
                None,
                None,
            );
        }

        let atlas_center_tile = IVec2::splat((MAX_TILE_COUNT / 2) as i32);
        let mut updated_tiles = Vec::new();

        for (world_tile, state) in &self.tile_cache {
            if let Uploading(frame_index) = state
                && *frame_index <= gpu_frame_index
            {
                updated_tiles.push((*world_tile, TileState::Resident));
                continue;
            }

            let TileState::Generated(height_data) = state else {
                continue;
            };

            let atlas_tile = atlas_center_tile + world_tile - self.world_center_tile;

            if atlas_tile.x < 0
                || atlas_tile.y < 0
                || atlas_tile.x >= MAX_TILE_COUNT as i32
                || atlas_tile.y >= MAX_TILE_COUNT as i32
            {
                continue;
            }

            let atlas_row_pitch = atlas_layout.Footprint.RowPitch as u64;

            let tile_offset_bytes = atlas_tile.y as u64 * TILE_PIXEL_SIZE as u64 * atlas_row_pitch
                + atlas_tile.x as u64 * TILE_PIXEL_SIZE as u64 * size_of::<f32>() as u64;

            for row in 0..TILE_PIXEL_SIZE as u32 {
                let src_offset = row * TILE_PIXEL_SIZE as u32;
                let dst_offset = tile_offset_bytes as usize + row as usize * atlas_row_pitch as usize;

                unsafe {
                    std::ptr::copy_nonoverlapping(
                        height_data.as_ptr().add(src_offset as usize),
                        (self.height_mapped_ptr as *mut f32).byte_add(dst_offset),
                        TILE_PIXEL_SIZE as usize,
                    );
                }
            }

            unsafe {
                cmd_list.CopyTextureRegion(
                    &D3D12_TEXTURE_COPY_LOCATION {
                        pResource: std::mem::transmute_copy(&self.height_atlas),
                        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
                    },
                    atlas_tile.x as u32 * TILE_PIXEL_SIZE as u32,
                    atlas_tile.y as u32 * TILE_PIXEL_SIZE as u32,
                    0,
                    &D3D12_TEXTURE_COPY_LOCATION {
                        pResource: std::mem::transmute_copy(&self.height_upload_buffer),
                        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                            PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                                Offset: tile_offset_bytes,
                                Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                                    Format: DXGI_FORMAT_R32_FLOAT,
                                    Width: TILE_PIXEL_SIZE as u32,
                                    Height: TILE_PIXEL_SIZE as u32,
                                    Depth: 1,
                                    RowPitch: atlas_row_pitch as u32,
                                },
                            },
                        },
                    },
                    None,
                );
            }

            updated_tiles.push((*world_tile, TileState::Uploading(cpu_frame_index)));
        }

        for (world_tile, state) in updated_tiles {
            self.tile_cache.insert(world_tile, state);
        }

        Ok(())
    }
}

enum TileState {
    Requested,
    Generated(Vec<f32>),
    Uploading(u64),
    Resident,
}

struct TileGenResult {
    world_tile: IVec2,
    height_map: Vec<f32>,
}

type TileGenTask = Box<dyn FnOnce() + Send + 'static>;

struct TileGenPool {
    workers: Vec<std::thread::JoinHandle<()>>,
    task_sender: Option<Sender<TileGenTask>>,
    result_sender: Sender<TileGenResult>,
    result_receiver: Receiver<TileGenResult>,

    fbm: Arc<Fbm<Perlin>>,
}

impl Drop for TileGenPool {
    fn drop(&mut self) {
        drop(self.task_sender.take());

        for worker in self.workers.drain(..) {
            worker.join().unwrap();
        }
    }
}

impl TileGenPool {
    fn new() -> Self {
        let (task_sender, task_receiver) = std::sync::mpsc::channel::<TileGenTask>();
        let task_receiver = Arc::new(Mutex::new(task_receiver));

        let workers = (0..TILE_GENERATOR_WORKER_COUNT)
            .map(|i| {
                let task_receiver = Arc::clone(&task_receiver);

                std::thread::Builder::new()
                    .name(format!("tile-generator-{}", i))
                    .spawn(move || {
                        loop {
                            let task = task_receiver.lock().unwrap().recv();

                            match task {
                                Ok(task) => task(),
                                Err(_) => break,
                            };
                        }
                    })
                    .unwrap()
            })
            .collect();

        let (result_sender, result_receiver) = std::sync::mpsc::channel::<TileGenResult>();

        let fbm = Fbm::<Perlin>::new(123)
            .set_octaves(6)
            .set_frequency(1.0)
            .set_lacunarity(2.0)
            .set_persistence(0.5);

        Self {
            workers,
            task_sender: Some(task_sender),
            result_sender,
            result_receiver,
            fbm: Arc::new(fbm),
        }
    }

    fn submit_task(&self, world_tile: IVec2) {
        let fbm = Arc::clone(&self.fbm);
        let result_sender = self.result_sender.clone();

        let task = move || {
            let noise_scale = 4.0_f64;
            let world_scale = 2048.0_f64;

            let world_x = world_tile.x as f64 * TILE_WORLD_SIZE as f64;
            let world_y = world_tile.y as f64 * TILE_WORLD_SIZE as f64;

            let x_norm = world_x / world_scale * noise_scale;
            let y_norm = world_y / world_scale * noise_scale;
            let tile_norm = TILE_WORLD_SIZE as f64 / world_scale * noise_scale;

            let height_map = PlaneMapBuilder::new(&*fbm)
                .set_size(TILE_PIXEL_SIZE as usize, TILE_PIXEL_SIZE as usize)
                .set_x_bounds(x_norm, x_norm + tile_norm)
                .set_y_bounds(y_norm, y_norm + tile_norm)
                .build()
                .into_iter()
                .map(|n| n as f32)
                .collect::<Vec<_>>();

            result_sender.send(TileGenResult { world_tile, height_map }).unwrap();
        };

        self.task_sender.as_ref().unwrap().send(Box::new(task)).unwrap();
    }

    fn drain_results(&self) -> impl Iterator<Item = TileGenResult> + '_ {
        self.result_receiver.try_iter()
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

    fn size(&self) -> f32 {
        self.half_size * 2.0
    }
}

struct QuadTree {
    root: QuadTreeNode,
}

impl QuadTree {
    fn new(root_center: Vec2, root_half_size: f32, camera_position: &Vec3, lod_factor: f32) -> Self {
        let root_lod_index = (root_half_size * 2.0 / TILE_WORLD_SIZE).log2() as u32;
        let mut root = QuadTreeNode::new(root_center, root_half_size, root_lod_index);

        Self::split_recursive(&mut root, camera_position, lod_factor);

        Self { root }
    }

    fn collect_leafs(&self) -> Vec<&QuadTreeNode> {
        let mut leafs = Vec::new();
        Self::traverse_node(&self.root, &mut leafs);

        leafs
    }

    fn split_recursive(node: &mut QuadTreeNode, camera_position: &Vec3, lod_factor: f32) {
        // || node.lod_index == 0
        if node.half_size <= TILE_WORLD_SIZE * 0.5 {
            return;
        }

        let distance = (camera_position - Vec3::new(node.center.x, 0.0, node.center.y)).length();
        if distance >= node.half_size * lod_factor {
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
            Self::split_recursive(child, camera_position, lod_factor);
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

#[allow(unused)]
struct MapData {
    height_mips: Vec<Vec<f32>>,
    normal_mips: Vec<Vec<Vec3>>,
}

#[allow(unused)]
struct MapGeneratorParams {
    size: usize,
    scale: f32,
    octaves: usize,
    frequency: f64,
    lacunarity: f64,
    persistence: f64,
    seed: u32,
}

#[allow(unused)]
impl MapGeneratorParams {
    fn new(size: usize) -> Self {
        Self {
            size,
            scale: 7.0,
            octaves: 6,
            frequency: 1.0,
            lacunarity: 2.0,
            persistence: 0.5,
            seed: 123,
        }
    }

    fn generate(&self, terrain_size: usize) -> MapData {
        let fbm = Fbm::<Perlin>::new(self.seed)
            .set_octaves(self.octaves)
            .set_frequency(self.frequency)
            .set_lacunarity(self.lacunarity)
            .set_persistence(self.persistence);

        let height_map = PlaneMapBuilder::new(fbm)
            .set_size(self.size, self.size)
            .set_x_bounds(0.0, self.scale as f64)
            .set_y_bounds(0.0, self.scale as f64)
            .build()
            .into_iter()
            .map(|n| n as f32)
            .collect::<Vec<_>>();

        let min = height_map.iter().cloned().fold(f32::INFINITY, f32::min);
        let max = height_map.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        println!("height map: min={}, max={}", min, max);

        let height_map = height_map.iter().map(|n| (n - min) / (max - min)).collect::<Vec<_>>();
        let normal_map = self.generate_normals(height_map.as_slice(), terrain_size);

        MapData {
            height_mips: self.generate_mips(height_map, |s0, s1, s2, s3| (s0 + s1 + s2 + s3) * 0.25),
            normal_mips: self.generate_mips(normal_map, |s0, s1, s2, s3| {
                let n = (s0 + s1 + s2 + s3) * 0.25;

                if n.length_squared() > 1e-8 {
                    n.normalize()
                } else {
                    Vec3::Y
                }
            }),
        }
    }

    fn generate_mips<T, F>(&self, data: Vec<T>, mut downsample: F) -> Vec<Vec<T>>
    where
        T: Copy,
        F: FnMut(T, T, T, T) -> T,
    {
        let mut mips = vec![data];
        let mut current_size = self.size;

        while current_size > 1 {
            current_size /= 2;

            let prev = mips.last().unwrap();
            let mip = (0..current_size * current_size)
                .map(|i| {
                    let x = (i % current_size) * 2;
                    let y = (i / current_size) * 2;
                    let prev_size = current_size * 2;

                    let s00 = prev[y * prev_size + x];
                    let s10 = prev[y * prev_size + x + 1];
                    let s01 = prev[(y + 1) * prev_size + x];
                    let s11 = prev[(y + 1) * prev_size + x + 1];

                    downsample(s00, s10, s01, s11)
                })
                .collect();

            mips.push(mip);
        }

        mips
    }

    fn generate_normals(&self, height_map: &[f32], terrain_size: usize) -> Vec<Vec3> {
        let world_scale = terrain_size as f32 / self.size as f32;
        let mut normals = vec![Vec3::ZERO; self.size * self.size];

        for z in 0..self.size {
            for x in 0..self.size {
                let sample = |sx: i32, sz: i32| -> f32 {
                    let cx = sx.clamp(0, self.size as i32 - 1) as usize;
                    let cz = sz.clamp(0, self.size as i32 - 1) as usize;

                    height_map[cz * self.size + cx]
                };

                let hl = sample(x as i32 - 1, z as i32);
                let hr = sample(x as i32 + 1, z as i32);
                let hb = sample(x as i32, z as i32 - 1);
                let ht = sample(x as i32, z as i32 + 1);

                let height_scale = 10.0;
                let dx = (hl - hr) * height_scale;
                let dz = (hb - ht) * height_scale;

                normals[z * self.size + x] = glam::Vec3::new(dx, 2.0 * world_scale, dz).normalize();
            }
        }

        normals
    }
}
