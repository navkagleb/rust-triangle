mod camera;
mod d3d12_utils;
mod terrain;

use std::time::{Duration, Instant};

use anyhow::Result;
use glam::Vec3;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::Win32::System::Threading::CreateEventA;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Interface, PCSTR, s};

use d3d12_utils::*;
use imgui_sys::*;
use terrain::*;

const WINDOW_REGISTRY_NAME: PCSTR = s!("rust-window");
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FRAME_COUNT: u32 = 3;
const BACK_BUFFER_FORMAT: DXGI_FORMAT = DXGI_FORMAT_R8G8B8A8_UNORM;
const DEPTH_BUFFER_FORMAT: DXGI_FORMAT = DXGI_FORMAT_D32_FLOAT;

#[macro_export]
macro_rules! imgui_text {
    ($($arg:tt)*) => {
        ImGui_TextUnformatted(std::ffi::CString::new(format!($($arg)*)).unwrap().as_ptr())
    };
}

macro_rules! measure_ms {
    ($expression:expr) => {{
        let start = Instant::now();
        $expression?;
        let elapsed_ms = start.elapsed().as_secs_f32() * 1000.0;

        elapsed_ms
    }};
}

#[repr(u32)]
enum GpuResource {
    ImGuiFont,
    TerrainIndirectionTexture,
    TerrainHeightAtlas,
    TerrainPatchIndexBuffer,
    TerrainPatchBufferFirst,
    #[allow(unused)]
    TerrainPatchBufferLast = GpuResource::TerrainPatchBufferFirst as u32 + FRAME_COUNT,
    Count,
}

struct InputState {
    keys: [bool; 256],
    mouse_x: i32,
    mouse_y: i32,
    mouse_dx: i32,
    mouse_dy: i32,
    right_mouse_down: bool,
}

