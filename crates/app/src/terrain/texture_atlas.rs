use anyhow::Result;
use glam::UVec2;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use super::config::{ATLAS_PATCH_PIXEL_SIZE, ATLAS_SIZE};
use crate::FRAME_COUNT;
use crate::d3d12_utils::{D3D12BufferExt, D3D12TextureExt, InterfaceExt};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AtlasSlot(UVec2);

impl AtlasSlot {
    pub fn new(x: u32, y: u32) -> Self {
        Self(UVec2::new(x, y))
    }

    pub fn coords(&self) -> UVec2 {
        self.0
    }
}

pub struct TextureAtlas<T> {
    texture: ID3D12Resource,
    upload: ID3D12Resource,
    mapped_ptr: *mut T,
    format: DXGI_FORMAT,
    gpu_layout: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
    gpu_size: u64,
}

impl<T> TextureAtlas<T> {
    pub fn new(
        device: &ID3D12Device,
        cpu_srv: D3D12_CPU_DESCRIPTOR_HANDLE,
        format: DXGI_FORMAT,
        debug_name: &str,
    ) -> Result<TextureAtlas<T>> {
        let texture = ID3D12Resource::new_texture_2d(device, format, ATLAS_SIZE, ATLAS_SIZE, 1)?;

        let mut gpu_layout = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        let mut gpu_size = 0;
        unsafe {
            device.GetCopyableFootprints(
                &texture.GetDesc(),
                0,
                1,
                0,
                Some(&mut gpu_layout),
                None,
                None,
                Some(&mut gpu_size),
            );
        }

        let upload = ID3D12Resource::new_buffer(device, D3D12_HEAP_TYPE_UPLOAD, gpu_size * FRAME_COUNT as u64)?;

        texture.set_debug_name(debug_name)?;
        upload.set_debug_name(format!("{}Upload", debug_name).as_str())?;

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
                            MipLevels: 1,
                            PlaneSlice: 0,
                            ResourceMinLODClamp: 0.0,
                        },
                    },
                }),
                cpu_srv,
            );
        }

        Ok(Self {
            texture,
            mapped_ptr: upload.map::<T>()?,
            upload,
            format,
            gpu_layout,
            gpu_size,
        })
    }

    pub fn copy_to(&self, cmd_list: &ID3D12GraphicsCommandList, active_frame_index: u32, slot: AtlasSlot, data: &[T]) {
        let row_pitch = self.gpu_layout.Footprint.RowPitch as usize;
        let texel_size = size_of::<T>();

        let frame_offset = active_frame_index as usize * self.gpu_size as usize;
        let patch_offset = slot.coords().y as usize * ATLAS_PATCH_PIXEL_SIZE * row_pitch
            + slot.coords().x as usize * ATLAS_PATCH_PIXEL_SIZE * texel_size;
        let dst_patch_base = frame_offset + patch_offset;

        for row in 0..ATLAS_PATCH_PIXEL_SIZE {
            unsafe {
                let src = data.as_ptr().add(row * ATLAS_PATCH_PIXEL_SIZE);
                let dst = self.mapped_ptr.byte_add(dst_patch_base + row * row_pitch);

                std::ptr::copy_nonoverlapping(src, dst, ATLAS_PATCH_PIXEL_SIZE);
            }
        }

        unsafe {
            cmd_list.CopyTextureRegion(
                &D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&self.texture),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
                },
                slot.coords().x * ATLAS_PATCH_PIXEL_SIZE as u32,
                slot.coords().y * ATLAS_PATCH_PIXEL_SIZE as u32,
                0,
                &D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&self.upload),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                            Offset: dst_patch_base as u64,
                            Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                                Format: self.format,
                                Width: ATLAS_PATCH_PIXEL_SIZE as u32,
                                Height: ATLAS_PATCH_PIXEL_SIZE as u32,
                                Depth: 1,
                                RowPitch: row_pitch as u32,
                            },
                        },
                    },
                },
                None,
            );
        }
    }
}
