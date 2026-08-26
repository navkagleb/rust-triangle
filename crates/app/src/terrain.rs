mod config;
mod patch;
mod patch_gen_pool;
mod patch_quad_tree;
mod texture_atlas;

use std::collections::HashMap;
use std::ptr::null_mut;

use anyhow::Result;
use glam::{IVec2, Mat4, UVec2, Vec2, Vec3, Vec3Swizzles, f32};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::camera::Camera;
use crate::d3d12_utils::*;
use crate::{BACK_BUFFER_FORMAT, DEPTH_BUFFER_FORMAT, FRAME_COUNT, GpuResource, imgui_text};
use config::*;
use imgui_sys::*;
use patch::*;
use patch_gen_pool::*;
use patch_quad_tree::*;
use texture_atlas::*;

#[repr(C)]
struct GpuTerrainPatch {
    grid_index: IVec2,
    lod_index: u32,
    stitch_mask: PatchStitchMask,
}

#[repr(C)]
struct GpuTerrainConsts {
    world_to_clip: Mat4,
    camera_grid_index: IVec2,
    terrain_to_world_scale: f32,
    terrain_height_scale: f32,
    elapsed_time: f32,
    stitching_enabled: u32,
    active_patch_buffer_index: u32,

    // Debug
    wireframe_pass: u32,
    display_normals: u32,
}

pub struct TerrainData {
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
    camera_terrain_pos: Vec3,
    camera_grid_index: IVec2,

    patch_cache: HashMap<PatchKey, PatchState>,
    patch_gen_pool: PatchGenPool,
    atlas_free_slots: Vec<UVec2>,
    patches_to_render: Vec<PatchKey>,

    patch_index_buffer: ID3D12Resource,
    #[allow(unused)]
    patch_buffer: ID3D12Resource,
    patch_buffer_item_count: u32,
    patch_buffer_ptr: *mut GpuTerrainPatch,

    indirection_texture: ID3D12Resource,
    indirection_texture_upload: ID3D12Resource,
    indirection_texture_ptr: *mut UVec2,

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

impl TerrainData {
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

        let indirection_format = DXGI_FORMAT_R32G32_UINT;
        let indirection_texture = ID3D12Resource::new_texture_2d(
            device,
            indirection_format,
            INDIRECTION_SLOT_COUNT,
            INDIRECTION_SLOT_COUNT,
            PATCH_LOD_COUNT,
        )?;
        let indirection_texture_upload = ID3D12Resource::new_buffer(
            device,
            D3D12_HEAP_TYPE_UPLOAD,
            indirection_texture.size()? * FRAME_COUNT as u64,
        )?;

        indirection_texture.set_debug_name("TerrainIndirection")?;
        indirection_texture_upload.set_debug_name("TerrainIndirectionUpload")?;

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
                resource_heap.get_cpu_handle(GpuResource::TerrainIndirectionTexture as u32),
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
            camera_terrain_pos: Vec3::ZERO,
            camera_grid_index: IVec2::ZERO,

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
            patches_to_render: Vec::new(),

            patch_index_buffer,
            patch_buffer_item_count: max_patch_count,
            patch_buffer_ptr: patch_buffer.map::<GpuTerrainPatch>()?,
            patch_buffer,

            indirection_texture_ptr: indirection_texture_upload.map::<UVec2>()?,
            indirection_texture_upload,
            indirection_texture,

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

    pub fn update_camera_pos(&mut self, camera_world_pos: &Vec3, dt: f32) {
        if !self.freeze_camera {
            self.camera_terrain_pos = self.world_to_terrain_pos(*camera_world_pos);
            self.camera_grid_index = self.camera_terrain_pos.xz().as_ivec2() / PATCH_TERRAIN_SIZE as i32;
        }

        if !self.pause_sun_animation {
            self.terrain_elapsed_time += dt;
        }
    }

