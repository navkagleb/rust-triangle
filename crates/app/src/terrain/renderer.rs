use anyhow::Result;
use glam::{Vec2, Vec3, Vec3Swizzles, f32};
use imgui_sys::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use super::cache::{Cache, PatchUpload};
use super::config::*;
use super::generator::{Generator, WantedPatch};
use super::gpu_types::{GpuTerrainConsts, GpuTerrainPatch};
use super::quad_tree::{PatchSelection, QuadTree};
use super::texture_atlas::TextureAtlas;
use crate::camera::Camera;
use crate::d3d12_utils::*;
use crate::{BACK_BUFFER_FORMAT, DEPTH_BUFFER_FORMAT, FRAME_COUNT, GpuResource, imgui_text};

pub struct Terrain {
    lod_factor: f32,
    height_scale: f32,

    solid_mode: bool,
    wireframe_mode: bool,
    display_normals: bool,
    pause_sun_animation: bool,
    elapsed_time: f32,

    freeze_camera: bool,
    camera_pos: Vec3,
    camera_forward: Vec2,

    quad_tree: QuadTree,
    generator: Generator,
    cache: Cache,
    selection: PatchSelection,
    patches_to_upload: Vec<PatchUpload>,

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

        let max_patch_count = PATCH_COUNT_PER_SIDE; // TODO
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
            lod_factor: 2.0,

            height_scale: 5000.0,

            solid_mode: true,
            wireframe_mode: false,
            display_normals: false,
            pause_sun_animation: false,
            elapsed_time: 0.0,

            freeze_camera: false,
            camera_pos: Vec3::ZERO,
            camera_forward: Vec2::ZERO,

