use anyhow::Result;
use windows::Win32::Foundation::{HANDLE, WAIT_FAILED};
use windows::Win32::Graphics::Direct3D::WKPDID_D3DDebugObjectName;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
use windows::core::{Error, Interface, PCWSTR};

use crate::FRAME_COUNT;

pub fn wait_for_gpu(fence: &ID3D12Fence, wait_event_handle: HANDLE, wait_value: u64) -> Result<()> {
    unsafe {
        fence.SetEventOnCompletion(wait_value, wait_event_handle)?;

        if WaitForSingleObject(wait_event_handle, INFINITE) == WAIT_FAILED {
            anyhow::bail!(windows::core::Error::from_thread());
        }
    }

    Ok(())
}

pub trait InterfaceExt {
    fn set_debug_name(&self, name: &str) -> Result<()>;
}

impl<T: Interface> InterfaceExt for T {
    fn set_debug_name(&self, name: &str) -> Result<()> {
        let wide = name.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let native_wide = PCWSTR::from_raw(wide.as_ptr());

        unsafe {
            if let Ok(d3d12_object) = self.cast::<ID3D12Object>() {
                d3d12_object.SetName(native_wide)?;
            }

            if let Ok(dxgi_object) = self.cast::<IDXGIObject>() {
                dxgi_object.SetPrivateData(
                    &WKPDID_D3DDebugObjectName,
                    native_wide.len() as u32,
                    native_wide.as_ptr() as *const _,
                )?;
            }
        }

        Ok(())
    }
}

pub trait D3D12HeapPropertiesExt {
    fn from_heap_type(heap_type: D3D12_HEAP_TYPE) -> Self;
}

impl D3D12HeapPropertiesExt for D3D12_HEAP_PROPERTIES {
    fn from_heap_type(heap_type: D3D12_HEAP_TYPE) -> Self {
        Self {
            Type: heap_type,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        }
    }
}

pub trait D3D12ResourceBarrierExt {
    fn new_transition(
        resource: &ID3D12Resource,
        state_before: D3D12_RESOURCE_STATES,
        state_after: D3D12_RESOURCE_STATES,
    ) -> Self;
}

impl D3D12ResourceBarrierExt for D3D12_RESOURCE_BARRIER {
    fn new_transition(
        resource: &ID3D12Resource,
        state_before: D3D12_RESOURCE_STATES,
        state_after: D3D12_RESOURCE_STATES,
    ) -> Self {
        Self {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: unsafe { std::mem::transmute_copy(resource) },
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: state_before,
                    StateAfter: state_after,
                }),
            },
        }
    }
}

pub trait D3D12BufferExt {
    fn new_buffer(device: &ID3D12Device, heap_type: D3D12_HEAP_TYPE, size: usize) -> Result<ID3D12Resource>;

    fn map_and_write<T>(&self, items: &[T]) -> Result<()>;
}

impl D3D12BufferExt for ID3D12Resource {
    fn new_buffer(device: &ID3D12Device, heap_type: D3D12_HEAP_TYPE, size: usize) -> Result<ID3D12Resource> {
        let mut buffer = None;
        unsafe {
            device.CreateCommittedResource::<ID3D12Resource>(
                &D3D12_HEAP_PROPERTIES::from_heap_type(heap_type),
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Alignment: 0,
                    Width: size as u64,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: DXGI_FORMAT_UNKNOWN,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    Flags: D3D12_RESOURCE_FLAG_NONE,
                },
                if heap_type == D3D12_HEAP_TYPE_UPLOAD {
                    D3D12_RESOURCE_STATE_GENERIC_READ
                } else {
                    D3D12_RESOURCE_STATE_COMMON
                },
                None,
                &mut buffer,
            )?;
        }

        buffer.ok_or(Error::from_thread().into())
    }

    fn map_and_write<T>(&self, items: &[T]) -> Result<()> {
        unsafe {
            let mut heap_props = std::mem::MaybeUninit::uninit();
            self.GetHeapProperties(Some(heap_props.as_mut_ptr()), None)?;
            assert_eq!(heap_props.assume_init_ref().Type, D3D12_HEAP_TYPE_UPLOAD);
        }

        let cpu_ptr = {
            let mut cpu_ptr = std::ptr::null_mut::<std::ffi::c_void>();
            unsafe { self.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }), Some(&mut cpu_ptr))? };

            cpu_ptr as *mut u8
        };

        unsafe {
            std::ptr::copy_nonoverlapping(items.as_ptr(), cpu_ptr as *mut T, items.len());
        }

        unsafe {
            self.Unmap(
                0,
                Some(&D3D12_RANGE {
                    Begin: 0,
                    End: size_of_val(items),
                }),
            );
        }

        Ok(())
    }
}

