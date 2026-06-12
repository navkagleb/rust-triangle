use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use bitflags::bitflags;
use glam::{IVec2, Mat4, UVec2, Vec2, Vec3, Vec3Swizzles, f32};
use noise::utils::{NoiseMapBuilder, PlaneMapBuilder};
use noise::{Fbm, MultiFractal, Perlin};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::camera::Camera;
use crate::d3d12_utils::*;
use crate::{BACK_BUFFER_FORMAT, DEPTH_BUFFER_FORMAT, FRAME_COUNT, GpuResource, imgui_text};
use imgui_sys::*;

const PATCH_GEN_WORKER_COUNT: usize = 16;

const PATCH_LOD_COUNT: u32 = 5;
const PATCH_PIXEL_SIZE: u32 = 128;
pub const PATCH_WORLD_SIZE: u32 = PATCH_PIXEL_SIZE / 2;

const ATLAS_PATCH_PIXEL_SIZE: u32 = PATCH_PIXEL_SIZE + 1; // for pixel overlap
const ATLAS_PATCH_COUNT: u32 = 32;
const ATLAS_SIZE: u32 = ATLAS_PATCH_PIXEL_SIZE * ATLAS_PATCH_COUNT;
const HEIGHT_ATLAS_FORMAT: DXGI_FORMAT = DXGI_FORMAT_R32_FLOAT;
const INDIRECTION_SLOT_COUNT: u32 = 128;

const PATCH_SIDE_QUAD_COUNT: u32 = PATCH_PIXEL_SIZE;
const PATCH_SIDE_VERTEX_COUNT: u32 = PATCH_PIXEL_SIZE + 1;
const PATCH_INDEX_COUNT: u32 = PATCH_SIDE_QUAD_COUNT.pow(2) * 6;

bitflags! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug)]
    pub struct StitchMask: u32 {
        const TOP = 1 << 0;
        const BOTTOM = 1 << 1;
        const LEFT = 1 << 2;
        const RIGHT = 1 << 3;
    }
}

#[repr(C)]
struct GpuTerrainPatch {
    pub world_index: IVec2,
    pub lod_index: u32,
    pub stitch_mask: StitchMask,
}

#[repr(C)]
struct GpuTerrainConsts {
    pub world_to_clip: Mat4,
    pub cam_world_index: IVec2,
    pub world_scale: f32,
    pub height_scale: f32,
    pub wireframe_pass: u32,
    pub stitching_enabled: u32,
    pub active_patch_buffer_index: u32,
}

pub struct TerrainPatchStats {
    pub render_count: u32,
    pub cached_count: u32,
    pub requested_count: u32,
    pub generated_count: u32,
    pub uploading_count: u32,
    pub resident_count: u32,
}

impl TerrainPatchStats {
    pub fn gather(terrain: &TerrainData) -> Self {
        let mut stats = Self {
            render_count: (terrain.render_distance * 2) / PATCH_WORLD_SIZE,
            cached_count: terrain.patch_cache.len() as u32,
            requested_count: 0,
            generated_count: 0,
            uploading_count: 0,
            resident_count: 0,
        };

        for state in terrain.patch_cache.values() {
            match state {
                PatchState::Requested => stats.requested_count += 1,
                PatchState::Generated(_) => stats.generated_count += 1,
                PatchState::Uploading(_, _) => stats.uploading_count += 1,
                PatchState::Resident(_) => stats.resident_count += 1,
            }
        }

        stats
    }
}

pub struct TerrainData {
    pub render_distance: u32,
    pub lod_factor: f32,

    pub height_scale: f32,
    pub world_scale: f32,

    pub solid_mode: bool,
    pub wireframe_mode: bool,
    pub stitching_enabled: bool,

    cam_world_index: IVec2,
    leaf_patches: Vec<PatchKey>,

    patch_cache: HashMap<PatchKey, PatchState>,
    patch_gen_pool: PatchGenPool,
    atlas_free_slots: Vec<UVec2>,

    patch_index_buffer: ID3D12Resource,
    #[allow(unused)]
    patch_buffer: ID3D12Resource,
    patch_buffer_item_count: u32,
    patch_buffer_ptr: *mut GpuTerrainPatch,

    indirection_texture: ID3D12Resource,
    indirection_texture_upload: ID3D12Resource,
    indirection_texture_ptr: *mut UVec2,
    indirection_texture_size: usize,

