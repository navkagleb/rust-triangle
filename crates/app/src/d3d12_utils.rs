use anyhow::Result;
use windows::Win32::Foundation::{HANDLE, WAIT_FAILED};
use windows::Win32::Graphics::Direct3D::WKPDID_D3DDebugObjectName;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
use windows::core::{Interface, PCSTR, PCWSTR};

pub fn wait_for_gpu(fence: &ID3D12Fence, wait_event_handle: HANDLE, wait_value: u64) -> Result<()> {
    unsafe {
        fence.SetEventOnCompletion(wait_value, wait_event_handle)?;

        if WaitForSingleObject(wait_event_handle, INFINITE) == WAIT_FAILED {
            anyhow::bail!(windows::core::Error::from_thread());
        }
    }

    Ok(())
}

pub extern "system" fn d3d12_message_callback(
    _category: D3D12_MESSAGE_CATEGORY,
    severity: D3D12_MESSAGE_SEVERITY,
    _id: D3D12_MESSAGE_ID,
    description: PCSTR,
    _context: *mut std::ffi::c_void,
) {
    let severity_str = match severity {
        D3D12_MESSAGE_SEVERITY_CORRUPTION => "CORRUPTION",
        D3D12_MESSAGE_SEVERITY_ERROR => "ERROR",
        D3D12_MESSAGE_SEVERITY_WARNING => "WARNING",
        D3D12_MESSAGE_SEVERITY_INFO => "INFO",
        D3D12_MESSAGE_SEVERITY_MESSAGE => "MESSAGE",
        _ => "",
    };

    let message = unsafe { std::ffi::CStr::from_ptr(description.0 as _).to_string_lossy() };
    println!("[D3D12 {}]: {}", severity_str, message);
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

pub trait D3D12ResourceExt {
    fn new_buf(device: &ID3D12Device, heap_type: D3D12_HEAP_TYPE, size: usize) -> Result<ID3D12Resource>;
}

impl D3D12ResourceExt for ID3D12Resource {
    fn new_buf(device: &ID3D12Device, heap_type: D3D12_HEAP_TYPE, size: usize) -> Result<ID3D12Resource> {
        let mut buf: Option<ID3D12Resource> = None;
        unsafe {
            device.CreateCommittedResource(
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
                &mut buf,
            )?;
        }

        Ok(buf.unwrap())
    }
}