pub trait D3D12TextureExt {
    fn new_texture(
        device: &ID3D12Device,
        format: DXGI_FORMAT,
        width: u32,
        height: u32,
        mip_count: u32,
    ) -> Result<ID3D12Resource>;
}

impl D3D12TextureExt for ID3D12Resource {
    fn new_texture(
        device: &ID3D12Device,
        format: DXGI_FORMAT,
        width: u32,
        height: u32,
        mip_count: u32,
    ) -> Result<ID3D12Resource> {
        assert_ne!(format, DXGI_FORMAT_UNKNOWN);

        let mut texture = None;
        unsafe {
            device.CreateCommittedResource::<ID3D12Resource>(
                &D3D12_HEAP_PROPERTIES::from_heap_type(D3D12_HEAP_TYPE_DEFAULT),
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Alignment: 0,
                    Width: width as u64,
                    Height: height,
                    DepthOrArraySize: 1,
                    MipLevels: mip_count as u16,
                    Format: format,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                    Flags: D3D12_RESOURCE_FLAG_NONE,
                },
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut texture,
            )?
        }

        texture.ok_or(Error::from_thread().into())
    }
}

pub struct ConstBuffer<T> {
    resource: ID3D12Resource,
    phantom_data: std::marker::PhantomData<T>,
    cpu_ptr: *mut u8,
}

impl<T> ConstBuffer<T> {
    pub fn new(device: &ID3D12Device) -> Result<Self> {
        let resource = ID3D12Resource::new_buffer(device, D3D12_HEAP_TYPE_UPLOAD, Self::buffer_size())?;

        let mut cpu_ptr = std::ptr::null_mut::<std::ffi::c_void>();
        unsafe { resource.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }), Some(&mut cpu_ptr))? };

        Ok(Self {
            resource,
            phantom_data: std::marker::PhantomData,
            cpu_ptr: cpu_ptr as *mut u8,
        })
    }

    pub fn write(&self, frame_index: u32, item: &T) -> u64 {
        assert!(frame_index < FRAME_COUNT);

        let offset = frame_index * Self::aligned_item_size() as u32;

        unsafe {
            std::ptr::copy_nonoverlapping(
                item as *const T as _,
                self.cpu_ptr.add(offset as usize),
                size_of_val(item),
            );

            self.resource.GetGPUVirtualAddress() + offset as u64
        }
    }

    fn aligned_item_size() -> usize {
        size_of::<T>().next_multiple_of(D3D12_CONSTANT_BUFFER_DATA_PLACEMENT_ALIGNMENT as usize)
    }

    fn buffer_size() -> usize {
        Self::aligned_item_size() * FRAME_COUNT as usize
    }
}

impl<T> Drop for ConstBuffer<T> {
    fn drop(&mut self) {
        unsafe {
            self.resource.Unmap(
                0,
                Some(&D3D12_RANGE {
                    Begin: 0,
                    End: Self::buffer_size(),
                }),
            );
        }
    }
}

pub trait D3D12GraphicsCommandListExt {
    #[allow(dead_code)]
    fn upload_top_mip(&self, device: &ID3D12Device, data: &[u8], texture: &ID3D12Resource) -> Result<ID3D12Resource>;
    fn upload_mips(&self, device: &ID3D12Device, mips: &[&[u8]], texture: &ID3D12Resource) -> Result<ID3D12Resource>;
}

