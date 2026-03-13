mod imgui_ffi;
use imgui_ffi::*;

use std::mem::ManuallyDrop;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

use glam::Mat4;
use glam::Vec3;

const WINDOW_REGISTRY_NAME: PCSTR = s!("rust-window");
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const FRAME_COUNT: u32 = 3;
const BACK_BUFFER_FORMAT: DXGI_FORMAT = DXGI_FORMAT_R8G8B8A8_UNORM;
const DEPTH_BUFFER_FORMAT: DXGI_FORMAT = DXGI_FORMAT_D32_FLOAT;

#[allow(dead_code)]
#[derive(Debug)]
struct MeshVertex {
    position: (f32, f32, f32),
    normal: (f32, f32, f32),
}

struct Mesh {
    vertex_offset: u32,
    vertex_count: u32,
    index_offset: u32,
    index_count: u32,
}

struct MeshGeometry {
    vertices: Vec<MeshVertex>,
    indices: Vec<u32>,
    meshes: Vec<Mesh>,
}

impl MeshGeometry {
    fn load(path: &str) -> Option<Self> {
        let (gltf, buffers, _) = gltf::import(path).map_err(|e| println!("GLTF error: {}", e)).ok()?;

        assert_eq!(gltf.scenes().len(), 1);
        assert_eq!(gltf.nodes().len(), 1);

        let mut vertex_count = 0;
        let mut index_count = 0;
        let mut meshes = Vec::new();

        for mesh in gltf.meshes() {
            for primitive in mesh.primitives() {
                let position_accessor = primitive.get(&gltf::Semantic::Positions)?;
                let normal_accessor = primitive.get(&gltf::Semantic::Normals)?;

                assert_eq!(position_accessor.count(), normal_accessor.count());
                assert_eq!(primitive.mode(), gltf::mesh::Mode::Triangles);

                let mesh = Mesh {
                    vertex_offset: vertex_count,
                    vertex_count: position_accessor.count() as u32,
                    index_offset: index_count,
                    index_count: primitive.indices()?.count() as u32,
                };

                vertex_count += mesh.vertex_count;
                index_count += mesh.index_count;
                meshes.push(mesh);
            }
        }

        let mut result = Self {
            vertices: Vec::with_capacity(vertex_count as usize),
            indices: Vec::with_capacity(index_count as usize),
            meshes,
        };

        for mesh in gltf.meshes() {
            for primitive in mesh.primitives() {
                let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));

                result
                    .vertices
                    .extend(std::iter::zip(reader.read_positions()?, reader.read_normals()?).map(
                        |(position, normal)| MeshVertex {
                            position: position.into(),
                            normal: normal.into(),
                        },
                    ));

                result.indices.extend(reader.read_indices()?.into_u32());
            }
        }

        Some(result)
    }
}