    pub fn traverse_qtree(&mut self, active_frame_index: u32) -> Result<()> {
        let qtree = PatchQuadTreeBuilder::new(&self.camera_terrain_pos, self.render_distance, self.lod_factor).build();

        let mut patches_to_render = Vec::new();
        let mut patches_to_request = Vec::new();
        let mut nodes_to_traverse = std::collections::VecDeque::from([&qtree.root]);

        let is_resident = |key| {
            self.patch_cache
                .get(key)
                .is_some_and(|status| matches!(status, PatchState::Resident { .. }))
        };

        while let Some(node) = nodes_to_traverse.pop_front() {
            let is_node_renderable = node.key.lod_index < PATCH_LOD_COUNT;

            if let Some(children) = node.children.as_deref() {
                let children_ready = children.iter().all(|child| is_resident(&child.key));

                if !is_node_renderable || children_ready {
                    nodes_to_traverse.extend(children.iter());
                    continue;
                }

                patches_to_request.extend(
                    children
                        .iter()
                        .filter(|child| !self.patch_cache.contains_key(&child.key))
                        .map(|child| child.key),
                );
            };

            if !is_node_renderable {
                continue;
            }

            if is_resident(&node.key) {
                patches_to_render.push(node.key);
            } else if !self.patch_cache.contains_key(&node.key) {
                patches_to_request.push(node.key);
            }
        }

        self.patch_cache.retain(|key, state| {
            let grid_index = key.grid_index;
            if grid_index.cmpge(qtree.min_grid_index).all() && grid_index.cmple(qtree.max_grid_index).all() {
                return true;
            }

            match state {
                PatchState::GpuUploadPending { atlas_slot, .. } | PatchState::Resident { atlas_slot } => {
                    self.atlas_free_slots.push(*atlas_slot);
                }
                _ => {}
            }

            false
        });

        self.patches_to_render = patches_to_render;

        patches_to_request.sort_unstable_by(|a, b| {
            let distance_a = (self.camera_terrain_pos - a.terrain_center().extend(0).xzy().as_vec3()).length_squared();
            let distance_b = (self.camera_terrain_pos - b.terrain_center().extend(0).xzy().as_vec3()).length_squared();

            distance_a.total_cmp(&distance_b)
        });

        for result in self.patch_gen_pool.drain_results() {
            self.patch_cache.insert(
                result.patch,
                PatchState::CpuGenerated {
                    heights: result.heights,
                    gradients: result.gradients,
                },
            );
        }

        for patch in patches_to_request {
            self.patch_gen_pool.request_patch_generation(patch);
            self.patch_cache.insert(patch, PatchState::GenerationQueued);
        }

        let is_neighbor_coarser = |node: &PatchKey, direction: IVec2| -> bool {
            let probe = node.terrain_center() + direction * node.terrain_size() as i32;

            let neighbor_lod_index = self
                .patches_to_render
                .iter()
                .find(|l| (l.terrain_center() - probe).length_squared() < node.terrain_size().pow(2) as i32)
                .map(|l| l.lod_index)
                .unwrap_or(node.lod_index);

            neighbor_lod_index > node.lod_index
        };

        let gpu_patches: Vec<_> = self
            .patches_to_render
            .iter()
            .map(|l| {
                let directions = [
                    (PatchStitchMask::TOP, IVec2::NEG_Y),
                    (PatchStitchMask::BOTTOM, IVec2::Y),
                    (PatchStitchMask::LEFT, IVec2::NEG_X),
                    (PatchStitchMask::RIGHT, IVec2::X),
                ];

                let mut stitch_mask = PatchStitchMask::empty();

                for &(flag, direction) in &directions {
                    if is_neighbor_coarser(l, direction) {
                        stitch_mask.insert(flag);
                    }
                }

                GpuTerrainPatch {
                    grid_index: l.grid_index,
                    lod_index: l.lod_index,
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

        Ok(())
    }

    pub fn upload_atlas_data(
        &mut self,
        cmd_list: &ID3D12GraphicsCommandList,
        cpu_frame_index: u64,
        gpu_frame_index: u64,
        active_frame_index: u32,
    ) {
        let mut patches_to_update = Vec::new();

        for (&key, state) in &self.patch_cache {
            if let PatchState::GpuUploadPending {
                atlas_slot,
                submitted_frame,
            } = state
                && *submitted_frame <= gpu_frame_index
            {
                patches_to_update.push((
                    key,
                    PatchState::Resident {
                        atlas_slot: *atlas_slot,
                    },
                ));
                continue;
            }

            let PatchState::CpuGenerated { heights, gradients } = state else {
                continue;
            };

            let atlas_slot = self.atlas_free_slots.pop().unwrap();

            self.height_atlas
                .copy_to(cmd_list, active_frame_index, atlas_slot, heights.as_slice());
            self.gradient_atlas
                .copy_to(cmd_list, active_frame_index, atlas_slot, gradients.as_slice());

            patches_to_update.push((
                key,
                PatchState::GpuUploadPending {
                    atlas_slot,
                    submitted_frame: cpu_frame_index,
                },
            ));
        }

        for (key, state) in patches_to_update {
            self.patch_cache.insert(key, state);
        }
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
            let PatchState::Resident { atlas_slot } = state else {
                continue;
            };

            let lod_index = key.lod_index;
            let slot_count = INDIRECTION_SLOT_COUNT >> lod_index;

            let relative_slot = (key.grid_index >> lod_index) - (self.camera_grid_index >> lod_index);
            let indirection_slot = relative_slot + slot_count as i32 / 2;

            let range = 0..slot_count as i32;
            if !range.contains(&indirection_slot.x) || !range.contains(&indirection_slot.y) {
                continue;
            }

            let flat_indirection_index = indirection_slot.y as u32 * slot_count + indirection_slot.x as u32;
            resident_patch_lods[lod_index as usize][flat_indirection_index as usize] = *atlas_slot;
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

        let upload_byte_offset = active_frame_index as u64 * self.indirection_texture.size()?;

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
                            .byte_add((upload_byte_offset + gpu_offset) as usize),
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
                                Offset: upload_byte_offset + gpu_lod_offset,
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

    pub fn render(&self, cmd_list: &ID3D12GraphicsCommandList, camera: &Camera, active_frame_index: u32) {
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

    pub fn render_imgui(&mut self) {
        unsafe {
            ImGui_Begin(c"Terrain".as_ptr(), null_mut(), 0);

            if ImGui_Button(c"Clear cache".as_ptr()) {
                self.patch_cache.clear();
            }

            ImGui_NewLine();
            ImGui_InputInt(c"Render distance".as_ptr(), &mut self.render_distance as *mut u32 as _);
            ImGui_InputFloat(c"LOD factor".as_ptr(), &mut self.lod_factor);
            ImGui_InputFloat(c"Terrain to world scale".as_ptr(), &mut self.terrain_to_world_scale);
            ImGui_InputFloat(c"Terrain height scale".as_ptr(), &mut self.terrain_height_scale);

            ImGui_NewLine();
            ImGui_Checkbox(c"Freeze camera".as_ptr(), &mut self.freeze_camera);
            ImGui_Checkbox(c"Solid mode".as_ptr(), &mut self.solid_mode);
            ImGui_Checkbox(c"Wireframe mode".as_ptr(), &mut self.wireframe_mode);
            ImGui_Checkbox(c"Display normals".as_ptr(), &mut self.display_normals);
            ImGui_Checkbox(c"Stitching".as_ptr(), &mut self.stitching_enabled);
            ImGui_Checkbox(c"Pause sun animation".as_ptr(), &mut self.pause_sun_animation);

            ImGui_NewLine();

            let render_count = (self.render_distance * 2) / PATCH_TERRAIN_SIZE;
            let mut requested_count = 0;
            let mut generated_count = 0;
            let mut uploading_count = 0;
            let mut resident_count = 0;

            for state in self.patch_cache.values() {
                match state {
                    PatchState::GenerationQueued => requested_count += 1,
                    PatchState::CpuGenerated { .. } => generated_count += 1,
                    PatchState::GpuUploadPending { .. } => uploading_count += 1,
                    PatchState::Resident { .. } => resident_count += 1,
                }
            }

            imgui_text!("Max render side patches: {}", render_count);
            imgui_text!("Max render squared patches: {}", render_count.pow(2));
            imgui_text!("Render (leafs): {}", self.patches_to_render.len());
            imgui_text!("Cached: {}", self.patch_cache.len());
            imgui_text!("Requested: {}", requested_count);
            imgui_text!("Generated: {}", generated_count);
            imgui_text!("Uploading: {}", uploading_count);
            imgui_text!("Resident: {}", resident_count);
            imgui_text!(
                "Atlas slots: {}/{}",
                self.atlas_free_slots.len(),
                ATLAS_PATCH_COUNT * ATLAS_PATCH_COUNT
            );

            ImGui_End();
        }
    }

    pub fn render_imgui_qtree(&mut self, camera_world_pos: &Vec3) {
        unsafe {
            ImGui_Begin(c"TerrainQuadTree".as_ptr(), null_mut(), 0);

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

            for patch in &self.patches_to_render {
                let minimap_leaf_pos = minimap_center + patch.terrain_origin().as_vec2() * minimap_scale;
                let minimap_leaf_size = patch.terrain_size() as f32 * minimap_scale;

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

                let label = std::ffi::CString::new(patch.lod_index.to_string()).unwrap();
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

            let minimap_camera_pos = minimap_center + self.world_to_terrain_pos(*camera_world_pos).xz() * minimap_scale;
            ImDrawList_AddCircleFilled(
                draw_list,
                ImVec2 {
                    x: minimap_camera_pos.x,
                    y: minimap_camera_pos.y,
                },
                5.0,
                0xFF0000FF,
                5,
            );

            let start = self
                .patches_to_render
                .iter()
                .map(|p| p.terrain_origin())
                .fold(IVec2::MAX, |acc, p| acc.min(p));
            let end = self
                .patches_to_render
                .iter()
                .map(|p| p.terrain_origin() + p.terrain_size() as i32)
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

    pub fn render_imgui_atlas(&self, resource_heap: &DescriptorHeap) {
        unsafe {
            ImGui_Begin(c"TerrainAtlas".as_ptr(), null_mut(), 0);

            let image_size = {
                let size = ImGui_GetContentRegionAvail();
                size.x.min(size.y)
            };

            ImGui_Image(
                ImTextureRef {
                    _TexData: std::ptr::null_mut(),
                    _TexID: resource_heap.get_gpu_handle(GpuResource::TerrainHeightAtlas as u32).ptr,
                },
                ImVec2 {
                    x: image_size,
                    y: image_size,
                },
            );

            ImGui_End();
        }
    }

    fn world_to_terrain_pos(&self, world_pos: Vec3) -> Vec3 {
        let world_scale = self.terrain_to_world_scale.max(0.0001);

        Vec3::new(world_pos.x / world_scale, world_pos.y, world_pos.z / world_scale)
    }
}
