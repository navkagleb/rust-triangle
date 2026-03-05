#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

include!(concat!(env!("OUT_DIR"), "/imgui_bindings.rs"));

// imgui backend functions are declared manually instead of using dear_bindings generated wrappers
// because dear_bindings backend generation is experimental and produces broken C++ code —
// types like ImDrawData* end up in the `cimgui` namespace which causes type mismatch errors
// when calling the original imgui backend functions that expect the plain ImDrawData* type.
// the backend API is small and stable enough that manual declarations are simpler and more reliable.

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;

unsafe extern "C" {
    pub unsafe fn cimgui_implwin32_init(hwnd: *mut std::ffi::c_void) -> bool;
    pub unsafe fn cimgui_implwin32_shutdown();
    pub unsafe fn cimgui_implwin32_new_frame();
}

#[repr(C)]
pub struct ImGui_ImplDX12_InitInfo {
    pub device: *mut ID3D12Device,
    pub command_queue: *mut ID3D12CommandQueue,
    pub num_frames_in_flight: i32,
    pub rtv_format: DXGI_FORMAT,
    pub dsv_format: DXGI_FORMAT,
    pub user_data: *mut std::ffi::c_void,
    pub srv_descriptor_heap: *mut ID3D12DescriptorHeap,
    pub srv_descriptor_alloc_fn: Option<
        unsafe extern "C" fn(
            info: *mut ImGui_ImplDX12_InitInfo,
            out_cpu: *mut D3D12_CPU_DESCRIPTOR_HANDLE,
            out_gpu: *mut D3D12_GPU_DESCRIPTOR_HANDLE,
        ),
    >,
    pub srv_descriptor_free_fn: Option<
        unsafe extern "C" fn(
            info: *mut ImGui_ImplDX12_InitInfo,
            cpu: D3D12_CPU_DESCRIPTOR_HANDLE,
            gpu: D3D12_GPU_DESCRIPTOR_HANDLE,
        ),
    >,
    pub legacy_srv_cpu: D3D12_CPU_DESCRIPTOR_HANDLE,
    pub legacy_srv_gpu: D3D12_GPU_DESCRIPTOR_HANDLE,
}

impl ImGui_ImplDX12_InitInfo {
    pub fn new() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

unsafe extern "C" {
    pub unsafe fn cimgui_impldx12_init(info: *mut ImGui_ImplDX12_InitInfo) -> bool;
    pub unsafe fn cimgui_impldx12_shutdown();
    pub unsafe fn cimgui_impldx12_new_frame();
    pub unsafe fn cimgui_impldx12_render_draw_data(
        draw_data: *mut ImDrawData,
        command_list: *mut ID3D12GraphicsCommandList,
    );
}