    height_atlas: ID3D12Resource,
    height_atlas_upload: ID3D12Resource,
    height_atlas_ptr: *mut f32,
    height_atlas_size: usize,

    solid_const_buffer: ConstBuffer<GpuTerrainConsts>,
    wireframe_const_buffer: ConstBuffer<GpuTerrainConsts>,

    solid_vertex_pso: ID3D12PipelineState,
    wireframe_vertex_pso: ID3D12PipelineState,

    // Debug
    minimap_offset: Vec2,
    minimap_zoom: f32,
}

impl TerrainData {
    pub fn new(
        device: &ID3D12Device4,
        resource_heap: &ID3D12DescriptorHeap,
        root_signature: &ID3D12RootSignature,
    ) -> Result<Self> {
        let patch_indices = {
            let mut indices = Vec::with_capacity(PATCH_INDEX_COUNT as usize);

            for z in 0..PATCH_SIDE_QUAD_COUNT {
                for x in 0..PATCH_SIDE_QUAD_COUNT {
                    let top_left = z * PATCH_SIDE_VERTEX_COUNT + x;
                    let top_right = top_left + 1;
                    let bottom_left = top_left + PATCH_SIDE_VERTEX_COUNT;
                    let bottom_right = bottom_left + 1;

                    if (x + z) % 2 == 0 {
                        indices.extend_from_slice(&[
                            top_left,
                            bottom_left,
                            bottom_right,
                            top_left,
                            bottom_right,
                            top_right,
                        ]);
                    } else {
                        indices.extend_from_slice(&[
                            top_left,
                            bottom_left,
                            top_right,
                            top_right,
                            bottom_left,
                            bottom_right,
                        ]);
                    }
                }
            }

            indices
        };
        let patch_index_buffer =
            ID3D12Resource::new_buffer(device, D3D12_HEAP_TYPE_UPLOAD, size_of_val(patch_indices.as_slice()))?;

        patch_index_buffer.map_and_write(patch_indices.as_slice())?;

        let render_distance = 2048;
        let lod_factor = 3.0;

        let max_patch_count = ((render_distance * 2) / PATCH_WORLD_SIZE).pow(2); // should be somehow recalculated
        let patch_buffer = ID3D12Resource::new_buffer(
            device,
            D3D12_HEAP_TYPE_UPLOAD,
            (max_patch_count * FRAME_COUNT) as usize * size_of::<GpuTerrainPatch>(),
        )?;

        unsafe {
            for i in 0..FRAME_COUNT {
                device.CreateShaderResourceView(
                    &patch_buffer,
                    Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                        Format: DXGI_FORMAT_UNKNOWN,
                        ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
                        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                            Buffer: D3D12_BUFFER_SRV {
                                FirstElement: (i * max_patch_count) as u64,
                                NumElements: max_patch_count,
                                StructureByteStride: size_of::<GpuTerrainPatch>() as u32,
                                Flags: D3D12_BUFFER_SRV_FLAG_NONE,
                            },
                        },
                    }),
                    resource_heap.get_cpu_handle(device, GpuResource::TerrainPatchBufferFirst as u32 + i),
                );
            }

            device.CreateShaderResourceView(
                &patch_index_buffer,
                Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: DXGI_FORMAT_R32_UINT,
                    ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Buffer: D3D12_BUFFER_SRV {
                            FirstElement: 0,
                            NumElements: patch_indices.len() as u32,
                            StructureByteStride: 0,
                            Flags: D3D12_BUFFER_SRV_FLAG_NONE,
                        },
                    },
                }),
                resource_heap.get_cpu_handle(device, GpuResource::TerrainPatchIndexBuffer as u32),
            );
        }

        let get_texture_size = |texture: &ID3D12Resource| -> usize {
            let desc = unsafe { texture.GetDesc() };
            let mut size = 0;

            unsafe {
                device.GetCopyableFootprints(
                    &desc,
                    0,
                    (desc.MipLevels * desc.DepthOrArraySize) as u32,
                    0,
                    None,
                    None,
                    None,
                    Some(&mut size),
                );
            }

            size as usize
        };

        let indirection_format = DXGI_FORMAT_R32G32_UINT;
        let indirection_texture = ID3D12Resource::new_texture_2d(
            device,
            indirection_format,
            INDIRECTION_SLOT_COUNT,
            INDIRECTION_SLOT_COUNT,
            PATCH_LOD_COUNT,
        )?;
        let indirection_texture_size = get_texture_size(&indirection_texture);
        let indirection_texture_upload = ID3D12Resource::new_buffer(
            device,
            D3D12_HEAP_TYPE_UPLOAD,
            indirection_texture_size * FRAME_COUNT as usize,
        )?;

        unsafe {
            device.CreateShaderResourceView(
                &indirection_texture,
                Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: indirection_format,
                    ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2D: D3D12_TEX2D_SRV {
                            MostDetailedMip: 0,
                            MipLevels: PATCH_LOD_COUNT,
                            PlaneSlice: 0,
                            ResourceMinLODClamp: 0.0,
                        },
                    },
                }),
                resource_heap.get_cpu_handle(device, GpuResource::TerrainIndirectionTexture as u32),
            );
        }

        let height_atlas = ID3D12Resource::new_texture_2d(device, HEIGHT_ATLAS_FORMAT, ATLAS_SIZE, ATLAS_SIZE, 1)?;
        let height_atlas_size = get_texture_size(&height_atlas);
        let height_atlas_upload =
            ID3D12Resource::new_buffer(device, D3D12_HEAP_TYPE_UPLOAD, height_atlas_size * FRAME_COUNT as usize)?;

        unsafe {
            device.CreateShaderResourceView(
                &height_atlas,
                Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: HEIGHT_ATLAS_FORMAT,
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

        let vs_blob = std::fs::read(std::path::Path::new("target/dxil/terrain.vs.dxil"))?;
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

        Ok(Self {
            render_distance,
            lod_factor,

            height_scale: 15.0,
            world_scale: 1.0,

            solid_mode: false,
            wireframe_mode: true,
            stitching_enabled: true,

            cam_world_index: IVec2::ZERO,
            leaf_patches: Vec::new(),

            patch_cache: HashMap::new(),
            patch_gen_pool: PatchGenPool::new(),
            atlas_free_slots: {
                let mut free_slots = Vec::with_capacity((ATLAS_PATCH_COUNT * ATLAS_PATCH_COUNT) as usize);
                for y in (0..ATLAS_PATCH_COUNT).rev() {
                    for x in (0..ATLAS_PATCH_COUNT).rev() {
                        free_slots.push(UVec2::new(x, y));
                    }
                }

                free_slots
            },

            patch_index_buffer,
            patch_buffer_item_count: max_patch_count,
            patch_buffer_ptr: patch_buffer.map::<GpuTerrainPatch>()?,
            patch_buffer,

            indirection_texture_ptr: indirection_texture_upload.map::<UVec2>()?,
            indirection_texture_upload,
            indirection_texture,
            indirection_texture_size,

            height_atlas_ptr: height_atlas_upload.map::<f32>()?,
            height_atlas_upload,
            height_atlas,
            height_atlas_size,

            solid_const_buffer: ConstBuffer::new(device)?,
            wireframe_const_buffer: ConstBuffer::new(device)?,

            solid_vertex_pso: create_vertex_pso(create_rasterizer_state(D3D12_FILL_MODE_SOLID))?,
            wireframe_vertex_pso: create_vertex_pso(create_rasterizer_state(D3D12_FILL_MODE_WIREFRAME))?,

            minimap_offset: Vec2::ZERO,
            minimap_zoom: 1.0,
        })
    }

    pub fn leaf_patches(&self) -> &[PatchKey] {
        &self.leaf_patches
    }

    pub fn collect_leaf_patches(&mut self, cam_pos: &Vec3, active_frame_index: u32) -> Result<()> {
        let qtree = PatchQuadTree::new(cam_pos, self.render_distance, self.lod_factor);

        self.leaf_patches = qtree.collect_leafs();
        self.cam_world_index = cam_pos.xz().as_ivec2() / PATCH_WORLD_SIZE as i32;

        let mut missing_patches = self
            .leaf_patches
            .iter()
            .filter(|l| !self.patch_cache.contains_key(l))
            .collect::<Vec<_>>();

        missing_patches.sort_unstable_by(|a, b| {
            let distance_a = (cam_pos - a.world_center().extend(0).xzy().as_vec3()).length_squared();
            let distance_b = (cam_pos - b.world_center().extend(0).xzy().as_vec3()).length_squared();

            distance_a.total_cmp(&distance_b)
        });

        for result in self.patch_gen_pool.drain_results() {
            self.patch_cache
                .insert(result.request, PatchState::Generated(result.height_map));
        }

        for &key in missing_patches {
            self.patch_gen_pool.requst_patch_generation(key);
            self.patch_cache.insert(key, PatchState::Requested);
        }

        let is_neighbor_coarser = |node: &PatchKey, direction: IVec2| -> bool {
            let probe = node.world_center() + direction * node.world_size() as i32;

            let neighbor_lod_index = self
                .leaf_patches
                .iter()
                .find(|l| (l.world_center() - probe).length_squared() < node.world_size().pow(2) as i32)
                .map(|l| l.lod_index)
                .unwrap_or(node.lod_index);

            neighbor_lod_index > node.lod_index
        };

        let gpu_patches = self
            .leaf_patches
            .iter()
            .filter(|l| {
                self.patch_cache
                    .get(l)
                    .is_some_and(|s| matches!(s, PatchState::Resident(_)))
            })
            .map(|l| {
                let directions = [
                    (StitchMask::TOP, IVec2::NEG_Y),
                    (StitchMask::BOTTOM, IVec2::Y),
                    (StitchMask::LEFT, IVec2::NEG_X),
                    (StitchMask::RIGHT, IVec2::X),
                ];

                let mut stitch_mask = StitchMask::empty();

                for &(flag, direction) in &directions {
                    if is_neighbor_coarser(l, direction) {
                        stitch_mask.insert(flag);
                    }
                }

                GpuTerrainPatch {
                    world_index: l.world_index,
                    lod_index: l.lod_index,
                    stitch_mask,
                }
            })
            .collect::<Vec<_>>();

        unsafe {
            std::ptr::copy_nonoverlapping(
                gpu_patches.as_ptr(),
                self.patch_buffer_ptr
                    .add((active_frame_index * self.patch_buffer_item_count) as usize),
                gpu_patches.len(),
            );
        }

        Ok(())
    }

    pub fn upload_atlas_data(
        &mut self,
        device: &ID3D12Device,
        cmd_list: &ID3D12GraphicsCommandList,
        cpu_frame_index: u64,
        gpu_frame_index: u64,
        active_frame_index: u32,
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

        let upload_byte_offset = active_frame_index as usize * self.height_atlas_size;

        let mut patches_to_update = Vec::new();

        for (&key, state) in &self.patch_cache {
            if let PatchState::Uploading(atlas_index, frame_index) = state
                && *frame_index <= gpu_frame_index
            {
                patches_to_update.push((key, PatchState::Resident(*atlas_index)));
                continue;
            }

            let PatchState::Generated(height_data) = state else {
                continue;
            };

            let atlas_index = self.atlas_free_slots.pop().unwrap();
            let atlas_row_pitch = atlas_layout.Footprint.RowPitch;

            let patch_offset_bytes = atlas_index.y * ATLAS_PATCH_PIXEL_SIZE * atlas_row_pitch
                + atlas_index.x * ATLAS_PATCH_PIXEL_SIZE * size_of::<f32>() as u32;

            for row in 0..ATLAS_PATCH_PIXEL_SIZE {
                let src_offset = row * ATLAS_PATCH_PIXEL_SIZE;
                let dst_offset = patch_offset_bytes + row * atlas_row_pitch;

                unsafe {
                    std::ptr::copy_nonoverlapping(
                        height_data.as_ptr().add(src_offset as usize),
                        self.height_atlas_ptr.byte_add(upload_byte_offset + dst_offset as usize),
                        ATLAS_PATCH_PIXEL_SIZE as usize,
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
                    atlas_index.x * ATLAS_PATCH_PIXEL_SIZE,
                    atlas_index.y * ATLAS_PATCH_PIXEL_SIZE,
                    0,
                    &D3D12_TEXTURE_COPY_LOCATION {
                        pResource: std::mem::transmute_copy(&self.height_atlas_upload),
                        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                            PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                                Offset: upload_byte_offset as u64 + patch_offset_bytes as u64,
                                Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                                    Format: HEIGHT_ATLAS_FORMAT,
                                    Width: ATLAS_PATCH_PIXEL_SIZE,
                                    Height: ATLAS_PATCH_PIXEL_SIZE,
                                    Depth: 1,
                                    RowPitch: atlas_row_pitch,
                                },
                            },
                        },
                    },
                    None,
                );
            }

            patches_to_update.push((key, PatchState::Uploading(atlas_index, cpu_frame_index)));
        }

        for (key, state) in patches_to_update {
            self.patch_cache.insert(key, state);
        }

        Ok(())
    }

    pub fn upload_indirection_data(
        &self,
        device: &ID3D12Device,
        cmd_list: &ID3D12GraphicsCommandList,
        active_frame_index: u32,
    ) -> Result<()> {
        let empty_patch = UVec2::splat(ATLAS_PATCH_COUNT);

        let mut resident_patch_lods: [Vec<UVec2>; PATCH_LOD_COUNT as usize] = std::array::from_fn(|i| {
            let slot_count = INDIRECTION_SLOT_COUNT >> i;
            vec![empty_patch; slot_count.pow(2) as usize]
        });

        for (key, state) in &self.patch_cache {
            let PatchState::Resident(atlas_index) = state else {
                continue;
            };

            let lod_index = key.lod_index;
            let slot_count = INDIRECTION_SLOT_COUNT >> lod_index;

            let relative_index = (key.world_index >> lod_index) - (self.cam_world_index >> lod_index);
            let indirection_index = relative_index + slot_count as i32 / 2;

            let range = 0..slot_count as i32;
            if !range.contains(&indirection_index.x) || !range.contains(&indirection_index.y) {
                continue;
            }

            let flat_indirection_index = indirection_index.y as u32 * slot_count + indirection_index.x as u32;
            resident_patch_lods[lod_index as usize][flat_indirection_index as usize] = *atlas_index;
        }

        let desc = unsafe { self.indirection_texture.GetDesc() };
        let mut layouts = vec![D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); PATCH_LOD_COUNT as usize];

        unsafe {
            device.GetCopyableFootprints(
                &desc,
                0,
                PATCH_LOD_COUNT,
                0,
                Some(layouts.as_mut_ptr()),
                None,
                None,
                None,
            );
        }

        let upload_byte_offset = active_frame_index as usize * self.indirection_texture_size;

        for lod_index in 0..PATCH_LOD_COUNT {
            let slot_count = INDIRECTION_SLOT_COUNT >> lod_index;

            let gpu_layout = layouts[lod_index as usize];
            let gpu_row_pitch = gpu_layout.Footprint.RowPitch;
            let gpu_lod_offset = gpu_layout.Offset;

            for row_index in 0..slot_count {
                let cpu_offset = row_index * slot_count;
                let gpu_offset = gpu_lod_offset + (row_index * gpu_row_pitch) as u64;

                unsafe {
                    std::ptr::copy_nonoverlapping(
                        resident_patch_lods[lod_index as usize]
                            .as_ptr()
                            .add(cpu_offset as usize),
                        self.indirection_texture_ptr
                            .byte_add(upload_byte_offset + gpu_offset as usize),
                        slot_count as usize,
                    );
                }
            }

            unsafe {
                cmd_list.CopyTextureRegion(
                    &D3D12_TEXTURE_COPY_LOCATION {
                        pResource: std::mem::transmute_copy(&self.indirection_texture),
                        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                            SubresourceIndex: lod_index,
                        },
                    },
                    0,
                    0,
                    0,
                    &D3D12_TEXTURE_COPY_LOCATION {
                        pResource: std::mem::transmute_copy(&self.indirection_texture_upload),
                        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                            PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                                Offset: upload_byte_offset as u64 + gpu_lod_offset,
                                Footprint: layouts[lod_index as usize].Footprint,
                            },
                        },
                    },
                    None,
                );
            }
        }

        Ok(())
    }

    pub fn render(&self, cmd_list: &ID3D12GraphicsCommandList, cam: &Camera, active_frame_index: u32) {
        let mut consts = GpuTerrainConsts {
            world_to_clip: cam.world_to_clip(),
            cam_world_index: self.cam_world_index,
            world_scale: self.world_scale,
            height_scale: self.height_scale,
            wireframe_pass: false.into(),
            stitching_enabled: self.stitching_enabled.into(),
            active_patch_buffer_index: GpuResource::TerrainPatchBufferFirst as u32 + active_frame_index,
        };

        let render_terrain = |vertex_pso: &ID3D12PipelineState| {
            if self.leaf_patches.is_empty() {
                return;
            }

            unsafe {
                cmd_list.SetPipelineState(vertex_pso);
                cmd_list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                cmd_list.IASetIndexBuffer(Some(&D3D12_INDEX_BUFFER_VIEW {
                    BufferLocation: self.patch_index_buffer.GetGPUVirtualAddress(),
                    SizeInBytes: PATCH_INDEX_COUNT * size_of::<f32>() as u32,
                    Format: DXGI_FORMAT_R32_UINT,
                }));

                cmd_list.DrawIndexedInstanced(PATCH_INDEX_COUNT, self.leaf_patches.len() as u32, 0, 0, 0);
            }
        };

        if self.solid_mode {
            unsafe {
                cmd_list
                    .SetGraphicsRootConstantBufferView(1, self.solid_const_buffer.write(active_frame_index, &consts));
            }
            render_terrain(&self.solid_vertex_pso);
        }

        if self.wireframe_mode {
            consts.wireframe_pass = true.into();

            unsafe {
                cmd_list.SetGraphicsRootConstantBufferView(
                    1,
                    self.wireframe_const_buffer.write(active_frame_index, &consts),
                );
            }
            render_terrain(&self.wireframe_vertex_pso);
        }
    }

    pub fn render_imgui_qtree(&mut self, cam_pos: &Vec3) {
        unsafe {
            ImGui_Begin(c"TerrainQuadTree".as_ptr(), std::ptr::null_mut(), 0);

            if ImGui_Button(c"Reset view".as_ptr()) {
                self.minimap_offset = Vec2::ZERO;
                self.minimap_zoom = 1.0;
            }

            ImGui_SameLine();
            imgui_text!("Render distance: {:.2}", self.render_distance);

            let minimap_pos = Vec2::new(ImGui_GetCursorScreenPos().x, ImGui_GetCursorScreenPos().y);
            let minimap_size = {
                let size = ImGui_GetContentRegionAvail();
                size.x.min(size.y)
            };
            ImGui_InvisibleButton(
                c"minimap".as_ptr(),
                ImVec2 {
                    x: minimap_size,
                    y: minimap_size,
                },
                ImGuiButtonFlags_MouseButtonRight,
            );

            let button = ImGuiMouseButton_Right;
            if ImGui_IsItemActive() && ImGui_IsMouseDragging(button, 0.0) {
                let delta = ImGui_GetMouseDragDelta(button, 0.0);
                self.minimap_offset.x += delta.x;
                self.minimap_offset.y += delta.y;

                ImGui_ResetMouseDragDeltaEx(button);
            }

            if ImGui_IsItemHovered(ImGuiHoveredFlags_None) {
                let scroll = ImGui_GetIO().as_ref().unwrap().MouseWheel;
                if scroll != 0.0 {
                    let mouse_pos = Vec2::new(ImGui_GetMousePos().x, ImGui_GetMousePos().y);
                    let mouse_relative_pos = mouse_pos - (minimap_pos + minimap_size * 0.5 + self.minimap_offset);

                    let prev_zoom = self.minimap_zoom;
                    self.minimap_zoom = (self.minimap_zoom * (1.0 + scroll * 0.1)).clamp(0.1, 10.0);

                    let zoom_factor = self.minimap_zoom / prev_zoom;
                    self.minimap_offset += mouse_relative_pos - mouse_relative_pos * zoom_factor;
                }
            }

            let minimap_center = minimap_pos + minimap_size * 0.5 + self.minimap_offset;
            let minimap_scale = minimap_size / (self.render_distance as f32 * 2.0) * self.minimap_zoom;

            let draw_list = ImGui_GetWindowDrawList();

            for leaf in &self.leaf_patches {
                let minimap_leaf_pos = minimap_center + leaf.world_pos().as_vec2() * minimap_scale;
                let minimap_leaf_size = leaf.world_size() as f32 * minimap_scale;

                ImDrawList_AddRectEx(
                    draw_list,
                    ImVec2 {
                        x: minimap_leaf_pos.x,
                        y: minimap_leaf_pos.y,
                    },
                    ImVec2 {
                        x: minimap_leaf_pos.x + minimap_leaf_size,
                        y: minimap_leaf_pos.y + minimap_leaf_size,
                    },
                    0xB3FFFFFF,
                    0.0,
                    ImDrawFlags_None,
                    0.5,
                );

                let label = std::ffi::CString::new(leaf.lod_index.to_string()).unwrap();
                let label_size = ImGui_CalcTextSize(label.as_ptr());

                if label_size.x >= minimap_leaf_size || label_size.y >= minimap_leaf_size {
                    continue;
                }

                ImDrawList_AddText(
                    draw_list,
                    ImVec2 {
                        x: minimap_leaf_pos.x + minimap_leaf_size * 0.5 - label_size.x * 0.5,
                        y: minimap_leaf_pos.y + minimap_leaf_size * 0.5 - label_size.y * 0.5,
                    },
                    0xFFFFFFFF,
                    label.as_ptr(),
                );
            }

            let minimap_cam_pos = minimap_center + cam_pos.xz() * minimap_scale;
            ImDrawList_AddCircleFilled(
                draw_list,
                ImVec2 {
                    x: minimap_cam_pos.x,
                    y: minimap_cam_pos.y,
                },
                5.0,
                0xFF0000FF,
                5,
            );

            let start = self
                .leaf_patches
                .iter()
                .map(|l| l.world_pos())
                .fold(IVec2::MAX, |acc, p| acc.min(p));
            let end = self
                .leaf_patches
                .iter()
                .map(|l| l.world_pos() + l.world_size() as i32)
                .fold(IVec2::MIN, |acc, p| acc.max(p));

            let corners = [
                (minimap_pos, format!("X={:.0} Z={:.0}", start.x, start.y)),
                (
                    minimap_pos + Vec2::new(minimap_size, 0.0),
                    format!("X={:.0} Z={:.0}", end.x, start.y),
                ),
                (
                    minimap_pos + Vec2::new(0.0, minimap_size),
                    format!("X={:.0} Z={:.0}", start.x, end.y),
                ),
                (
                    minimap_pos + Vec2::new(minimap_size, minimap_size),
                    format!("X={:.0} Z={:.0}", end.x, end.y),
                ),
            ];

            let padding = 4.0;
            for (corner, label) in &corners {
                let text = std::ffi::CString::new(label.as_str()).unwrap();
                let text_size = ImGui_CalcTextSize(text.as_ptr());

                let x = if corner.x == minimap_pos.x {
                    corner.x + padding
                } else {
                    corner.x - text_size.x - padding
                };

                let y = if corner.y == minimap_pos.y {
                    corner.y + padding
                } else {
                    corner.y - text_size.y - padding
                };

                ImDrawList_AddText(draw_list, ImVec2 { x, y }, 0xFFFFFFFF, text.as_ptr());
            }

            ImGui_End();
        }
    }
}

