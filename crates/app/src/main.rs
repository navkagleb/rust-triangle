mod async_compute;
mod mesh;

use std::ffi::CString;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glam::{Mat4, Quat, Vec3};
use rand::prelude::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Interface, PCSTR, PCWSTR, s};

use imgui_sys::*;
use mesh::*;

const WINDOW_REGISTRY_NAME: PCSTR = s!("rust-window");
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const FRAME_COUNT: u32 = 3;
const BACK_BUFFER_FORMAT: DXGI_FORMAT = DXGI_FORMAT_R8G8B8A8_UNORM;
const DEPTH_BUFFER_FORMAT: DXGI_FORMAT = DXGI_FORMAT_D32_FLOAT;
const GPU_MESH_THREAD_POOL_SIZE: usize = 4;

fn measure<F, R>(f: F) -> (R, f32)
where
    F: FnOnce() -> R,
{
    let t = Instant::now();
    let r = f();

    (r, t.elapsed().as_secs_f32() * 1000.0)
}

macro_rules! imgui_text {
    ($($arg:tt)*) => {
        ImGui_Text(CString::new(format!($($arg)*)).unwrap().as_ptr())
    };
}

fn main() -> Result<()> {
    println!("Hello D3D12 Rust Triangle!");

    let cam_pos = Vec3::ZERO;
    let cam_front_dir = Vec3::new(0.0, 0.0, 1.0).normalize();
    let fov_y = 90_f32.to_radians();
    let near_plane = 0.1;

    let world_to_view = Mat4::look_to_lh(cam_pos, cam_front_dir, Vec3::Y);
    let view_to_clip = Mat4::perspective_infinite_reverse_lh(fov_y, WIDTH as f32 / HEIGHT as f32, near_plane);
    let world_to_clip = view_to_clip * world_to_view;

    let (loaded_mesh_sender, loaded_mesh_receiver) = std::sync::mpsc::channel::<Result<LoadedMesh>>();
    let (ready_mesh_sender, ready_mesh_receiver) = std::sync::mpsc::channel::<GpuMesh>();
    let load_mesh_thread_pool = LoadThreadPool::new(GPU_MESH_THREAD_POOL_SIZE, loaded_mesh_sender);

    let mut gpu_meshes = Vec::new();

    let mesh_paths = collect_mesh_paths(Path::new("assets"))?;

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

        println!("{:?}, width: {}, height: {}", window_handle, WIDTH, HEIGHT);

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

            Arc::new(device.cast::<ID3D12Device4>()?)
        };

        if cfg!(debug_assertions) {
            let info_queue = device.cast::<ID3D12InfoQueue1>()?;
            info_queue.RegisterMessageCallback(
                Some(d3d12_message_callback),
                D3D12_MESSAGE_CALLBACK_FLAG_NONE,
                std::ptr::null_mut(),
                &mut 0u32,
            )?;
        }

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

        let render_fence = device.CreateFence::<ID3D12Fence>(0, D3D12_FENCE_FLAG_NONE)?;
        let render_fence_event = CreateEventA(None, false, false, s!("render_fence_event"))?;

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

        let rtv_heap = device.CreateDescriptorHeap::<ID3D12DescriptorHeap>(&D3D12_DESCRIPTOR_HEAP_DESC {
            NumDescriptors: FRAME_COUNT,
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
            Flags: D3D12_DESCRIPTOR_HEAP_FLAG_NONE,
            NodeMask: 0,
        })?;

        let dsv_heap = device.CreateDescriptorHeap::<ID3D12DescriptorHeap>(&D3D12_DESCRIPTOR_HEAP_DESC {
            NumDescriptors: 1,
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_DSV,
            Flags: D3D12_DESCRIPTOR_HEAP_FLAG_NONE,
            NodeMask: 0,
        })?;

        let resource_heap = device.CreateDescriptorHeap::<ID3D12DescriptorHeap>(&D3D12_DESCRIPTOR_HEAP_DESC {
            NumDescriptors: 1,
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
            Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
            NodeMask: 0,
        })?;

        let rtvs: [_; FRAME_COUNT as usize] = std::array::from_fn(|i| {
            let rtv_begin_ptr = rtv_heap.GetCPUDescriptorHandleForHeapStart().ptr;
            let rtv_size = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) as usize;
            let rtv = D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: rtv_begin_ptr + i * rtv_size,
            };

            device.CreateRenderTargetView(&back_buffers[i], None, rtv);

            rtv
        });

        let dsv = {
            let dsv_begin_ptr = dsv_heap.GetCPUDescriptorHandleForHeapStart().ptr;
            let dsv = D3D12_CPU_DESCRIPTOR_HANDLE { ptr: dsv_begin_ptr };

            device.CreateDepthStencilView(&depth_buffer, None, dsv);

            dsv
        };

        let cmd_allocators: [_; FRAME_COUNT as usize] = std::array::from_fn(|_| {
            device
                .CreateCommandAllocator::<ID3D12CommandAllocator>(D3D12_COMMAND_LIST_TYPE_DIRECT)
                .unwrap()
        });

        let render_cmd_list = device.CreateCommandList1::<ID3D12GraphicsCommandList1>(
            0,
            D3D12_COMMAND_LIST_TYPE_DIRECT,
            D3D12_COMMAND_LIST_FLAG_NONE,
        )?;

        let root_signature = {
            let mut blob: Option<ID3DBlob> = None;
            let mut error: Option<ID3DBlob> = None;

            let root_params = [D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Constants: D3D12_ROOT_CONSTANTS {
                        ShaderRegister: 0,
                        RegisterSpace: 0,
                        Num32BitValues: 32,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            }];

            D3D12SerializeRootSignature(
                &D3D12_ROOT_SIGNATURE_DESC {
                    NumParameters: root_params.len() as u32,
                    pParameters: root_params.as_ptr(),
                    NumStaticSamplers: 0,
                    pStaticSamplers: std::ptr::null(),
                    Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
                },
                D3D_ROOT_SIGNATURE_VERSION_1,
                &mut blob,
                Some(&mut error),
            )?;

            let blob = blob.unwrap();
            let bytecode = std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize());

            device.CreateRootSignature::<ID3D12RootSignature>(0, bytecode)?
        };

        let pso = {
            let vs_blob = std::fs::read(Path::new("target/dxil/triangle.vs.dxil"))?;
            let ps_blob = std::fs::read(Path::new("target/dxil/triangle.ps.dxil"))?;

            let to_bytecode = |blob: &[u8]| D3D12_SHADER_BYTECODE {
                pShaderBytecode: blob.as_ptr() as _,
                BytecodeLength: blob.len(),
            };

            let input_elements = [
                D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: s!("sem_Position"),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32B32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 0,
                    InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
                D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: s!("sem_Normal"),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32B32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: size_of::<f32>() as u32 * 3,
                    InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
            ];

            device.CreateGraphicsPipelineState::<ID3D12PipelineState>(&D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                pRootSignature: ManuallyDrop::new(std::mem::transmute_copy(&root_signature)),
                VS: to_bytecode(&vs_blob),
                PS: to_bytecode(&ps_blob),
                BlendState: D3D12_BLEND_DESC {
                    RenderTarget: {
                        let mut render_targets = [D3D12_RENDER_TARGET_BLEND_DESC::default(); 8];
                        render_targets[0].RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8;
                        render_targets
                    },
                    ..Default::default()
                },
                SampleMask: u32::MAX,
                RasterizerState: D3D12_RASTERIZER_DESC {
                    FillMode: D3D12_FILL_MODE_SOLID,
                    CullMode: D3D12_CULL_MODE_BACK,
                    FrontCounterClockwise: BOOL(0),
                    ..Default::default()
                },
                DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
                    DepthEnable: BOOL(1),
                    DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ALL,
                    DepthFunc: D3D12_COMPARISON_FUNC_GREATER,
                    ..Default::default()
                },
                InputLayout: D3D12_INPUT_LAYOUT_DESC {
                    pInputElementDescs: input_elements.as_ptr(),
                    NumElements: input_elements.len() as u32,
                },
                PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                NumRenderTargets: 1,
                RTVFormats: {
                    let mut formats = [DXGI_FORMAT_UNKNOWN; 8];
                    formats[0] = BACK_BUFFER_FORMAT;
                    formats
                },
                DSVFormat: DEPTH_BUFFER_FORMAT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                ..Default::default()
            })?
        };

        {
            ImGui_CreateContext(std::ptr::null_mut());
            ImGui_StyleColorsClassic(std::ptr::null_mut());

            let io = &mut *ImGui_GetIO();
            io.ConfigFlags |= ImGuiConfigFlags__ImGuiConfigFlags_NavEnableKeyboard;
            io.ConfigFlags |= ImGuiConfigFlags__ImGuiConfigFlags_DockingEnable;
            io.ConfigFlags |= ImGuiConfigFlags__ImGuiConfigFlags_ViewportsEnable;

            cimgui_implwin32_init(window_handle.0);
            cimgui_impldx12_init(&mut ImGui_ImplDX12_InitInfo {
                device: device.as_raw() as *mut _,
                command_queue: cmd_queue.as_raw() as *mut _,
                num_frames_in_flight: FRAME_COUNT as i32,
                rtv_format: BACK_BUFFER_FORMAT,
                dsv_format: DEPTH_BUFFER_FORMAT,
                user_data: std::ptr::null_mut(),
                srv_descriptor_heap: resource_heap.as_raw() as *mut _,
                srv_descriptor_alloc_fn: None,
                srv_descriptor_free_fn: None,
                legacy_srv_cpu: resource_heap.GetCPUDescriptorHandleForHeapStart(),
                legacy_srv_gpu: resource_heap.GetGPUDescriptorHandleForHeapStart(),
            });
        }

        let async_compute_thread =
            async_compute::start_thread(Arc::clone(&device), loaded_mesh_receiver, ready_mesh_sender);

        let mut cpu_frame_index = 0;
        let mut frame_timer = FrameTimer::new();

        let mut mesh_spawn_count = 1;
        let mut pending_mesh_count = 0;

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

            let (dt, fps) = frame_timer.tick();

            let active_frame_index = swap_chain.GetCurrentBackBufferIndex();
            let cmd_allocator = &cmd_allocators[active_frame_index as usize];

            while let Ok(gpu_mesh) = ready_mesh_receiver.try_recv() {
                pending_mesh_count -= 1;
                gpu_meshes.push(gpu_mesh);
            }

            cmd_allocator.Reset()?;
            render_cmd_list.Reset(cmd_allocator, None)?;

            render_cmd_list.RSSetViewports(&[D3D12_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: WIDTH as f32,
                Height: HEIGHT as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]);

            render_cmd_list.RSSetScissorRects(&[RECT {
                left: 0,
                top: 0,
                right: WIDTH as i32,
                bottom: HEIGHT as i32,
            }]);

            render_cmd_list.ResourceBarrier(&[D3D12_RESOURCE_BARRIER::new_transition(
                &back_buffers[active_frame_index as usize],
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            )]);

            let rtv = rtvs[active_frame_index as usize];

            render_cmd_list.OMSetRenderTargets(1, Some(&rtv), false, Some(&dsv));
            render_cmd_list.ClearRenderTargetView(rtv, &[0.3, 0.3, 0.3, 1.0], None);
            render_cmd_list.ClearDepthStencilView(dsv, D3D12_CLEAR_FLAG_DEPTH, 0.0, 0, None);

            render_cmd_list.SetGraphicsRootSignature(&root_signature);
            render_cmd_list.SetPipelineState(&pso);

            render_cmd_list.SetGraphicsRoot32BitConstants(0, 16, std::ptr::from_ref(&world_to_clip).cast(), 0);

            render_cmd_list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

            for gpu_mesh in &gpu_meshes {
                render_cmd_list.SetGraphicsRoot32BitConstants(
                    0,
                    16,
                    std::ptr::from_ref(&gpu_mesh.local_to_world).cast(),
                    16,
                );

                render_cmd_list.ResourceBarrier(&[
                    D3D12_RESOURCE_BARRIER::new_transition(
                        &gpu_mesh.vertex_buffer,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                        D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER,
                    ),
                    D3D12_RESOURCE_BARRIER::new_transition(
                        &gpu_mesh.index_buffer,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                        D3D12_RESOURCE_STATE_INDEX_BUFFER,
                    ),
                ]);

                render_cmd_list.IASetVertexBuffers(0, Some(&[gpu_mesh.vbv]));
                render_cmd_list.IASetIndexBuffer(Some(&gpu_mesh.ibv));

                for draw in &gpu_mesh.draws {
                    render_cmd_list.DrawIndexedInstanced(
                        draw.index_count,
                        1,
                        draw.index_offset,
                        draw.vertex_offset as i32,
                        0,
                    );
                }
            }

            {
                cimgui_implwin32_new_frame();
                cimgui_impldx12_new_frame();
                ImGui_NewFrame();

                ImGui_Begin(c"Hello Rust".as_ptr(), std::ptr::null_mut(), 0);
                {
                    imgui_text!("Main TID: {}", GetCurrentThreadId());
                    imgui_text!("FPS: {} ({:.2} ms)", fps, dt);

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

                    imgui_text!("Mesh count: {}", gpu_meshes.len());

                    ImGui_InputInt(c"Spawn count".as_ptr(), &mut mesh_spawn_count);

                    for path in &mesh_paths {
                        let button_text =
                            CString::new(format!("Load '{}'", path.file_name().unwrap().to_str().unwrap())).unwrap();

                        if ImGui_Button(button_text.as_ptr()) {
                            for _ in 0..mesh_spawn_count {
                                load_mesh_thread_pool.submit(&device, path.clone());
                                pending_mesh_count += 1;
                            }
                        }
                    }

                    if pending_mesh_count > 0 {
                        imgui_text!("Loading... ({} pending)", pending_mesh_count);
                    }
                }
                ImGui_End();

                ImGui_ShowDemoWindow(std::ptr::null_mut());
                ImGui_Render();

                render_cmd_list.SetDescriptorHeaps(&[Some(resource_heap.clone())]);
                cimgui_impldx12_render_draw_data(ImGui_GetDrawData(), render_cmd_list.as_raw() as *mut _);

                let io = *ImGui_GetIO();
                if io.ConfigFlags & ImGuiConfigFlags__ImGuiConfigFlags_ViewportsEnable != 0 {
                    ImGui_UpdatePlatformWindows();
                    ImGui_RenderPlatformWindowsDefault();
                }
            }

            render_cmd_list.ResourceBarrier(&[D3D12_RESOURCE_BARRIER::new_transition(
                &back_buffers[active_frame_index as usize],
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PRESENT,
            )]);

            render_cmd_list.Close()?;

            cmd_queue.ExecuteCommandLists(&[Some(render_cmd_list.cast::<ID3D12CommandList>()?)]);

            swap_chain.Present(0, DXGI_PRESENT_ALLOW_TEARING).ok()?;

            cpu_frame_index += 1;
            cmd_queue.Signal(&render_fence, cpu_frame_index)?;

            let gpu_frame_index = render_fence.GetCompletedValue();
            if cpu_frame_index - gpu_frame_index >= FRAME_COUNT as u64 {
                let gpu_frame_index_to_wait = cpu_frame_index - FRAME_COUNT as u64 + 1;
                wait_for_gpu(&render_fence, render_fence_event, gpu_frame_index_to_wait)?;
            }
        }

        wait_for_gpu(&render_fence, render_fence_event, cpu_frame_index)?;

        {
            cimgui_impldx12_shutdown();
            cimgui_implwin32_shutdown();
            ImGui_DestroyContext(std::ptr::null_mut());
        }

        for gpu_mesh in gpu_meshes {
            drop(gpu_mesh.vertex_buffer);
            drop(gpu_mesh.index_buffer);
        }

        drop(load_mesh_thread_pool);
        _ = async_compute_thread.join().unwrap();

        drop(pso);
        drop(root_signature);
        drop(render_cmd_list);
        drop(cmd_allocators);
        drop(resource_heap);
        drop(dsv_heap);
        drop(rtv_heap);
        drop(depth_buffer);
        drop(back_buffers);
        drop(swap_chain);
        CloseHandle(render_fence_event)?;
        drop(render_fence);
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
    unsafe {
        if cimgui_implwin32_wnd_proc_handler(window_handle, message, wparam, lparam).0 != 0 {
            return LRESULT(1);
        }

        match message {
            WM_DESTROY => {
                println!("WM_DESTROY");
                PostQuitMessage(0);
                LRESULT::default()
            }
            // DestroyWindow is handled by DefWindowProcA
            _ => DefWindowProcA(window_handle, message, wparam, lparam),
        }
    }
}

