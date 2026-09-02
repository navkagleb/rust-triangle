mod config;
mod gpu_types;
mod patch;
mod patch_cache;
mod patch_generator;
mod patch_quad_tree;
mod patch_queue;
mod terrain_imgui;
mod texture_atlas;

use anyhow::Result;
use glam::{IVec2, Vec2, Vec3, Vec3Swizzles, f32};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::camera::Camera;
use crate::d3d12_utils::*;
use crate::{BACK_BUFFER_FORMAT, DEPTH_BUFFER_FORMAT, FRAME_COUNT, GpuResource};
use config::*;
use gpu_types::{GpuTerrainConsts, GpuTerrainPatch};
use patch::{PatchKey, PatchStitchMask};
use patch_cache::{PatchCache, PatchUpload};
use patch_generator::{PatchGenerator, WantedPatch};
use patch_quad_tree::PatchQuadTree;
use texture_atlas::TextureAtlas;

pub struct Terrain {
    render_distance: u32,
    lod_factor: f32,

    terrain_to_world_scale: f32,
    terrain_height_scale: f32,

    solid_mode: bool,
    wireframe_mode: bool,
    display_normals: bool,
    stitching_enabled: bool,
    pause_sun_animation: bool,
    terrain_elapsed_time: f32,

    freeze_camera: bool,
    camera_pos: Vec2,
    camera_forward: Vec2,
    camera_grid_index: IVec2,

    patch_generator: PatchGenerator,
    patch_cache: PatchCache,
    patches_to_upload: Vec<PatchUpload>,
    patches_to_render: Vec<PatchKey>,

    patch_index_buffer: ID3D12Resource,
    #[allow(unused)]
    patch_buffer: ID3D12Resource,
    patch_buffer_item_count: u32,
    patch_buffer_ptr: *mut GpuTerrainPatch,

    height_atlas: TextureAtlas<f32>,
    gradient_atlas: TextureAtlas<Vec2>,

    solid_const_buffer: ConstBuffer<GpuTerrainConsts>,
    wireframe_const_buffer: ConstBuffer<GpuTerrainConsts>,

    solid_vertex_pso: ID3D12PipelineState,
    wireframe_vertex_pso: ID3D12PipelineState,

    // Debug
    minimap_offset: Vec2,
    minimap_zoom: f32,
}