enum PatchState {
    Requested,
    Generated(Vec<f32>),
    Uploading(UVec2, u64),
    Resident(UVec2),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PatchKey {
    pub world_index: IVec2,
    pub lod_index: u32,
}

impl PatchKey {
    fn world_pos(&self) -> IVec2 {
        self.world_index * PATCH_WORLD_SIZE as i32
    }

    fn world_size(&self) -> u32 {
        PATCH_WORLD_SIZE * 2_u32.pow(self.lod_index)
    }

    fn world_center(&self) -> IVec2 {
        self.world_pos() + self.world_size() as i32 / 2
    }
}

type PatchGenRequest = PatchKey;

struct PatchGenResult {
    request: PatchGenRequest,
    height_map: Vec<f32>,
}

struct PatchGenPool {
    workers: Vec<std::thread::JoinHandle<()>>,
    request_sender: Option<Sender<PatchGenRequest>>,
    result_receiver: Receiver<PatchGenResult>,
}

impl Drop for PatchGenPool {
    fn drop(&mut self) {
        drop(self.request_sender.take());

        for worker in self.workers.drain(..) {
            worker.join().unwrap();
        }
    }
}

impl PatchGenPool {
    fn new() -> Self {
        let (request_sender, request_receiver) = std::sync::mpsc::channel::<PatchGenRequest>();
        let (result_sender, result_receiver) = std::sync::mpsc::channel::<PatchGenResult>();

        let request_receiver = Arc::new(Mutex::new(request_receiver));

        let fbm = Fbm::<Perlin>::new(123)
            .set_octaves(8)
            .set_frequency(1.0)
            .set_lacunarity(2.0)
            .set_persistence(0.5);

        let workers = (0..PATCH_GEN_WORKER_COUNT)
            .map(|i| {
                let request_receiver = Arc::clone(&request_receiver);
                let result_sender = result_sender.clone();
                let fbm = fbm.clone();

                std::thread::Builder::new()
                    .name(format!("tile-generator-{}", i))
                    .spawn(move || {
                        loop {
                            let request = request_receiver.lock().unwrap().recv();
                            let Ok(request) = request else {
                                break;
                            };

                            let noise_scale = 4.0_f64;
                            let world_scale = 2048.0_f64;

                            let fbm_pos = request.world_pos().as_dvec2() / world_scale * noise_scale;
                            let fbm_size = request.world_size() as f64 / world_scale * noise_scale;
                            let fbm_pixel_size =
                                request.world_size() as f64 / PATCH_PIXEL_SIZE as f64 / world_scale * noise_scale;

                            let height_map = PlaneMapBuilder::new(&fbm)
                                .set_size(ATLAS_PATCH_PIXEL_SIZE as usize, ATLAS_PATCH_PIXEL_SIZE as usize)
                                .set_x_bounds(fbm_pos.x, fbm_pos.x + fbm_size + fbm_pixel_size) // pixel overlap
                                .set_y_bounds(fbm_pos.y, fbm_pos.y + fbm_size + fbm_pixel_size) // pixel overlap
                                .build()
                                .into_iter()
                                .map(|n| (n as f32 * 1.5 + 0.3).clamp(0.0, 1.0))
                                .collect::<Vec<_>>();

                            result_sender.send(PatchGenResult { request, height_map }).unwrap();
                        }
                    })
                    .unwrap()
            })
            .collect();

        Self {
            workers,
            request_sender: Some(request_sender),
            result_receiver,
        }
    }

