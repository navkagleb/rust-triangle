mod async_compute;
mod camera;
mod d3d12_utils;
mod mesh;
mod terrain;

use std::ffi::CString;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use noise::utils::{NoiseMapBuilder, PlaneMapBuilder};
use noise::{Fbm, MultiFractal, Perlin};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::Win32::System::Threading::{CreateEventA, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Interface, PCSTR, s};

use d3d12_utils::*;
use imgui_sys::*;
use mesh::{GpuMesh, LoadThreadPool, LoadedMesh};
use terrain::{GpuTerrainConsts, TerrainData, TerrainDataUi};

const WINDOW_REGISTRY_NAME: PCSTR = s!("rust-window");
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FRAME_COUNT: u32 = 3;
const BACK_BUFFER_FORMAT: DXGI_FORMAT = DXGI_FORMAT_R8G8B8A8_UNORM;
const DEPTH_BUFFER_FORMAT: DXGI_FORMAT = DXGI_FORMAT_D32_FLOAT;
const GPU_MESH_THREAD_POOL_SIZE: usize = 4;

macro_rules! imgui_text {
    ($($arg:tt)*) => {
        ImGui_Text(CString::new(format!($($arg)*)).unwrap().as_ptr())
    };
}

#[repr(u32)]
enum GpuResource {
    ImGuiFont,
    HeightMap,
    TerrainNodeBuffer,
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
    let mut camera = camera::Camera::new();
    let mut camera_controller = camera::CameraController::default();

    let mut input = InputState {
        keys: [false; 256],
        mouse_x: 0,
        mouse_y: 0,
        mouse_dx: 0,
        mouse_dy: 0,
        right_mouse_down: false,
    };

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