            quad_tree: QuadTree::new(),
            generator: Generator::new(),
            cache: Cache::new(),
            selection: PatchSelection::default(),
            patches_to_upload: Vec::new(),

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
            self.camera_pos = *camera_pos;
            self.camera_forward = camera_forward.xz().normalize_or_zero();
        }

        if !self.pause_sun_animation {
            self.elapsed_time += dt;
        }
    }

    pub fn update(&mut self, cpu_frame_index: u64, gpu_frame_index: u64, active_frame_index: u32) {
        self.collect_generated_patches();

        self.quad_tree.select(
            self.camera_pos,
            self.height_scale,
            self.lod_factor,
            &self.cache,
            &mut self.selection,
        );

        self.generator
            .update_wanted_patches(self.selection.missing.iter().map(|missing| {
                WantedPatch::new(
                    missing.coord,
                    missing.coverage_required,
                    self.camera_pos.xz(),
                    self.camera_forward,
                )
            }));

        self.patches_to_upload = self
            .cache
            .update(cpu_frame_index, gpu_frame_index, &self.selection.needed);

        self.write_gpu_patch_buffer(active_frame_index);
    }

    pub fn render(&self, cmd_list: &ID3D12GraphicsCommandList, camera: &Camera, active_frame_index: u32) {
        for upload in &self.patches_to_upload {
            self.height_atlas.copy_to(
                cmd_list,
                active_frame_index,
                upload.atlas_slot,
                upload.payload.heights.as_slice(),
            );

            self.gradient_atlas.copy_to(
                cmd_list,
                active_frame_index,
                upload.atlas_slot,
                upload.payload.gradients.as_slice(),
            );
        }

        let mut consts = GpuTerrainConsts {
            world_to_clip: camera.world_to_clip(),
            height_scale: self.height_scale,
            elapsed_time: self.elapsed_time,
            active_patch_buffer_index: GpuResource::TerrainPatchBufferFirst as u32 + active_frame_index,

            wireframe_pass: false.into(),
            display_normals: self.display_normals.into(),
        };

        let render_terrain = |vertex_pso: &ID3D12PipelineState| {
            if self.selection.renderable.is_empty() {
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

                cmd_list.DrawIndexedInstanced(PATCH_INDEX_COUNT, self.selection.renderable.len() as u32, 0, 0, 0);
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

    pub unsafe fn render_imgui(&mut self, descriptor_heap: &DescriptorHeap, camera_pos: &Vec3, camera_forward: &Vec3) {
        unsafe {
            ImGui_Begin(c"Terrain".as_ptr(), std::ptr::null_mut(), 0);

            if ImGui_Button(c"Clear cache".as_ptr()) {
                self.cache = Cache::new();
            }

            ImGui_NewLine();
            imgui_text!("World size: {}", WORLD_SIZE_IN_METERS);

            for lod in 0..PATCH_LOD_COUNT {
                let lod_size_in_meters = PATCH_SIZE_IN_METERS * (1 << lod);
                imgui_text!("LOD {} size: {}", lod, lod_size_in_meters);
            }

            ImGui_NewLine();
            ImGui_InputFloat(c"LOD factor".as_ptr(), &mut self.lod_factor);
            ImGui_InputFloat(c"Height scale".as_ptr(), &mut self.height_scale);

            ImGui_NewLine();
            ImGui_Checkbox(c"Freeze camera".as_ptr(), &mut self.freeze_camera);
            ImGui_Checkbox(c"Solid mode".as_ptr(), &mut self.solid_mode);
            ImGui_Checkbox(c"Wireframe mode".as_ptr(), &mut self.wireframe_mode);
            ImGui_Checkbox(c"Display normals".as_ptr(), &mut self.display_normals);
            ImGui_Checkbox(c"Pause sun animation".as_ptr(), &mut self.pause_sun_animation);

            ImGui_NewLine();
            imgui_text!("Patches to upload: {}", self.patches_to_upload.len());
            imgui_text!("Patches to render: {}", self.selection.renderable.len());

            ImGui_End();

            self.cache
                .render_imgui(descriptor_heap.get_gpu_handle(GpuResource::TerrainHeightAtlas as u32));

            self.render_imgui_qtree(camera_pos, camera_forward)
        }
    }

    fn collect_generated_patches(&mut self) {
        for generated in self.generator.drain_generated() {
            self.quad_tree
                .set_height_range(&generated.coord, generated.height_range);
            self.cache.insert_generated(generated);
        }
    }

    fn write_gpu_patch_buffer(&self, active_frame_index: u32) {
        let gpu_patches: Vec<_> = self
            .selection
            .renderable
            .iter()
            .map(|renderable| GpuTerrainPatch {
                atlas_slot: renderable.atlas_slot,
                x: renderable.coord.x,
                z: renderable.coord.z,
                lod: renderable.coord.lod,
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

    fn render_imgui_qtree(&mut self, camera_pos: &Vec3, camera_forward: &Vec3) {
        unsafe {
            ImGui_Begin(c"TerrainQuadTree".as_ptr(), std::ptr::null_mut(), 0);

            if ImGui_Button(c"Reset view".as_ptr()) {
                self.minimap_offset = Vec2::ZERO;
                self.minimap_zoom = 1.0;
            }

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
                    self.minimap_zoom = (self.minimap_zoom * (1.0 + scroll * 0.1)).clamp(1.0, 100.0);

                    let zoom_factor = self.minimap_zoom / prev_zoom;
                    self.minimap_offset += mouse_relative_pos - mouse_relative_pos * zoom_factor;
                }
            }

            let max_minimap_offset = Vec2::splat(minimap_size * (self.minimap_zoom - 1.0) * 0.5);
            self.minimap_offset = self.minimap_offset.clamp(-max_minimap_offset, max_minimap_offset);

            let minimap_center = minimap_pos + minimap_size * 0.5 + self.minimap_offset;
            let minimap_scale = minimap_size / WORLD_SIZE_IN_METERS as f32 * self.minimap_zoom;

            let draw_list = ImGui_GetWindowDrawList();

            for renderable in &self.selection.renderable {
                let coord = &renderable.coord;

                let minimap_leaf_pos = minimap_center + coord.world_origin() * minimap_scale;
                let minimap_leaf_size = coord.size_in_meters() as f32 * minimap_scale;

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

                let label = std::ffi::CString::new(coord.lod.to_string()).unwrap();
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

            let camera_color = 0xFF0000FF_u32;
            let minimap_camera_pos = minimap_center + camera_pos.xz() * minimap_scale;
            let minimap_camera_forward_pos = minimap_camera_pos + camera_forward.xz().normalize() * 100.0;

            ImDrawList_AddCircleFilled(
                draw_list,
                ImVec2 {
                    x: minimap_camera_pos.x,
                    y: minimap_camera_pos.y,
                },
                5.0,
                camera_color,
                5,
            );

            ImDrawList_AddLine(
                draw_list,
                ImVec2 {
                    x: minimap_camera_pos.x,
                    y: minimap_camera_pos.y,
                },
                ImVec2 {
                    x: minimap_camera_forward_pos.x,
                    y: minimap_camera_forward_pos.y,
                },
                camera_color,
            );

            let minimap_freezed_camera_pos = minimap_center + self.camera_pos.xz() * minimap_scale;

            for lod_index in 1..PATCH_LOD_COUNT {
                let split_distance = QuadTree::split_distance(lod_index, self.lod_factor);

                let center = ImVec2 {
                    x: minimap_freezed_camera_pos.x,
                    y: minimap_freezed_camera_pos.y,
                };
                let color = get_lod_color(lod_index - 1);
                let segment_count = 40;

                ImDrawList_AddCircleEx(
                    draw_list,
                    center,
                    split_distance * minimap_scale,
                    im_color32(color, 0xff),
                    segment_count,
                    2.0,
                );
            }

            ImGui_End();
        }
    }
}

/// Mirrors `get_lod_color` in `shaders/terrain.hlsl` - change both together, or the
/// minimap rings stop matching the patches they describe.
fn get_lod_color(lod_index: u32) -> Vec3 {
    match lod_index % PATCH_LOD_COUNT {
        0 => Vec3::new(0.10, 0.80, 0.20),  // green
        1 => Vec3::new(0.10, 0.45, 1.00),  // blue
        2 => Vec3::new(1.00, 0.80, 0.10),  // yellow
        3 => Vec3::new(1.00, 0.30, 0.10),  // orange
        4 => Vec3::new(0.75, 0.20, 1.00),  // purple
        5 => Vec3::new(0.10, 0.90, 0.90),  // cyan
        6 => Vec3::new(1.00, 0.40, 0.70),  // pink
        7 => Vec3::new(0.60, 0.60, 0.60),  // grey
        8 => Vec3::new(0.95, 0.15, 0.15),  // red
        9 => Vec3::new(0.00, 0.55, 0.50),  // teal
        10 => Vec3::new(0.85, 0.65, 0.40), // tan
        _ => Vec3::ZERO,
    }
}

fn im_color32(color: Vec3, alpha: u8) -> u32 {
    let r = (color.x.clamp(0.0, 1.0) * 255.0) as u32;
    let g = (color.y.clamp(0.0, 1.0) * 255.0) as u32;
    let b = (color.z.clamp(0.0, 1.0) * 255.0) as u32;

    (u32::from(alpha) << 24) | (b << 16) | (g << 8) | r
}