    fn requst_patch_generation(&self, request: PatchGenRequest) {
        self.request_sender.as_ref().unwrap().send(request).unwrap()
    }

    fn drain_results(&self) -> impl Iterator<Item = PatchGenResult> + '_ {
        self.result_receiver.try_iter()
    }
}

#[derive(Clone)]
struct PatchQuadNode {
    key: PatchKey,
    children: Option<Box<[PatchQuadNode; 4]>>,
}

impl PatchQuadNode {
    fn root(cam_pos: &Vec3, render_distance: u32) -> Self {
        let snap_size = PATCH_WORLD_SIZE * 2_u32.pow(PATCH_LOD_COUNT - 1);
        let snapped_cam_pos = (cam_pos.xz() / snap_size as f32).round().as_ivec2() * snap_size as i32;

        Self::new(
            (snapped_cam_pos / PATCH_WORLD_SIZE as i32) - (render_distance / PATCH_WORLD_SIZE) as i32,
            (render_distance * 2 / PATCH_WORLD_SIZE).ilog2(),
        )
    }

    fn new(world_index: IVec2, lod_index: u32) -> Self {
        Self {
            key: PatchKey { world_index, lod_index },
            children: None,
        }
    }
}

struct PatchQuadTree {
    root: PatchQuadNode,
}