            Arc::new(device.cast::<ID3D12Device4>()?)
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
            NumDescriptors: GpuResource::Count as u32,
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
            Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
            NodeMask: 0,
        })?;

        let rtvs: [_; FRAME_COUNT as usize] = std::array::from_fn(|i| {
            let handle = rtv_heap.get_cpu_handle(&device, i as u32);
            device.CreateRenderTargetView(&back_buffers[i], None, handle);

            handle
        });

        let dsv = {
            let handle = dsv_heap.get_cpu_handle(&device, 0);
            device.CreateDepthStencilView(&depth_buffer, None, handle);

            handle
        };

        let cmd_allocators: [_; FRAME_COUNT as usize] = (0..FRAME_COUNT)
            .map(|_| device.CreateCommandAllocator::<ID3D12CommandAllocator>(D3D12_COMMAND_LIST_TYPE_DIRECT))
            .collect::<windows::core::Result<Vec<_>>>()?
            .try_into()
            .expect("0..FRAME_COUNT must produce exactly FRAME_COUNT elements");

        let render_cmd_list = device.CreateCommandList1::<ID3D12GraphicsCommandList1>(
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

            let static_samplers = [D3D12_STATIC_SAMPLER_DESC {
                Filter: D3D12_FILTER_MIN_MAG_MIP_POINT,
                AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                ComparisonFunc: D3D12_COMPARISON_FUNC_NEVER,
                BorderColor: D3D12_STATIC_BORDER_COLOR_TRANSPARENT_BLACK,
                MaxLOD: D3D12_FLOAT32_MAX,
                ShaderRegister: 0, // s0
                RegisterSpace: 0,
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                ..Default::default()
            }];

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

        let to_bytecode = |blob: &[u8]| D3D12_SHADER_BYTECODE {
            pShaderBytecode: blob.as_ptr() as _,
            BytecodeLength: blob.len(),
        };

        let blend_state = D3D12_BLEND_DESC {
            RenderTarget: {
                let mut render_targets = [D3D12_RENDER_TARGET_BLEND_DESC::default(); 8];
                render_targets[0].RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8;
                render_targets
            },
            ..Default::default()
        };

        let pso = {
            let vs_blob = std::fs::read(Path::new("target/dxil/triangle.vs.dxil"))?;
            let ps_blob = std::fs::read(Path::new("target/dxil/triangle.ps.dxil"))?;

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
                BlendState: blend_state,
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

        let terrain_pso = {
            let vs_blob = std::fs::read(Path::new("target/dxil/terrain.vs.dxil"))?;
            let ps_blob = std::fs::read(Path::new("target/dxil/terrain.ps.dxil"))?;

            device.CreateGraphicsPipelineState::<ID3D12PipelineState>(&D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                pRootSignature: ManuallyDrop::new(std::mem::transmute_copy(&root_signature)),
                VS: to_bytecode(&vs_blob),
                PS: to_bytecode(&ps_blob),
                BlendState: blend_state,
                SampleMask: u32::MAX,
                RasterizerState: D3D12_RASTERIZER_DESC {
                    FillMode: D3D12_FILL_MODE_SOLID,
                    CullMode: D3D12_CULL_MODE_NONE,
                    FrontCounterClockwise: BOOL(0),
                    ..Default::default()
                },
                DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
                    DepthEnable: BOOL(1),
                    DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ALL,
                    DepthFunc: D3D12_COMPARISON_FUNC_GREATER,
                    ..Default::default()
                },
                InputLayout: Default::default(),
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
                srv_descriptor_heap: resource_heap.as_raw() as *mut _,
                srv_descriptor_alloc_fn: None,
                srv_descriptor_free_fn: None,
                legacy_srv_cpu: resource_heap.get_cpu_handle(&device, GpuResource::ImGuiFont as u32),
                legacy_srv_gpu: resource_heap.get_gpu_handle(&device, GpuResource::ImGuiFont as u32),
            });
        }

        let mut terrain = TerrainData::new(
            &device,
            resource_heap.get_cpu_handle(&device, GpuResource::TerrainNodeBuffer as u32),
        )?;

        let mut terrain_ui = TerrainDataUi {
            height_map_size: terrain.height_map_size as i32,
            height_map_scale: 5.0,
        };

        let async_compute_thread =
            async_compute::start_thread(Arc::clone(&device), loaded_mesh_receiver, ready_mesh_sender);

        let mut cpu_frame_index = 0;
        let mut frame_timer = FrameTimer::new();

        let mut mesh_spawn_count = 1;
        let mut pending_mesh_count = 0;

        let mut deferred_release = Vec::new();

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

            let (dt, fps) = frame_timer.tick();

            {
                camera_controller.control(dt, &input, &mut camera);

                input.mouse_dx = 0;
                input.mouse_dy = 0;
            }

            let terrain_nodes = terrain.collect_nodes(camera.position());

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

            render_cmd_list.SetDescriptorHeaps(&[Some(resource_heap.clone())]);
            render_cmd_list.SetGraphicsRootSignature(&root_signature);

            render_cmd_list.SetPipelineState(&pso);
            render_cmd_list.SetGraphicsRoot32BitConstants(0, 16, std::ptr::from_ref(&camera.world_to_clip()).cast(), 0);

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
                terrain.node_buffer.map_and_write(terrain_nodes.as_slice())?;

                render_cmd_list.SetGraphicsRootConstantBufferView(
                    1,
                    terrain.const_buffer.write(
                        active_frame_index,
                        &GpuTerrainConsts {
                            terrain_size: terrain.size,
                            world_scale: 1.0,
                            height_scale: terrain.height_scale,
                        },
                    ),
                );

                render_cmd_list.SetPipelineState(&terrain_pso);
                render_cmd_list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                render_cmd_list.IASetIndexBuffer(Some(&terrain.chunk_ibv));

                render_cmd_list.DrawIndexedInstanced(
                    terrain.chunk_index_count as u32,
                    terrain_nodes.len() as u32,
                    0,
                    0,
                    0,
                );
            }

            {
                cimgui_implwin32_new_frame();
                cimgui_impldx12_new_frame();
                ImGui_NewFrame();

                ImGui_Begin(c"HeightMap".as_ptr(), std::ptr::null_mut(), 0);
                {
                    imgui_text!("Camera position: {}", camera.position());
                    ImGui_DragFloat(c"Camera speed".as_ptr(), &mut camera_controller.speed);
                    ImGui_NewLine();

                    ImGui_InputFloat(c"Height scale".as_ptr(), &mut terrain.height_scale);
                    ImGui_InputFloat(c"LOD factor".as_ptr(), &mut terrain.lod_factor);
                    ImGui_InputFloat(c"Terrain size".as_ptr(), &mut terrain.size);
                    ImGui_NewLine();

                    ImGui_InputInt(c"Height map size".as_ptr(), &mut terrain_ui.height_map_size);
                    ImGui_InputFloat(c"Height map scale".as_ptr(), &mut terrain_ui.height_map_scale);

                    if ImGui_Button(c"Generate height map".as_ptr()) {
                        let fbm = Fbm::<Perlin>::new(123)
                            .set_octaves(8)
                            .set_frequency(1.0)
                            .set_lacunarity(2.0)
                            .set_persistence(0.7);

                        let height_map_data = PlaneMapBuilder::new(fbm)
                            .set_size(terrain_ui.height_map_size as usize, terrain_ui.height_map_size as usize)
                            .set_x_bounds(0.0, terrain_ui.height_map_scale as f64)
                            .set_y_bounds(0.0, terrain_ui.height_map_scale as f64)
                            .build()
                            .into_iter()
                            .map(|n| n as f32)
                            .collect::<Vec<_>>();

                        let min = height_map_data.iter().cloned().fold(f32::INFINITY, f32::min);
                        let max = height_map_data.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                        println!("min: {min}, max: {max}");

                        let top_mip_data = height_map_data
                            .iter()
                            .map(|n| (n - min) / (max - min))
                            .collect::<Vec<_>>();

                        let mips = terrain::generate_mips(top_mip_data, terrain_ui.height_map_size as usize);

                        let height_map_texture = ID3D12Resource::new_texture(
                            &device,
                            DXGI_FORMAT_R32_FLOAT,
                            terrain_ui.height_map_size as u32,
                            terrain_ui.height_map_size as u32,
                            mips.len() as u32,
                        )?;

                        device.CreateShaderResourceView(
                            &height_map_texture,
                            Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                                Format: DXGI_FORMAT_R32_FLOAT,
                                ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                                    Texture2D: D3D12_TEX2D_SRV {
                                        MostDetailedMip: 0,
                                        MipLevels: mips.len() as u32,
                                        PlaneSlice: 0,
                                        ResourceMinLODClamp: 0.0,
                                    },
                                },
                            }),
                            resource_heap.get_cpu_handle(&device, GpuResource::HeightMap as u32),
                        );

                        let mip_slices = mips
                            .iter()
                            .map(|m| std::slice::from_raw_parts(m.as_ptr() as *const u8, m.len() * size_of::<f32>()))
                            .collect::<Vec<_>>();

                        let upload_buffer =
                            render_cmd_list.upload_mips(&device, mip_slices.as_slice(), &height_map_texture)?;

                        deferred_release.push((cpu_frame_index, upload_buffer.cast::<ID3D12Object>()?));

                        if let Some(prev) = terrain.height_map_texture.replace(height_map_texture) {
                            deferred_release.push((cpu_frame_index, prev.cast::<ID3D12Object>()?));
                        }
                    }

                    let image_position = ImGui_GetCursorScreenPos();
                    let image_size = {
                        let size = ImGui_GetContentRegionAvail();
                        size.x.min(size.y)
                    };

                    if terrain.height_map_texture.is_some() {
                        ImGui_Image(
                            ImTextureRef {
                                _TexData: std::ptr::null_mut(),
                                _TexID: resource_heap.get_gpu_handle(&device, GpuResource::HeightMap as u32).ptr,
                            },
                            ImVec2 {
                                x: image_size,
                                y: image_size,
                            },
                        );
                    }

                    let draw_list = ImGui_GetWindowDrawList();

                    for node in terrain_nodes {
                        let center_x = image_position.x + (node.center.x / terrain.size) * image_size;
                        let center_y = image_position.y + (node.center.y / terrain.size) * image_size;
                        let half_size = (node.half_size / terrain.size) * image_size;

                        ImDrawList_AddRectEx(
                            draw_list,
                            ImVec2 {
                                x: center_x - half_size,
                                y: center_y - half_size,
                            },
                            ImVec2 {
                                x: center_x + half_size,
                                y: center_y + half_size,
                            },
                            0xB3FFFFFF,
                            0.0,
                            ImDrawFlags_None,
                            0.5,
                        );

                        let text = CString::new(node.lod_index.to_string()).unwrap();
                        let text_size = ImGui_CalcTextSize(text.as_ptr());

                        ImDrawList_AddText(
                            draw_list,
                            ImVec2 {
                                x: center_x - text_size.x / 2.0,
                                y: center_y - text_size.y / 2.0,
                            },
                            0xFFFFFFFF,
                            text.as_ptr(),
                        )
                    }
                }
                ImGui_End();

                ImGui_Begin(c"Hello Rust".as_ptr(), std::ptr::null_mut(), 0);
                {
                    imgui_text!("Main TID: {}", GetCurrentThreadId());
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

                cimgui_impldx12_render_draw_data(ImGui_GetDrawData(), render_cmd_list.as_raw() as *mut _);

                let io = *ImGui_GetIO();
                if io.ConfigFlags & ImGuiConfigFlags_ViewportsEnable != 0 {
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
            cmd_queue.Signal(&render_fence, cpu_frame_index)?;

            let mut gpu_frame_index = render_fence.GetCompletedValue();
            if cpu_frame_index - gpu_frame_index >= FRAME_COUNT as u64 {
                let gpu_frame_index_to_wait = cpu_frame_index - FRAME_COUNT as u64 + 1;
                wait_for_gpu(&render_fence, render_fence_event, gpu_frame_index_to_wait)?;

                gpu_frame_index = render_fence.GetCompletedValue();
            }

            deferred_release.retain(|&(frame_index, _)| frame_index > gpu_frame_index);
        }

        wait_for_gpu(&render_fence, render_fence_event, cpu_frame_index)?;

        {
            cimgui_impldx12_shutdown();
            cimgui_implwin32_shutdown();
            ImGui_DestroyContext(std::ptr::null_mut());
        }

        assert!(deferred_release.is_empty());

        for gpu_mesh in gpu_meshes {
            drop(gpu_mesh.vertex_buffer);
            drop(gpu_mesh.index_buffer);
        }

        drop(load_mesh_thread_pool);
        _ = async_compute_thread.join().unwrap();

        drop(terrain.height_map_texture);
        drop(terrain_pso);
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

            input.mouse_dx = x - input.mouse_x;
            input.mouse_dy = y - input.mouse_y;
            input.mouse_x = x;
            input.mouse_y = y;

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
