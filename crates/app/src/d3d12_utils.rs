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
    fn map<T>(&self) -> Result<*mut T>;
    fn unmap(&self, size: usize);

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

    fn map<T>(&self) -> Result<*mut T> {
        #[cfg(debug_assertions)]
        unsafe {
            let mut heap_props = std::mem::MaybeUninit::uninit();
            self.GetHeapProperties(Some(heap_props.as_mut_ptr()), None)?;
            assert_eq!(heap_props.assume_init_ref().Type, D3D12_HEAP_TYPE_UPLOAD);
        }

        let mut cpu_ptr = std::ptr::null_mut::<std::ffi::c_void>();
        unsafe { self.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }), Some(&mut cpu_ptr))? };

        Ok(cpu_ptr as *mut T)
    }

    fn unmap(&self, size: usize) {
        unsafe {
            self.Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: size }));
        }
    }

    fn map_and_write<T>(&self, items: &[T]) -> Result<()> {
        let cpu_ptr = self.map::<T>()?;

        unsafe {
            std::ptr::copy_nonoverlapping(items.as_ptr(), cpu_ptr, items.len());
        }

        self.unmap(size_of_val(items));

        Ok(())
    }
}

pub trait D3D12TextureExt {
    fn new_texture_2d(
        device: &ID3D12Device,
        format: DXGI_FORMAT,
        width: u32,
        height: u32,
        mip_count: u32,
    ) -> Result<ID3D12Resource>;
}

impl D3D12TextureExt for ID3D12Resource {
    fn new_texture_2d(
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

        Ok(Self {
            cpu_ptr: resource.map::<u8>()?,
            resource,
            phantom_data: std::marker::PhantomData,
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
        self.resource.unmap(Self::buffer_size());
    }
}

pub struct DescriptorHeap {
    heap: ID3D12DescriptorHeap,
    descriptor_size: u32,
}

impl DescriptorHeap {
    pub fn new(device: &ID3D12Device, heap_type: D3D12_DESCRIPTOR_HEAP_TYPE, descriptor_count: u32) -> Result<Self> {
        let heap = unsafe {
            device.CreateDescriptorHeap::<ID3D12DescriptorHeap>(&D3D12_DESCRIPTOR_HEAP_DESC {
                NumDescriptors: descriptor_count,
                Type: heap_type,
                Flags: if heap_type == D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV {
                    D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE
                } else {
                    D3D12_DESCRIPTOR_HEAP_FLAG_NONE
                },
                NodeMask: 0,
            })?
        };

        let descriptor_size = unsafe { device.GetDescriptorHandleIncrementSize(heap_type) };

        Ok(Self { heap, descriptor_size })
    }

    pub fn d3d12(&self) -> &ID3D12DescriptorHeap {
        &self.heap
    }

    pub fn get_cpu_handle(&self, index: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: unsafe { self.heap.GetCPUDescriptorHandleForHeapStart().ptr }
                + (index * self.descriptor_size) as usize,
        }
    }

    pub fn get_gpu_handle(&self, index: u32) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        D3D12_GPU_DESCRIPTOR_HANDLE {
            ptr: unsafe { self.heap.GetGPUDescriptorHandleForHeapStart().ptr } + (index * self.descriptor_size) as u64,
        }
    }
}

pub trait ShaderBytecodeExt {
    fn from_slice(blob: &[u8]) -> Self;
}

impl ShaderBytecodeExt for D3D12_SHADER_BYTECODE {
    fn from_slice(blob: &[u8]) -> Self {
        Self {
            pShaderBytecode: blob.as_ptr() as _,
            BytecodeLength: blob.len(),
        }
    }
}

#[allow(unused)]
#[repr(C)]
pub struct MeshPipelineStream {
    pub root_signature: PsoSubobject<*mut std::ffi::c_void, { D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_ROOT_SIGNATURE.0 }>,
    pub ms: PsoSubobject<D3D12_SHADER_BYTECODE, { D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_MS.0 }>,
    pub ps: PsoSubobject<D3D12_SHADER_BYTECODE, { D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PS.0 }>,
    pub rasterizer: PsoSubobject<D3D12_RASTERIZER_DESC, { D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RASTERIZER.0 }>,
    pub depth_stencil: PsoSubobject<D3D12_DEPTH_STENCIL_DESC, { D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL.0 }>,
    pub rtv_formats:
        PsoSubobject<D3D12_RT_FORMAT_ARRAY, { D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RENDER_TARGET_FORMATS.0 }>,
    pub dsv_format: PsoSubobject<DXGI_FORMAT, { D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL_FORMAT.0 }>,
    pub sample_desc: PsoSubobject<DXGI_SAMPLE_DESC, { D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_DESC.0 }>,
}

#[repr(C, align(8))]
pub struct PsoSubobject<T, const TYPE: i32> {
    subobject_type: D3D12_PIPELINE_STATE_SUBOBJECT_TYPE,
    value: T,
}

#[allow(unused)]
impl<T, const TYPE: i32> PsoSubobject<T, TYPE> {
    pub fn new(value: T) -> Self {
        Self {
            subobject_type: D3D12_PIPELINE_STATE_SUBOBJECT_TYPE(TYPE),
            value,
        }
    }
}