fn wide_to_string(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}

fn wait_for_gpu(fence: &ID3D12Fence, wait_event_handle: HANDLE, wait_value: u64) -> Result<()> {
    unsafe {
        fence.SetEventOnCompletion(wait_value, wait_event_handle)?;

        if WaitForSingleObject(wait_event_handle, INFINITE) == WAIT_FAILED {
            bail!(windows::core::Error::from_thread());
        }
    }

    Ok(())
}

extern "system" fn d3d12_message_callback(
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

trait InterfaceExt {
    fn set_debug_name(&self, name: &str) -> Result<()>;
}

impl<T> InterfaceExt for T
where
    T: Interface,
{
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

trait D3D12HeapPropertiesExt {
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

trait D3D12ResourceBarrierExt {
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
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: unsafe { std::mem::transmute_copy(resource) },
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: state_before,
                    StateAfter: state_after,
                }),
            },
        }
    }
}

trait D3D12ResourceExt {
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

        (delta.as_secs_f32() * 1000.0, self.fps)
    }
}

fn collect_mesh_paths(dir: &Path) -> Result<Vec<PathBuf>> {
    // Could be more efficient if Vec is passed as &mut,
    // but for app startup this is fine
    let mut result = Vec::new();

    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();

        if path.is_dir() {
            result.extend(collect_mesh_paths(&path)?);
        } else if matches!(path.extension().and_then(|e| e.to_str()), Some("glb") | Some("gltf")) {
            result.push(path);
        }
    }

    Ok(result)
}