impl Terrain {
    pub fn new(
        device: &ID3D12Device4,
        resource_heap: &DescriptorHeap,
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
        let patch_index_buffer = ID3D12Resource::new_buffer(
            device,
            D3D12_HEAP_TYPE_UPLOAD,
            size_of_val(patch_indices.as_slice()) as u64,
        )?;

        patch_index_buffer.map_and_write(patch_indices.as_slice())?;

        let render_distance = 4096;

        let max_patch_count = ((render_distance * 2) / PATCH_TERRAIN_SIZE).pow(2); // should be somehow recalculated
        let patch_buffer = ID3D12Resource::new_buffer(
            device,
            D3D12_HEAP_TYPE_UPLOAD,
            (max_patch_count * FRAME_COUNT) as u64 * size_of::<GpuTerrainPatch>() as u64,
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
                    resource_heap.get_cpu_handle(GpuResource::TerrainPatchBufferFirst as u32 + i),
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
                resource_heap.get_cpu_handle(GpuResource::TerrainPatchIndexBuffer as u32),
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
            lod_factor: 3.5,

            terrain_to_world_scale: 1.0,
            terrain_height_scale: 120.0,

            solid_mode: true,
            wireframe_mode: false,
            display_normals: false,
            stitching_enabled: true,
            pause_sun_animation: false,
            terrain_elapsed_time: 0.0,

            freeze_camera: false,
            camera_pos: Vec2::ZERO,
            camera_forward: Vec2::Y,
            camera_grid_index: IVec2::ZERO,

            patch_generator: PatchGenerator::new(),
            patch_cache: PatchCache::new(),
            patches_to_upload: Vec::new(),
            patches_to_render: Vec::new(),

            patch_index_buffer,
            patch_buffer_item_count: max_patch_count,
            patch_buffer_ptr: patch_buffer.map::<GpuTerrainPatch>()?,
            patch_buffer,

            height_atlas: TextureAtlas::new(
                device,
                resource_heap.get_cpu_handle(GpuResource::TerrainHeightAtlas as u32),
                DXGI_FORMAT_R32_FLOAT,
                "HeightAtlas",
            )?,
            gradient_atlas: TextureAtlas::new(
                device,
                resource_heap.get_cpu_handle(GpuResource::TerrainGradientAtlas as u32),
                DXGI_FORMAT_R32G32_FLOAT,
                "NormalAtlas",
            )?,

            solid_const_buffer: ConstBuffer::new(device)?,
            wireframe_const_buffer: ConstBuffer::new(device)?,

            solid_vertex_pso: create_vertex_pso(create_rasterizer_state(D3D12_FILL_MODE_SOLID))?,
            wireframe_vertex_pso: create_vertex_pso(create_rasterizer_state(D3D12_FILL_MODE_WIREFRAME))?,

            minimap_offset: Vec2::ZERO,
            minimap_zoom: 1.0,
        })
    }

    pub fn update_camera(&mut self, camera_pos: &Vec3, camera_forward: &Vec3, dt: f32) {
        if !self.freeze_camera {
            self.camera_pos = self.world_to_terrain_pos(*camera_pos);
            self.camera_forward = camera_forward.xz().normalize_or_zero();
            self.camera_grid_index = self.camera_pos.as_ivec2() / PATCH_TERRAIN_SIZE as i32;
        }

        if !self.pause_sun_animation {
            self.terrain_elapsed_time += dt;
        }
    }

    pub fn update(&mut self, cpu_frame_index: u64, gpu_frame_index: u64, active_frame_index: u32) {
        self.collect_generated_patches();

        let qtree = PatchQuadTree::build(self.camera_pos, self.render_distance, self.lod_factor);
        // It's better to update the cache states before calling 'select' to avoid one-frame delay
        let selection = qtree.select(&self.patch_cache);

        self.patch_generator
            .update_wanted_patches(selection.missing.into_iter().map(|missing| {
                WantedPatch::new(
                    missing.patch,
                    missing.coverage_required,
                    self.camera_pos,
                    self.camera_forward,
                )
            }));

        let uploads = self.patch_cache.update(
            cpu_frame_index,
            gpu_frame_index,
            selection.renderable.iter().chain(&selection.retained),
        );

        self.patches_to_upload = uploads;
        self.patches_to_render = selection.renderable;

        self.write_gpu_patch_buffer(active_frame_index);
    }

    pub fn render(&self, cmd_list: &ID3D12GraphicsCommandList, camera: &Camera, active_frame_index: u32) {
        for upload in &self.patches_to_upload {
            self.height_atlas.copy_to(
                cmd_list,
                active_frame_index,
                upload.atlas_slot,
                upload.data.heights.as_slice(),
            );

            self.gradient_atlas.copy_to(
                cmd_list,
                active_frame_index,
                upload.atlas_slot,
                upload.data.gradients.as_slice(),
            );
        }

        let mut consts = GpuTerrainConsts {
            world_to_clip: camera.world_to_clip(),
            camera_grid_index: self.camera_grid_index,
            terrain_to_world_scale: self.terrain_to_world_scale,
            terrain_height_scale: self.terrain_height_scale,
            elapsed_time: self.terrain_elapsed_time,
            stitching_enabled: self.stitching_enabled.into(),
            active_patch_buffer_index: GpuResource::TerrainPatchBufferFirst as u32 + active_frame_index,

            wireframe_pass: false.into(),
            display_normals: self.display_normals.into(),
        };

        let render_terrain = |vertex_pso: &ID3D12PipelineState| {
            if self.patches_to_render.is_empty() {
                return;
            }

            unsafe {
                cmd_list.SetPipelineState(vertex_pso);
                cmd_list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                cmd_list.IASetIndexBuffer(Some(&D3D12_INDEX_BUFFER_VIEW {
                    BufferLocation: self.patch_index_buffer.GetGPUVirtualAddress(),
                    SizeInBytes: PATCH_INDEX_COUNT * size_of::<u32>() as u32,
                    Format: DXGI_FORMAT_R32_UINT,
                }));

                cmd_list.DrawIndexedInstanced(PATCH_INDEX_COUNT, self.patches_to_render.len() as u32, 0, 0, 0);
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

    fn world_to_terrain_pos(&self, world_pos: Vec3) -> Vec2 {
        let world_scale = self.terrain_to_world_scale.max(0.0001);

        Vec2::new(world_pos.x / world_scale, world_pos.z / world_scale)
    }

    fn collect_generated_patches(&mut self) {
        for generated in self.patch_generator.drain_generated() {
            self.patch_cache.insert_generated(generated);
        }
    }

    fn write_gpu_patch_buffer(&self, active_frame_index: u32) {
        let is_neighbor_coarser = |node: &PatchKey, direction: IVec2| -> bool {
            let probe = node.terrain_center() + direction * node.terrain_size() as i32;

            let neighbor_lod_index = self
                .patches_to_render
                .iter()
                .find(|p| (p.terrain_center() - probe).length_squared() < node.terrain_size().pow(2) as i32)
                .map(|p| p.lod_index)
                .unwrap_or(node.lod_index);

            neighbor_lod_index > node.lod_index
        };

        let gpu_patches: Vec<_> = self
            .patches_to_render
            .iter()
            .map(|patch| {
                let directions = [
                    (PatchStitchMask::TOP, IVec2::NEG_Y),
                    (PatchStitchMask::BOTTOM, IVec2::Y),
                    (PatchStitchMask::LEFT, IVec2::NEG_X),
                    (PatchStitchMask::RIGHT, IVec2::X),
                ];

                let mut stitch_mask = PatchStitchMask::empty();

                for &(flag, direction) in &directions {
                    if is_neighbor_coarser(patch, direction) {
                        stitch_mask.insert(flag);
                    }
                }

                GpuTerrainPatch {
                    grid_index: patch.grid_index,
                    lod_index: patch.lod_index,
                    atlas_slot: self
                        .patch_cache
                        .atlas_slot(patch)
                        .unwrap_or_else(|| panic!("renderable patch {patch:?} must be resident in the cache")),
                    stitch_mask,
                }
            })
            .collect();

        unsafe {
            std::ptr::copy_nonoverlapping(
                gpu_patches.as_ptr(),
                self.patch_buffer_ptr
                    .add((active_frame_index * self.patch_buffer_item_count) as usize),
                gpu_patches.len(),
            );
        }
    }
}