impl PatchQuadTree {
    fn new(cam_pos: &Vec3, render_distance: u32, lod_factor: f32) -> Self {
        let mut root = PatchQuadNode::root(cam_pos, render_distance);
        Self::split_recursive(&mut root, cam_pos, lod_factor);

        Self { root }
    }

    fn collect_leafs(&self) -> Vec<PatchKey> {
        let mut leafs = Vec::new();
        Self::traverse_node(&self.root, &mut leafs);

        leafs
    }

    fn split_recursive(node: &mut PatchQuadNode, cam_pos: &Vec3, lod_factor: f32) {
        let distance = (cam_pos - node.key.world_center().extend(0).xzy().as_vec3()).length();
        if distance >= (node.key.world_size() as f32 * 0.5 * lod_factor) && node.key.lod_index <= (PATCH_LOD_COUNT - 1)
        {
            return;
        }

        let next_lod_index = node.key.lod_index - 1;
        let next_offset = 2_u32.pow(next_lod_index) as i32;

        node.children = Some(Box::new([
            PatchQuadNode::new(node.key.world_index + IVec2::ZERO * next_offset, next_lod_index),
            PatchQuadNode::new(node.key.world_index + IVec2::X * next_offset, next_lod_index),
            PatchQuadNode::new(node.key.world_index + IVec2::Y * next_offset, next_lod_index),
            PatchQuadNode::new(node.key.world_index + IVec2::ONE * next_offset, next_lod_index),
        ]));

        if next_lod_index == 0 {
            return;
        }

        for child in node.children.as_mut().unwrap().iter_mut() {
            Self::split_recursive(child, cam_pos, lod_factor);
        }
    }

    fn traverse_node(node: &PatchQuadNode, leafs: &mut Vec<PatchKey>) {
        if node.children.is_none() {
            leafs.push(node.key);
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