fn main() -> Result<()> {
    let mut camera = camera::Camera::new(Vec3::new(0.0, 100.0, 0.0));
    let mut camera_controller = camera::CameraController::default();

    let mut input = InputState {
        keys: [false; 256],
        mouse_x: 0,
        mouse_y: 0,
        mouse_dx: 0,
        mouse_dy: 0,
        right_mouse_down: false,
    };

    unsafe {
        let class_atom = RegisterClassA(&WNDCLASSA {
            style: CS_VREDRAW | CS_HREDRAW | CS_OWNDC,
            hInstance: GetModuleHandleA(None)?.into(),
            lpszClassName: WINDOW_REGISTRY_NAME,
            lpfnWndProc: Some(handle_window_message),
            ..Default::default()
        });

        if class_atom == 0 {
            GetLastError().ok()?;
        }

        let window_handle = {
            let mut window_rect = RECT {
                left: 0,
                top: 0,
                right: WIDTH as i32,
                bottom: HEIGHT as i32,
            };

            AdjustWindowRect(&mut window_rect, WS_OVERLAPPEDWINDOW, false)?;

            CreateWindowExA(
                WINDOW_EX_STYLE::default(),
                WINDOW_REGISTRY_NAME,
                s!("Hello Rust Triangle"),
                WS_OVERLAPPEDWINDOW,
                (GetSystemMetrics(SM_CXSCREEN) - window_rect.right) / 2,
                (GetSystemMetrics(SM_CYSCREEN) - window_rect.bottom) / 2,
                window_rect.right - window_rect.left,
                window_rect.bottom - window_rect.top,
                None,
                None,
                None,
                None,
            )?
        };

        SetWindowLongPtrA(window_handle, GWLP_USERDATA, &mut input as *mut InputState as isize);

        _ = ShowWindow(window_handle, SW_SHOW);
        _ = UpdateWindow(window_handle);

        let dxgi_factory = CreateDXGIFactory2::<IDXGIFactory2>(DXGI_CREATE_FACTORY_DEBUG)?.cast::<IDXGIFactory7>()?;

        let adapter = {
            let mut adapter_index = 0;
            let mut selected_adapter: Option<IDXGIAdapter1> = None;

            loop {
                match dxgi_factory
                    .EnumAdapterByGpuPreference::<IDXGIAdapter1>(adapter_index, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE)
                {
                    Ok(adapter) => {
                        let desc = adapter.GetDesc1()?;
                        println!("Adapter {}: {}", adapter_index, wide_to_string(&desc.Description));

                        selected_adapter.get_or_insert(adapter);
                        adapter_index += 1;
                    }
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(e.into()),
                }
            }

            selected_adapter.unwrap().cast::<IDXGIAdapter3>()?
        };

        if cfg!(debug_assertions) {
            let mut debug: Option<ID3D12Debug5> = None;
            D3D12GetDebugInterface(&mut debug)?;

            if let Some(debug) = debug {
                debug.EnableDebugLayer();
                println!("Enable D3D12 debug layer");

                // debug.SetEnableGPUBasedValidation(true);
                debug.SetEnableAutoName(true);
            }
        }

        let device = {
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device)?;

            let device = device.unwrap();
            device.set_debug_name("MainDevice")?;

            device.cast::<ID3D12Device4>()?
        };

        {
            let shader_model = D3D12_FEATURE_DATA_SHADER_MODEL {
                HighestShaderModel: D3D_SHADER_MODEL_6_6,
            };
            device.CheckFeatureSupport(
                D3D12_FEATURE_SHADER_MODEL,
                std::ptr::addr_of!(shader_model) as _,
                size_of::<D3D12_FEATURE_DATA_SHADER_MODEL>() as u32,
            )?;

            println!(
                "Supported shader model: {}.{}",
                shader_model.HighestShaderModel.0 / 16,
                shader_model.HighestShaderModel.0 % 16
            );
        }

        {
            let options7 = D3D12_FEATURE_DATA_D3D12_OPTIONS7::default();
            device.CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS7,
                std::ptr::addr_of!(options7) as _,
                size_of_val(&options7) as u32,
            )?;

            assert_ne!(options7.MeshShaderTier, D3D12_MESH_SHADER_TIER_NOT_SUPPORTED);
        }

        {
            let options: D3D12_FEATURE_DATA_D3D12_OPTIONS16 = Default::default();
            device.CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS16,
                std::ptr::addr_of!(options) as _,
                size_of::<D3D12_FEATURE_DATA_D3D12_OPTIONS16>() as u32,
            )?;

            println!("GPUUploadHeapSupported: {}", options.GPUUploadHeapSupported.as_bool());
        }

        let cmd_queue = device.CreateCommandQueue::<ID3D12CommandQueue>(&D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
            Priority: D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
            Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        })?;

        let fence = device.CreateFence::<ID3D12Fence>(0, D3D12_FENCE_FLAG_NONE)?;
        let fence_event = CreateEventA(None, false, false, s!("render_fence_event"))?;

        let swap_chain = {
            let mut is_tearring_supported: u32 = 0;
            dxgi_factory.CheckFeatureSupport(
                DXGI_FEATURE_PRESENT_ALLOW_TEARING,
                std::ptr::addr_of_mut!(is_tearring_supported) as _,
                size_of::<u32>() as u32,
            )?;

            let mut flags = DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH;
            if is_tearring_supported != 0 {
                flags |= DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING;
            }

            dxgi_factory
                .CreateSwapChainForHwnd(
                    &cmd_queue,
                    window_handle,
                    &DXGI_SWAP_CHAIN_DESC1 {
                        Width: WIDTH,
                        Height: HEIGHT,
                        Format: BACK_BUFFER_FORMAT,
                        Stereo: BOOL(0),
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: FRAME_COUNT,
                        Scaling: DXGI_SCALING_STRETCH,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                        AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
                        Flags: flags.0 as u32,
                    },
                    None,
                    None,
                )?
                .cast::<IDXGISwapChain3>()?
        };

        let back_buffers: [_; FRAME_COUNT as usize] = std::array::from_fn(|i| swap_chain.GetBuffer(i as u32).unwrap());

        let depth_buffer = {
            let mut resource: Option<ID3D12Resource> = None;
            device.CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES::from_heap_type(D3D12_HEAP_TYPE_DEFAULT),
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Width: WIDTH as u64,
                    Height: HEIGHT,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: DXGI_FORMAT_D32_FLOAT,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Flags: D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_DEPTH_WRITE,
                Some(&D3D12_CLEAR_VALUE {
                    Format: DXGI_FORMAT_D32_FLOAT,
                    Anonymous: D3D12_CLEAR_VALUE_0 {
                        DepthStencil: D3D12_DEPTH_STENCIL_VALUE { Depth: 0.0, Stencil: 0 },
                    },
                }),
                &mut resource,
            )?;

            resource.unwrap()
        };

        let rtv_heap = DescriptorHeap::new(&device, D3D12_DESCRIPTOR_HEAP_TYPE_RTV, FRAME_COUNT)?;
        let dsv_heap = DescriptorHeap::new(&device, D3D12_DESCRIPTOR_HEAP_TYPE_DSV, 1)?;

        let resource_heap = DescriptorHeap::new(
            &device,
            D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
            GpuResource::Count as u32,
        )?;

        let rtvs: [_; FRAME_COUNT as usize] = std::array::from_fn(|i| {
            let handle = rtv_heap.get_cpu_handle(i as u32);
            device.CreateRenderTargetView(&back_buffers[i], None, handle);

            handle
        });

        let dsv = {
            let handle = dsv_heap.get_cpu_handle(0);
            device.CreateDepthStencilView(&depth_buffer, None, handle);

            handle
        };

        let cmd_allocators: [_; FRAME_COUNT as usize] = (0..FRAME_COUNT)
            .map(|_| device.CreateCommandAllocator::<ID3D12CommandAllocator>(D3D12_COMMAND_LIST_TYPE_DIRECT))
            .collect::<windows::core::Result<Vec<_>>>()?
            .try_into()
            .expect("0..FRAME_COUNT must produce exactly FRAME_COUNT elements");

        let cmd_list = device.CreateCommandList1::<ID3D12GraphicsCommandList6>(
            0,
            D3D12_COMMAND_LIST_TYPE_DIRECT,
            D3D12_COMMAND_LIST_FLAG_NONE,
        )?;

        let root_signature = {
            let mut blob: Option<ID3DBlob> = None;
            let mut error: Option<ID3DBlob> = None;

            let root_params = [
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                    Anonymous: D3D12_ROOT_PARAMETER_0 {
                        Constants: D3D12_ROOT_CONSTANTS {
                            ShaderRegister: 0,
                            RegisterSpace: 0,
                            Num32BitValues: 32,
                        },
                    },
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                },
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_CBV,
                    Anonymous: D3D12_ROOT_PARAMETER_0 {
                        Descriptor: D3D12_ROOT_DESCRIPTOR {
                            ShaderRegister: 0,
                            RegisterSpace: 1,
                        },
                    },
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                },
            ];

            let static_samplers = [
                D3D12_STATIC_SAMPLER_DESC {
                    Filter: D3D12_FILTER_MIN_MAG_MIP_POINT,
                    AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                    AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                    AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                    ComparisonFunc: D3D12_COMPARISON_FUNC_NEVER,
                    BorderColor: D3D12_STATIC_BORDER_COLOR_TRANSPARENT_BLACK,
                    MaxLOD: D3D12_FLOAT32_MAX,
                    ShaderRegister: 0, // s0
                    RegisterSpace: 0,  // space0
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                    ..Default::default()
                },
                D3D12_STATIC_SAMPLER_DESC {
                    Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR,
                    AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                    AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                    AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                    ComparisonFunc: D3D12_COMPARISON_FUNC_NEVER,
                    BorderColor: D3D12_STATIC_BORDER_COLOR_TRANSPARENT_BLACK,
                    MaxLOD: D3D12_FLOAT32_MAX,
                    ShaderRegister: 0, // s0
                    RegisterSpace: 1,  // space1
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                    ..Default::default()
                },
            ];

            D3D12SerializeRootSignature(
                &D3D12_ROOT_SIGNATURE_DESC {
                    NumParameters: root_params.len() as u32,
                    pParameters: root_params.as_ptr(),
                    NumStaticSamplers: static_samplers.len() as u32,
                    pStaticSamplers: static_samplers.as_ptr(),
                    Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT
                        | D3D12_ROOT_SIGNATURE_FLAG_CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED,
                },
                D3D_ROOT_SIGNATURE_VERSION_1,
                &mut blob,
                Some(&mut error),
            )?;

            let blob = blob.unwrap();
            let bytecode = std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize());

            device.CreateRootSignature::<ID3D12RootSignature>(0, bytecode)?
        };

        {
            ImGui_CreateContext(std::ptr::null_mut());
            ImGui_StyleColorsClassic(std::ptr::null_mut());

            let io = ImGui_GetIO().as_mut().unwrap();
            io.ConfigFlags |= ImGuiConfigFlags_NavEnableKeyboard;
            io.ConfigFlags |= ImGuiConfigFlags_DockingEnable;
            io.ConfigFlags |= ImGuiConfigFlags_ViewportsEnable;

            cimgui_implwin32_init(window_handle.0);
            cimgui_impldx12_init(&mut ImGui_ImplDX12_InitInfo {
                device: device.as_raw() as *mut _,
                command_queue: cmd_queue.as_raw() as *mut _,
                num_frames_in_flight: FRAME_COUNT as i32,
                rtv_format: BACK_BUFFER_FORMAT,
                dsv_format: DEPTH_BUFFER_FORMAT,
                user_data: std::ptr::null_mut(),
                srv_descriptor_heap: resource_heap.d3d12().as_raw() as *mut _,
                srv_descriptor_alloc_fn: None,
                srv_descriptor_free_fn: None,
                legacy_srv_cpu: resource_heap.get_cpu_handle(GpuResource::ImGuiFont as u32),
                legacy_srv_gpu: resource_heap.get_gpu_handle(GpuResource::ImGuiFont as u32),
            });
        }

        let mut terrain = TerrainData::new(&device, &resource_heap, &root_signature)?;

        let mut cpu_frame_index = 0;
        let mut gpu_frame_index = 0;
        let mut frame_timer = FrameTimer::new();

        loop {
            {
                let mut message = MSG::default();
                let mut is_done = false;

                while PeekMessageA(&mut message, None, 0, 0, PM_REMOVE).into() {
                    _ = TranslateMessage(&message);
                    DispatchMessageA(&message);

                    if message.message == WM_QUIT {
                        is_done = true;
                    }
                }

                if is_done {
                    break;
                }
            }

            cpu_frame_index += 1;

            // Update
            let (dt, fps) = frame_timer.tick();

            {
                camera_controller.control(dt, &input, &mut camera);

                input.mouse_dx = 0;
                input.mouse_dy = 0;
            }

            // Render
            let active_frame_index = swap_chain.GetCurrentBackBufferIndex();
            let cmd_allocator = &cmd_allocators[active_frame_index as usize];

            cmd_allocator.Reset()?;
            cmd_list.Reset(cmd_allocator, None)?;

            cmd_list.RSSetViewports(&[D3D12_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: WIDTH as f32,
                Height: HEIGHT as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]);

            cmd_list.RSSetScissorRects(&[RECT {
                left: 0,
                top: 0,
                right: WIDTH as i32,
                bottom: HEIGHT as i32,
            }]);

            cmd_list.ResourceBarrier(&[D3D12_RESOURCE_BARRIER::new_transition(
                &back_buffers[active_frame_index as usize],
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            )]);

            let rtv = rtvs[active_frame_index as usize];

            cmd_list.OMSetRenderTargets(1, Some(&rtv), false, Some(&dsv));
            cmd_list.ClearRenderTargetView(rtv, &[0.3, 0.3, 0.3, 1.0], None);
            cmd_list.ClearDepthStencilView(dsv, D3D12_CLEAR_FLAG_DEPTH, 0.0, 0, None);

            cmd_list.SetDescriptorHeaps(&[Some(resource_heap.d3d12().clone())]);
            cmd_list.SetGraphicsRootSignature(&root_signature);

            let upload_atlas_ms = measure_ms!(terrain.upload_atlas_data(
                &device,
                &cmd_list,
                cpu_frame_index,
                gpu_frame_index,
                active_frame_index
            ));
            let collect_patches_ms = measure_ms!(terrain.collect_leaf_patches(camera.position(), active_frame_index));
            let upload_indirection_ms =
                measure_ms!(terrain.upload_indirection_data(&device, &cmd_list, active_frame_index));

            terrain.render(&cmd_list, &camera, active_frame_index);

            {
                cimgui_implwin32_new_frame();
                cimgui_impldx12_new_frame();
                ImGui_NewFrame();

                ImGui_Begin(c"App".as_ptr(), std::ptr::null_mut(), 0);
                {
                    imgui_text!("FPS: {} ({:.2} ms)", fps, dt * 1000.0);

                    let mut local_mem = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                    let mut host_mem = DXGI_QUERY_VIDEO_MEMORY_INFO::default();

                    adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut local_mem)?;
                    adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, &mut host_mem)?;

                    imgui_text!(
                        "Local VRAM: {} / {} mb",
                        local_mem.CurrentUsage / (1024 * 1024),
                        local_mem.Budget / (1024 * 1024)
                    );

                    imgui_text!("Host VRAM: {} mb", host_mem.CurrentUsage / (1024 * 1024));

                    ImGui_NewLine();
                    ImGui_SeparatorText(c"Camera".as_ptr());
                    imgui_text!("Position: {}", camera.position());
                    imgui_text!("Yaw:  {:>7.2}", camera_controller.yaw());
                    imgui_text!("Pitch: {:>6.2}", camera_controller.pitch());
                    ImGui_DragFloat(c"Speed".as_ptr(), &mut camera_controller.speed);
                }
                ImGui_End();

                ImGui_Begin(c"Profiler".as_ptr(), std::ptr::null_mut(), 0);
                {
                    imgui_text!("Upload atlas: {:.2} ms", upload_atlas_ms);
                    imgui_text!("Collect patches: {:.2} ms", collect_patches_ms);
                    imgui_text!("Upload indirection: {:.2} ms", upload_indirection_ms);

                    ImGui_NewLine();
                }
                ImGui_End();

                terrain.render_imgui();
                terrain.render_imgui_qtree();
                terrain.render_imgui_atlas(&resource_heap);

                // ImGui_ShowDemoWindow(std::ptr::null_mut());
                ImGui_Render();

                cimgui_impldx12_render_draw_data(ImGui_GetDrawData(), cmd_list.as_raw() as *mut _);

                let io = ImGui_GetIO().as_ref().unwrap();
                if io.ConfigFlags & ImGuiConfigFlags_ViewportsEnable != 0 {
                    ImGui_UpdatePlatformWindows();
                    ImGui_RenderPlatformWindowsDefault();
                }
            }

            cmd_list.ResourceBarrier(&[D3D12_RESOURCE_BARRIER::new_transition(
                &back_buffers[active_frame_index as usize],
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PRESENT,
            )]);

            cmd_list.Close()?;

            cmd_queue.ExecuteCommandLists(&[Some(cmd_list.cast::<ID3D12CommandList>()?)]);

            swap_chain.Present(0, DXGI_PRESENT_ALLOW_TEARING).ok()?;
            cmd_queue.Signal(&fence, cpu_frame_index)?;

            gpu_frame_index = fence.GetCompletedValue();
            if cpu_frame_index - gpu_frame_index >= FRAME_COUNT as u64 {
                let gpu_frame_index_to_wait = cpu_frame_index - FRAME_COUNT as u64 + 1;
                wait_for_gpu(&fence, fence_event, gpu_frame_index_to_wait)?;

                gpu_frame_index = fence.GetCompletedValue();
            }
        }

        wait_for_gpu(&fence, fence_event, cpu_frame_index)?;

        {
            cimgui_impldx12_shutdown();
            cimgui_implwin32_shutdown();
            ImGui_DestroyContext(std::ptr::null_mut());
        }

        drop(terrain);
        drop(root_signature);
        drop(cmd_list);
        drop(cmd_allocators);
        drop(resource_heap);
        drop(dsv_heap);
        drop(rtv_heap);
        drop(depth_buffer);
        drop(back_buffers);
        drop(swap_chain);
        CloseHandle(fence_event)?;
        drop(fence);
        drop(cmd_queue);

        if cfg!(debug_assertions) {
            let debug_device = device.cast::<ID3D12DebugDevice>()?;
            debug_device.ReportLiveDeviceObjects(D3D12_RLDO_DETAIL | D3D12_RLDO_IGNORE_INTERNAL)?;
        }

        drop(device);
        drop(adapter);
        drop(dxgi_factory);

        UnregisterClassA(WINDOW_REGISTRY_NAME, None)?;
    }

    Ok(())
}

