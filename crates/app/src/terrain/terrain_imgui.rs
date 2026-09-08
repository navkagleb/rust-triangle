use std::ptr::null_mut;

use glam::{IVec2, Vec2, Vec3, Vec3Swizzles};
use imgui_sys::*;

use super::config::{PATCH_LOD_COUNT, PATCH_TERRAIN_SIZE};
use super::patch_cache::PatchCache;
use super::patch_quad_tree::PatchQuadTree;
use crate::d3d12_utils::DescriptorHeap;
use crate::terrain::Terrain;
use crate::{GpuResource, imgui_text};

impl Terrain {
    pub unsafe fn render_imgui(&mut self, descriptor_heap: &DescriptorHeap, camera_pos: &Vec3, camera_forward: &Vec3) {
        unsafe {
            ImGui_Begin(c"Terrain".as_ptr(), null_mut(), 0);

            if ImGui_Button(c"Clear cache".as_ptr()) {
                self.patch_cache = PatchCache::new();
            }

            ImGui_NewLine();
            ImGui_InputInt(c"Render distance".as_ptr(), &mut self.render_distance as *mut u32 as _);
            ImGui_InputFloat(c"LOD factor".as_ptr(), &mut self.lod_factor);
            ImGui_InputFloat(c"Height scale".as_ptr(), &mut self.height_scale);
            ImGui_SliderFloat(c"Morph start ratio".as_ptr(), &mut self.morph_start_ratio, 0.0, 1.0);

            ImGui_NewLine();
            ImGui_Checkbox(c"Freeze camera".as_ptr(), &mut self.freeze_camera);
            ImGui_Checkbox(c"Solid mode".as_ptr(), &mut self.solid_mode);
            ImGui_Checkbox(c"Wireframe mode".as_ptr(), &mut self.wireframe_mode);
            ImGui_Checkbox(c"Display normals".as_ptr(), &mut self.display_normals);
            ImGui_Checkbox(c"Pause sun animation".as_ptr(), &mut self.pause_sun_animation);

            ImGui_NewLine();

            let render_count = (self.render_distance * 2) / PATCH_TERRAIN_SIZE;
            imgui_text!("Max render side patches: {}", render_count);
            imgui_text!("Max render squared patches: {}", render_count.pow(2));

            imgui_text!("Patches to upload: {}", self.patches_to_upload.len());
            imgui_text!("Patches to render: {}", self.patches_to_render.len());

            ImGui_End();

            self.patch_cache
                .render_imgui(descriptor_heap.get_gpu_handle(GpuResource::TerrainHeightAtlas as u32));

            self.render_imgui_qtree(camera_pos, camera_forward)
        }
    }

    fn render_imgui_qtree(&mut self, camera_pos: &Vec3, camera_forward: &Vec3) {
        unsafe {
            ImGui_Begin(c"TerrainQuadTree".as_ptr(), null_mut(), 0);

            if ImGui_Button(c"Reset view".as_ptr()) {
                self.minimap_offset = Vec2::ZERO;
                self.minimap_zoom = 1.0;
            }

            ImGui_SameLine();
            ImGui_Checkbox(c"Morph range".as_ptr(), &mut self.minimap_display_morph_range);

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

            if self.minimap_display_morph_range {
                let minimap_freezed_camera_pos = minimap_center + self.camera_pos * minimap_scale;

                // lod_index's split_distance is the threshold at which an LOD lod_index patch splits
                // into LOD (lod_index - 1) children, so the circle bounds the LOD (lod_index - 1)
                // region and must be colored accordingly. LOD 0 never splits, so it has no threshold.
                for lod_index in 1..PATCH_LOD_COUNT {
                    let split_distance = PatchQuadTree::split_distance(lod_index, self.lod_factor);
                    let morph_start_distance = split_distance * self.morph_start_ratio;

                    let center = ImVec2 {
                        x: minimap_freezed_camera_pos.x,
                        y: minimap_freezed_camera_pos.y,
                    };
                    let color = get_lod_color(lod_index - 1);
                    let segment_count = 40;

                    // ImGui has no annulus primitive, so the morph band is a circle stroked at the
                    // middle of the band with a thickness equal to its width, which expands
                    // symmetrically to cover exactly morph_start_distance..split_distance.
                    ImDrawList_AddCircleEx(
                        draw_list,
                        center,
                        (morph_start_distance + split_distance) * 0.5 * minimap_scale,
                        im_color32(color, 0x30),
                        segment_count,
                        (split_distance - morph_start_distance) * minimap_scale,
                    );

                    ImDrawList_AddCircleEx(
                        draw_list,
                        center,
                        morph_start_distance * minimap_scale,
                        im_color32(color, 0x80),
                        segment_count,
                        1.0,
                    );

                    ImDrawList_AddCircleEx(
                        draw_list,
                        center,
                        split_distance * minimap_scale,
                        im_color32(color, 0xff),
                        segment_count,
                        2.0,
                    );
                }
            }

            ImGui_End();
        }
    }
}

fn get_lod_color(lod_index: u32) -> Vec3 {
    match lod_index % PATCH_LOD_COUNT {
        0 => Vec3::new(0.10, 0.80, 0.20), // green
        1 => Vec3::new(0.10, 0.45, 1.00), // blue
        2 => Vec3::new(1.00, 0.80, 0.10), // yellow
        3 => Vec3::new(1.00, 0.30, 0.10), // orange
        4 => Vec3::new(0.75, 0.20, 1.00), // purple
        5 => Vec3::new(0.10, 0.90, 0.90), // cyan
        _ => Vec3::ZERO,
    }
}

fn im_color32(color: Vec3, alpha: u8) -> u32 {
    let r = (color.x.clamp(0.0, 1.0) * 255.0) as u32;
    let g = (color.y.clamp(0.0, 1.0) * 255.0) as u32;
    let b = (color.z.clamp(0.0, 1.0) * 255.0) as u32;

    (u32::from(alpha) << 24) | (b << 16) | (g << 8) | r
}