fn main() -> Result<()> {
    println!("Hello D3D12 Rust Triangle!");

    let mesh_geometry = MeshGeometry::load("assets/Dinosaur.glb").unwrap();
    println!(
        "vertex count: {}, index count: {}",
        mesh_geometry.vertices.len(),
        mesh_geometry.indices.len()
    );

    let cam_pos = Vec3::new(0.0, 50.0, 200.0);
    let cam_front_dir = Vec3::new(0.0, 0.0, -1.0).normalize();
    let fov_y = 90_f32.to_radians();
    let near_plane = 0.1;

    let world_to_view = Mat4::look_to_lh(cam_pos, cam_front_dir, Vec3::Y);
    let view_to_clip = Mat4::perspective_infinite_reverse_lh(fov_y, WIDTH as f32 / HEIGHT as f32, near_plane);
    let world_to_clip = view_to_clip * world_to_view;

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

        let dxgi_adapter = {
            let mut adapter_index = 0;
            let mut selected_dxgi_adapter: Option<IDXGIAdapter1> = None;

            loop {
                match dxgi_factory
                    .EnumAdapterByGpuPreference::<IDXGIAdapter1>(adapter_index, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE)
                {
                    Ok(dxgi_adapter) => {
                        let desc = dxgi_adapter.GetDesc1()?;
                        println!("Adapter {}: {}", adapter_index, wide_to_string(&desc.Description));

                        selected_dxgi_adapter.get_or_insert(dxgi_adapter);
                        adapter_index += 1;
                    }
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(e),
                }
            }

            selected_dxgi_adapter.unwrap()
        };

        {
            let mut d3d12_debug: Option<ID3D12Debug5> = None;
            D3D12GetDebugInterface(&mut d3d12_debug)?;

            if let Some(d3d12_debug) = d3d12_debug {
                d3d12_debug.EnableDebugLayer();
                println!("Enable D3D12 debug layer");

                d3d12_debug.SetEnableGPUBasedValidation(true);
                d3d12_debug.SetEnableAutoName(true);
            }
        }

        let d3d12_device = {
            let mut d3d12_device: Option<ID3D12Device> = None;
            D3D12CreateDevice(&dxgi_adapter, D3D_FEATURE_LEVEL_12_0, &mut d3d12_device)?;

            let d3d12_device = d3d12_device.unwrap();
            d3d12_device.set_debug_name("MainDevice")?;
            d3d12_device.cast::<ID3D12Device4>()?
        };

        {
            let d3d12_info_queue = d3d12_device.cast::<ID3D12InfoQueue1>()?;
            d3d12_info_queue.RegisterMessageCallback(
                Some(d3d12_message_callback),
                D3D12_MESSAGE_CALLBACK_FLAG_NONE,
                std::ptr::null_mut(),
                &mut 0u32,
            )?;
        }

        {
            let d3d12_shader_model = D3D12_FEATURE_DATA_SHADER_MODEL {
                HighestShaderModel: D3D_SHADER_MODEL_6_6,
            };
            d3d12_device.CheckFeatureSupport(
                D3D12_FEATURE_SHADER_MODEL,
                std::ptr::addr_of!(d3d12_shader_model) as _,
                size_of::<D3D12_FEATURE_DATA_SHADER_MODEL>() as u32,
            )?;

            println!(
                "Supported shader model: {}.{}",
                d3d12_shader_model.HighestShaderModel.0 / 16,
                d3d12_shader_model.HighestShaderModel.0 % 16
            );
        }

        {
            let d3d12_options: D3D12_FEATURE_DATA_D3D12_OPTIONS16 = Default::default();
            d3d12_device.CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS16,
                std::ptr::addr_of!(d3d12_options) as _,
                size_of::<D3D12_FEATURE_DATA_D3D12_OPTIONS16>() as u32,
            )?;

            println!(
                "GPUUploadHeapSupported: {}",
                d3d12_options.GPUUploadHeapSupported.as_bool()
            );
        }

        let d3d12_cmd_queue = d3d12_device.CreateCommandQueue::<ID3D12CommandQueue>(&D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
            Priority: D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
            Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        })?;

        let d3d12_frame_fence = d3d12_device.CreateFence::<ID3D12Fence>(0, D3D12_FENCE_FLAG_NONE)?;
        let wait_event_handle = CreateEventA(None, false, false, s!("wait-event"))?;

        let dxgi_swap_chain = {
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
                    &d3d12_cmd_queue,
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

        let d3d12_back_buffers: [_; FRAME_COUNT as usize] =
            std::array::from_fn(|i| dxgi_swap_chain.GetBuffer(i as u32).unwrap());

        let d3d12_depth_buffer = {
            let mut resource: Option<ID3D12Resource> = None;
            d3d12_device.CreateCommittedResource(
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

        let d3d12_rtv_heap =
            d3d12_device.CreateDescriptorHeap::<ID3D12DescriptorHeap>(&D3D12_DESCRIPTOR_HEAP_DESC {
                NumDescriptors: FRAME_COUNT,
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_NONE,
                NodeMask: 0,
            })?;

        let d3d12_dsv_heap =
            d3d12_device.CreateDescriptorHeap::<ID3D12DescriptorHeap>(&D3D12_DESCRIPTOR_HEAP_DESC {
                NumDescriptors: 1,
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_DSV,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_NONE,
                NodeMask: 0,
            })?;

        let d3d12_resource_heap =
            d3d12_device.CreateDescriptorHeap::<ID3D12DescriptorHeap>(&D3D12_DESCRIPTOR_HEAP_DESC {
                NumDescriptors: 1,
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            })?;

        let d3d12_rtvs: [_; FRAME_COUNT as usize] = std::array::from_fn(|i| {
            let rtv_begin_ptr = d3d12_rtv_heap.GetCPUDescriptorHandleForHeapStart().ptr;
            let rtv_size = d3d12_device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) as usize;
            let rtv = D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: rtv_begin_ptr + i * rtv_size,
            };

            d3d12_device.CreateRenderTargetView(&d3d12_back_buffers[i], None, rtv);

            rtv
        });

        let d3d12_dsv = {
            let dsv_begin_ptr = d3d12_dsv_heap.GetCPUDescriptorHandleForHeapStart().ptr;
            let dsv = D3D12_CPU_DESCRIPTOR_HANDLE { ptr: dsv_begin_ptr };

            d3d12_device.CreateDepthStencilView(&d3d12_depth_buffer, None, dsv);

            dsv
        };

        let d3d12_cmd_allocators: [_; FRAME_COUNT as usize] = std::array::from_fn(|_| {
            d3d12_device
                .CreateCommandAllocator::<ID3D12CommandAllocator>(D3D12_COMMAND_LIST_TYPE_DIRECT)
                .unwrap()
        });

        let d3d12_cmd_list = d3d12_device.CreateCommandList1::<ID3D12GraphicsCommandList1>(
            0,
            D3D12_COMMAND_LIST_TYPE_DIRECT,
            D3D12_COMMAND_LIST_FLAG_NONE,
        )?;

        let d3d12_vertex_buffer = ID3D12Resource::new_upload_buffer(&d3d12_device, mesh_geometry.vertices.as_slice())?;
        let d3d12_index_buffer = ID3D12Resource::new_upload_buffer(&d3d12_device, mesh_geometry.indices.as_slice())?;

        let d3d12_root_signature = {
            let mut blob: Option<ID3DBlob> = None;
            let mut error: Option<ID3DBlob> = None;

            let root_params = [D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Constants: D3D12_ROOT_CONSTANTS {
                        ShaderRegister: 0,
                        RegisterSpace: 0,
                        Num32BitValues: 16,
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

            d3d12_device.CreateRootSignature::<ID3D12RootSignature>(0, bytecode)?
        };

        let d3d12_pso = {
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

            d3d12_device.CreateGraphicsPipelineState::<ID3D12PipelineState>(&D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                pRootSignature: ManuallyDrop::new(std::mem::transmute_copy(&d3d12_root_signature)),
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
            ImGui_StyleColorsDark(std::ptr::null_mut());

            let io = &mut *ImGui_GetIO();
            io.ConfigFlags |= ImGuiConfigFlags__ImGuiConfigFlags_NavEnableKeyboard;
            io.ConfigFlags |= ImGuiConfigFlags__ImGuiConfigFlags_DockingEnable;
            io.ConfigFlags |= ImGuiConfigFlags__ImGuiConfigFlags_ViewportsEnable;

            cimgui_implwin32_init(window_handle.0);
            cimgui_impldx12_init(&mut ImGui_ImplDX12_InitInfo {
                device: d3d12_device.as_raw() as *mut _,
                command_queue: d3d12_cmd_queue.as_raw() as *mut _,
                num_frames_in_flight: FRAME_COUNT as i32,
                rtv_format: BACK_BUFFER_FORMAT,
                dsv_format: DEPTH_BUFFER_FORMAT,
                user_data: std::ptr::null_mut(),
                srv_descriptor_heap: d3d12_resource_heap.as_raw() as *mut _,
                srv_descriptor_alloc_fn: None,
                srv_descriptor_free_fn: None,
                legacy_srv_cpu: d3d12_resource_heap.GetCPUDescriptorHandleForHeapStart(),
                legacy_srv_gpu: d3d12_resource_heap.GetGPUDescriptorHandleForHeapStart(),
            });
        }

        let mut cpu_frame_index = 0;
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

            let (dt, fps) = frame_timer.tick();

            let active_frame_index = dxgi_swap_chain.GetCurrentBackBufferIndex();
            let d3d12_cmd_allocator = &d3d12_cmd_allocators[active_frame_index as usize];

            d3d12_cmd_allocator.Reset()?;
            d3d12_cmd_list.Reset(d3d12_cmd_allocator, None)?;

            d3d12_cmd_list.RSSetViewports(&[D3D12_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: WIDTH as f32,
                Height: HEIGHT as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]);

            d3d12_cmd_list.RSSetScissorRects(&[RECT {
                left: 0,
                top: 0,
                right: WIDTH as i32,
                bottom: HEIGHT as i32,
            }]);

            d3d12_cmd_list.ResourceBarrier(&[D3D12_RESOURCE_BARRIER::new_transition(
                &d3d12_back_buffers[active_frame_index as usize],
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            )]);

            let d3d12_rtv = d3d12_rtvs[active_frame_index as usize];

            d3d12_cmd_list.OMSetRenderTargets(1, Some(&d3d12_rtv), false, Some(&d3d12_dsv));
            d3d12_cmd_list.ClearRenderTargetView(d3d12_rtv, &[0.3, 0.3, 0.3, 1.0], None);
            d3d12_cmd_list.ClearDepthStencilView(d3d12_dsv, D3D12_CLEAR_FLAG_DEPTH, 0.0, 0, None);

            d3d12_cmd_list.SetGraphicsRootSignature(&d3d12_root_signature);
            d3d12_cmd_list.SetPipelineState(&d3d12_pso);

            d3d12_cmd_list.SetGraphicsRoot32BitConstants(0, 16, std::ptr::from_ref(&world_to_clip).cast(), 0);

            d3d12_cmd_list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            d3d12_cmd_list.IASetVertexBuffers(
                0,
                Some(&[D3D12_VERTEX_BUFFER_VIEW {
                    BufferLocation: d3d12_vertex_buffer.GetGPUVirtualAddress(),
                    SizeInBytes: std::mem::size_of_val(mesh_geometry.vertices.as_slice()) as u32,
                    StrideInBytes: size_of::<MeshVertex>() as u32,
                }]),
            );
            d3d12_cmd_list.IASetIndexBuffer(Some(&D3D12_INDEX_BUFFER_VIEW {
                BufferLocation: d3d12_index_buffer.GetGPUVirtualAddress(),
                SizeInBytes: std::mem::size_of_val(mesh_geometry.indices.as_slice()) as u32,
                Format: DXGI_FORMAT_R32_UINT,
            }));

            for mesh in &mesh_geometry.meshes {
                d3d12_cmd_list.DrawIndexedInstanced(
                    mesh.index_count,
                    1,
                    mesh.index_offset,
                    mesh.vertex_offset as i32,
                    0,
                );
            }

            {
                cimgui_implwin32_new_frame();
                cimgui_impldx12_new_frame();
                ImGui_NewFrame();

                ImGui_Begin(c"Metrics".as_ptr(), std::ptr::null_mut(), 0);
                let text = std::ffi::CString::new(format!("FPS: {} ({:.2} ms)", fps, dt)).unwrap();
                ImGui_Text(text.as_ptr());
                ImGui_End();

                ImGui_ShowDemoWindow(std::ptr::null_mut());
                ImGui_Render();

                d3d12_cmd_list.SetDescriptorHeaps(&[Some(d3d12_resource_heap.clone())]);
                cimgui_impldx12_render_draw_data(ImGui_GetDrawData(), d3d12_cmd_list.as_raw() as *mut _);

                let io = *ImGui_GetIO();
                if io.ConfigFlags & ImGuiConfigFlags__ImGuiConfigFlags_ViewportsEnable != 0 {
                    ImGui_UpdatePlatformWindows();
                    ImGui_RenderPlatformWindowsDefault();
                }
            }

            d3d12_cmd_list.ResourceBarrier(&[D3D12_RESOURCE_BARRIER::new_transition(
                &d3d12_back_buffers[active_frame_index as usize],
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PRESENT,
            )]);

            d3d12_cmd_list.Close()?;
            let d3d12_cmd_lists: [Option<ID3D12CommandList>; 1] =
                [Some(d3d12_cmd_list.cast::<ID3D12CommandList>().unwrap())];
            d3d12_cmd_queue.ExecuteCommandLists(&d3d12_cmd_lists);

            let code: HRESULT = dxgi_swap_chain.Present(0, DXGI_PRESENT_ALLOW_TEARING);
            if code.is_err() {
                return Err(code.into());
            }

            cpu_frame_index += 1;
            d3d12_cmd_queue.Signal(&d3d12_frame_fence, cpu_frame_index)?;

            let gpu_frame_index = d3d12_frame_fence.GetCompletedValue();

            if cpu_frame_index - gpu_frame_index >= FRAME_COUNT as u64 {
                let gpu_frame_index_to_wait = cpu_frame_index - FRAME_COUNT as u64 + 1;
                wait_for_gpu(&d3d12_frame_fence, wait_event_handle, gpu_frame_index_to_wait)?;
            }
        }

        wait_for_gpu(&d3d12_frame_fence, wait_event_handle, cpu_frame_index)?;

        {
            cimgui_impldx12_shutdown();
            cimgui_implwin32_shutdown();
            ImGui_DestroyContext(std::ptr::null_mut());
        }

        {
            CloseHandle(wait_event_handle)?;

            drop(d3d12_pso);
            drop(d3d12_root_signature);
            drop(d3d12_index_buffer);
            drop(d3d12_vertex_buffer);
            drop(d3d12_cmd_list);
            drop(d3d12_cmd_allocators);
            drop(d3d12_resource_heap);
            drop(d3d12_dsv_heap);
            drop(d3d12_rtv_heap);
            drop(d3d12_depth_buffer);
            drop(d3d12_back_buffers);
            drop(dxgi_swap_chain);
            drop(d3d12_frame_fence);
            drop(d3d12_cmd_queue);

            {
                let debug_device = d3d12_device.cast::<ID3D12DebugDevice>()?;
                debug_device.ReportLiveDeviceObjects(D3D12_RLDO_DETAIL | D3D12_RLDO_IGNORE_INTERNAL)?;
            }

            drop(d3d12_device);
            drop(dxgi_adapter);
            drop(dxgi_factory);
        }

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
    println!("[D3D12][{}]: {}", severity_str, message);
}

fn wide_to_string(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}

fn wait_for_gpu(d3d12_fence: &ID3D12Fence, wait_event_handle: HANDLE, wait_value: u64) -> Result<()> {
    unsafe {
        d3d12_fence.SetEventOnCompletion(wait_value, wait_event_handle)?;

        if WaitForSingleObject(wait_event_handle, INFINITE) == WAIT_FAILED {
            return Err(Error::from_thread());
        }
    }

    Ok(())
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
    fn from_heap_type(d3d12_heap_type: D3D12_HEAP_TYPE) -> Self;
}

impl D3D12HeapPropertiesExt for D3D12_HEAP_PROPERTIES {
    fn from_heap_type(d3d12_heap_type: D3D12_HEAP_TYPE) -> Self {
        Self {
            Type: d3d12_heap_type,
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
    fn new_upload_buffer<T>(d3d12_device: &ID3D12Device, items: &[T]) -> Result<ID3D12Resource>;
}

impl D3D12ResourceExt for ID3D12Resource {
    fn new_upload_buffer<T>(d3d12_device: &ID3D12Device, items: &[T]) -> Result<ID3D12Resource> {
        let mut d3d12_resource: Option<ID3D12Resource> = None;
        unsafe {
            d3d12_device.CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES::from_heap_type(D3D12_HEAP_TYPE_UPLOAD),
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Alignment: 0,
                    Width: std::mem::size_of_val(items) as u64,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: DXGI_FORMAT_UNKNOWN,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    Flags: D3D12_RESOURCE_FLAG_NONE,
                },
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                &mut d3d12_resource,
            )?;
        }

        let d3d12_resource = d3d12_resource.unwrap();

        let mut mapped_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        unsafe { d3d12_resource.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }), Some(&mut mapped_ptr))? };

        let items_ptr = mapped_ptr as *mut T;
        unsafe { std::ptr::copy_nonoverlapping(items.as_ptr(), items_ptr, items.len()) };

        unsafe {
            d3d12_resource.Unmap(
                0,
                Some(&D3D12_RANGE {
                    Begin: 0,
                    End: std::mem::size_of_val(items),
                }),
            );
        }

        Ok(d3d12_resource)
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
        let now = Instant::now();

        Self {
            last_frame: now,
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