extern "system" fn handle_window_message(window_handle: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if unsafe { cimgui_implwin32_wnd_proc_handler(window_handle, message, wparam, lparam).0 } != 0 {
        return LRESULT::default();
    }

    let input = unsafe {
        let input = GetWindowLongPtrA(window_handle, GWLP_USERDATA) as *mut InputState;
        if input.is_null() {
            return DefWindowProcA(window_handle, message, wparam, lparam);
        }

        &mut *input
    };

    let imgui_mouse_capture =
        unsafe { !ImGui_GetCurrentContext().is_null() && ImGui_GetIO().as_ref().unwrap().WantCaptureMouse };

    match message {
        WM_KEYDOWN => {
            input.keys[wparam.0] = true;
            LRESULT::default()
        }
        WM_KEYUP => {
            input.keys[wparam.0] = false;
            LRESULT::default()
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let dx = x - input.mouse_x;
            let dy = y - input.mouse_y;

            input.mouse_x = x;
            input.mouse_y = y;

            if !imgui_mouse_capture {
                input.mouse_dx = dx;
                input.mouse_dy = dy;
            } else {
                input.mouse_dx = 0;
                input.mouse_dy = 0;
            }

            LRESULT::default()
        }
        WM_RBUTTONDOWN => {
            input.right_mouse_down = true;
            LRESULT::default()
        }
        WM_RBUTTONUP => {
            input.right_mouse_down = false;
            LRESULT::default()
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT::default()
        }
        // DestroyWindow is handled by DefWindowProcA
        _ => unsafe { DefWindowProcA(window_handle, message, wparam, lparam) },
    }
}

fn wide_to_string(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}

struct FrameTimer {
    last_frame: Instant,
    accumulated: Duration,
    frame_count: u32,
    fps: u32,
}

impl FrameTimer {
    fn new() -> Self {
        Self {
            last_frame: Instant::now(),
            accumulated: Duration::ZERO,
            frame_count: 0,
            fps: 0,
        }
    }

    fn tick(&mut self) -> (f32, u32) {
        let now = Instant::now();
        let delta = now.duration_since(self.last_frame);

        self.last_frame = now;
        self.accumulated += delta;
        self.frame_count += 1;

        if self.accumulated >= Duration::from_secs(1) {
            self.fps = (self.frame_count as f32 / self.accumulated.as_secs_f32()) as u32;
            self.accumulated = Duration::ZERO;
            self.frame_count = 0;
        }

        (delta.as_secs_f32(), self.fps)
    }
}