impl D3D12GraphicsCommandListExt for ID3D12GraphicsCommandList {
    fn upload_top_mip(&self, device: &ID3D12Device, data: &[u8], texture: &ID3D12Resource) -> Result<ID3D12Resource> {
        let mut layout = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        let mut row_count = 0;
        let mut row_size = 0;
        let mut texture_size = 0;
        unsafe {
            device.GetCopyableFootprints(
                &texture.GetDesc(),
                0,
                1,
                0,
                Some(&mut layout),
                Some(&mut row_count),
                Some(&mut row_size),
                Some(&mut texture_size),
            )
        };

        let upload_buffer = ID3D12Resource::new_buffer(device, D3D12_HEAP_TYPE_UPLOAD, texture_size as usize)?;
        let upload_cpu_ptr = {
            let mut cpu_ptr = std::ptr::null_mut::<std::ffi::c_void>();
            unsafe { upload_buffer.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }), Some(&mut cpu_ptr))? };

            cpu_ptr as *mut u8
        };

        for row_index in 0..row_count {
            let data_row_offset = row_size as usize * row_index as usize;
            let upload_row_offset = (layout.Footprint.RowPitch * row_index) as usize;

            unsafe {
                std::ptr::copy_nonoverlapping(
                    data.as_ptr().add(data_row_offset),
                    upload_cpu_ptr.add(upload_row_offset),
                    row_size as usize,
                );
            }
        }

        unsafe {
            upload_buffer.Unmap(
                0,
                Some(&D3D12_RANGE {
                    Begin: 0,
                    End: texture_size as usize,
                }),
            );
        }

        let dst_location = D3D12_TEXTURE_COPY_LOCATION {
            pResource: unsafe { std::mem::transmute_copy(texture) },
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
        };

        let src_location = D3D12_TEXTURE_COPY_LOCATION {
            pResource: unsafe { std::mem::transmute_copy(&upload_buffer) },
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: layout,
            },
        };

        unsafe { self.CopyTextureRegion(&dst_location, 0, 0, 0, &src_location, None) };

        Ok(upload_buffer)
    }

    fn upload_mips(&self, device: &ID3D12Device, mips: &[&[u8]], texture: &ID3D12Resource) -> Result<ID3D12Resource> {
        let subresource_count = mips.len();

        let mut layouts = vec![D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); subresource_count];
        let mut row_counts = vec![0; subresource_count];
        let mut row_sizes = vec![0; subresource_count];
        let mut texture_size = 0;
        unsafe {
            let desc = texture.GetDesc();

            device.GetCopyableFootprints(
                &desc,
                0,
                desc.MipLevels as u32,
                0,
                Some(layouts.as_mut_ptr()),
                Some(row_counts.as_mut_ptr()),
                Some(row_sizes.as_mut_ptr()),
                Some(&mut texture_size),
            )
        };

        let upload_buffer = ID3D12Resource::new_buffer(device, D3D12_HEAP_TYPE_UPLOAD, texture_size as usize)?;
        let upload_cpu_ptr = {
            let mut cpu_ptr = std::ptr::null_mut::<std::ffi::c_void>();
            unsafe { upload_buffer.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }), Some(&mut cpu_ptr))? };

            cpu_ptr as *mut u8
        };

        for si in 0..subresource_count {
            let layout = layouts[si];
            let row_count = row_counts[si];
            let row_size = row_sizes[si];
            let mip = mips[si];

            for ri in 0..row_count {
                let cpu_offset = row_size as usize * ri as usize;
                let gpu_offset = (layout.Offset + (layout.Footprint.RowPitch * ri) as u64) as usize;

                unsafe {
                    std::ptr::copy_nonoverlapping(
                        mip.as_ptr().add(cpu_offset),
                        upload_cpu_ptr.add(gpu_offset),
                        row_size as usize,
                    );
                }
            }
        }

        unsafe {
            upload_buffer.Unmap(
                0,
                Some(&D3D12_RANGE {
                    Begin: 0,
                    End: texture_size as usize,
                }),
            );
        }

        for (i, &layout) in layouts.iter().enumerate() {
            let dst_location = D3D12_TEXTURE_COPY_LOCATION {
                pResource: unsafe { std::mem::transmute_copy(texture) },
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: i as u32,
                },
            };

            let src_location = D3D12_TEXTURE_COPY_LOCATION {
                pResource: unsafe { std::mem::transmute_copy(&upload_buffer) },
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: layout,
                },
            };

            unsafe { self.CopyTextureRegion(&dst_location, 0, 0, 0, &src_location, None) };
        }

        Ok(upload_buffer)
    }
}

pub trait D3D12DescriptorHeapExt {
    fn get_cpu_handle(&self, device: &ID3D12Device, index: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE;
    fn get_gpu_handle(&self, device: &ID3D12Device, index: u32) -> D3D12_GPU_DESCRIPTOR_HANDLE;
}

impl D3D12DescriptorHeapExt for ID3D12DescriptorHeap {
    fn get_cpu_handle(&self, device: &ID3D12Device, index: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        let view_size = unsafe { device.GetDescriptorHandleIncrementSize(self.GetDesc().Type) };

        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: unsafe { self.GetCPUDescriptorHandleForHeapStart().ptr } + (index * view_size) as usize,
        }
    }

    fn get_gpu_handle(&self, device: &ID3D12Device, index: u32) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        let view_size = unsafe { device.GetDescriptorHandleIncrementSize(self.GetDesc().Type) };

        D3D12_GPU_DESCRIPTOR_HANDLE {
            ptr: unsafe { self.GetGPUDescriptorHandleForHeapStart().ptr } + (index * view_size) as u64,
        }
    }
}
