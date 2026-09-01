use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use glam::IVec2;

use super::config::{INDIRECTION_SLOT_COUNT, PATCH_LOD_COUNT};
use super::patch_cache::ResidentPatch;
use super::texture_atlas::AtlasSlot;
use crate::FRAME_COUNT;
use crate::d3d12_utils::{D3D12BufferExt, D3D12TextureExt, InterfaceExt, align_up};

pub struct PatchIndirectionTexture {
    texture: ID3D12Resource,
    upload_buffer: ID3D12Resource,
    upload_ptr: *mut AtlasSlot,

    data: [Vec<AtlasSlot>; PATCH_LOD_COUNT as usize],
    layouts: [D3D12_PLACED_SUBRESOURCE_FOOTPRINT; PATCH_LOD_COUNT as usize],
    frame_stride: u64,
}

impl PatchIndirectionTexture {
    pub fn new(device: &ID3D12Device, cpu_src: D3D12_CPU_DESCRIPTOR_HANDLE) -> anyhow::Result<Self> {
        assert_eq!(std::mem::size_of::<AtlasSlot>(), 8);

        let format = DXGI_FORMAT_R32G32_UINT;
        let texture = ID3D12Resource::new_texture_2d(
            device,
            format,
            INDIRECTION_SLOT_COUNT,
            INDIRECTION_SLOT_COUNT,
            PATCH_LOD_COUNT,
        )?;

        unsafe {
            device.CreateShaderResourceView(
                &texture,
                Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: format,
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
                cpu_src,
            );
        }

        let data = std::array::from_fn(|i| {
            let slot_count = INDIRECTION_SLOT_COUNT >> i;
            vec![AtlasSlot::invalid(); slot_count.pow(2) as usize]
        });

        let desc = unsafe { texture.GetDesc() };
        let mut layouts = [D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); PATCH_LOD_COUNT as usize];
        let mut size = 0;

        unsafe {
            device.GetCopyableFootprints(
                &desc,
                0,
                PATCH_LOD_COUNT,
                0,
                Some(layouts.as_mut_ptr()),
                None,
                None,
                Some(&mut size),
            );
        }

        let frame_stride = align_up(size, D3D12_TEXTURE_DATA_PLACEMENT_ALIGNMENT as u64);
        let upload_buffer =
            ID3D12Resource::new_buffer(device, D3D12_HEAP_TYPE_UPLOAD, frame_stride * FRAME_COUNT as u64)?;

        texture.set_debug_name("TerrainIndirectionTexture")?;
        upload_buffer.set_debug_name("TerrainIndirectionUploadBuffer")?;

        Ok(Self {
            texture,
            upload_ptr: upload_buffer.map::<AtlasSlot>()?,
            upload_buffer,
            data,
            layouts,
            frame_stride,
        })
    }

    pub fn rebuild(&mut self, camera_grid_index: IVec2, resident_pathes: &[ResidentPatch]) {
        for lod_data in &mut self.data {
            lod_data.fill(AtlasSlot::invalid());
        }

        for resident in resident_pathes {
            let lod_index = resident.patch.lod_index;
            let slot_count = INDIRECTION_SLOT_COUNT >> lod_index;

            let relative_slot = (resident.patch.grid_index >> lod_index) - (camera_grid_index >> lod_index);
            let indirection_slot = relative_slot + slot_count as i32 / 2;

            let range = 0..slot_count as i32;
            if !range.contains(&indirection_slot.x) || !range.contains(&indirection_slot.y) {
                continue;
            }

            let flat_index = indirection_slot.y as usize * slot_count as usize + indirection_slot.x as usize;
            self.data[lod_index as usize][flat_index] = resident.atlas_slot;
        }
    }

    pub fn upload(&self, cmd_list: &ID3D12GraphicsCommandList, active_frame_index: u32) {
        let upload_byte_offset = active_frame_index as u64 * self.frame_stride;

        for lod_index in 0..PATCH_LOD_COUNT {
            let slot_count = INDIRECTION_SLOT_COUNT >> lod_index;

            let gpu_layout = self.layouts[lod_index as usize];
            let gpu_row_pitch = gpu_layout.Footprint.RowPitch;
            let gpu_lod_offset = gpu_layout.Offset;

            for row_index in 0..slot_count {
                let cpu_offset = row_index * slot_count;
                let gpu_offset = gpu_lod_offset + (row_index * gpu_row_pitch) as u64;

                unsafe {
                    std::ptr::copy_nonoverlapping(
                        self.data[lod_index as usize].as_ptr().add(cpu_offset as usize),
                        self.upload_ptr.byte_add((upload_byte_offset + gpu_offset) as usize),
                        slot_count as usize,
                    );
                }
            }

            unsafe {
                cmd_list.CopyTextureRegion(
                    &D3D12_TEXTURE_COPY_LOCATION {
                        pResource: std::mem::transmute_copy(&self.texture),
                        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                            SubresourceIndex: lod_index,
                        },
                    },
                    0,
                    0,
                    0,
                    &D3D12_TEXTURE_COPY_LOCATION {
                        pResource: std::mem::transmute_copy(&self.upload_buffer),
                        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                            PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                                Offset: upload_byte_offset + gpu_lod_offset,
                                Footprint: self.layouts[lod_index as usize].Footprint,
                            },
                        },
                    },
                    None,
                );
            }
        }
    }
}
