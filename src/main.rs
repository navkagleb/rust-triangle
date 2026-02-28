use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const FRAME_COUNT: u32 = 3;

fn main() -> Result<()> {
    println!("Hello D3D12 Rust Triangle!");

    unsafe {
        let exe_handle: HMODULE = GetModuleHandleA(None)?;
        let class_registry_name: PCSTR = s!("rust-window");

        let wc = WNDCLASSA {
            style: CS_VREDRAW | CS_HREDRAW | CS_OWNDC,
            hInstance: exe_handle.into(),
            lpszClassName: class_registry_name,
            lpfnWndProc: Some(handle_window_message),
            ..Default::default()
        };

        let class_atom = RegisterClassA(&wc);
        if class_atom == 0 {
            GetLastError().ok()?;
        }

        let mut window_rect = RECT {
            left: 0,
            top: 0,
            right: WIDTH as i32,
            bottom: HEIGHT as i32,
        };
        AdjustWindowRect(&mut window_rect, WS_OVERLAPPEDWINDOW, false)?;

        let window_handle: HWND = CreateWindowExA(
            WINDOW_EX_STYLE::default(),
            class_registry_name,
            s!("Hello Rust Triangle"),
            WS_OVERLAPPEDWINDOW,
            (GetSystemMetrics(SM_CXSCREEN) - window_rect.right) / 2,
            (GetSystemMetrics(SM_CYSCREEN) - window_rect.bottom) / 2,
            window_rect.right - window_rect.left,
            window_rect.bottom - window_rect.top,
            None,
            None,
            Some(exe_handle.into()),
            None,
        )?;

        println!("{:?}, width: {}, height: {}", window_handle, WIDTH, HEIGHT);

        _ = ShowWindow(window_handle, SW_SHOW);
        _ = UpdateWindow(window_handle);

        {
            let mut d3d12_debug: Option<ID3D12Debug> = None;
            D3D12GetDebugInterface(&mut d3d12_debug)?;

            if let Some(d3d12_debug) = d3d12_debug {
                d3d12_debug.EnableDebugLayer();
                println!("Enable D3D12 debug layer");
            }
        }

        let dxgi_factory = {
            let dxgi_factory_2 = CreateDXGIFactory2::<IDXGIFactory2>(DXGI_CREATE_FACTORY_DEBUG)?;
            dxgi_factory_2.cast::<IDXGIFactory7>()?
        };

        let dxgi_adapter = {
            let mut adapter_index = 0;
            let mut selected_dxgi_adapter: Option<IDXGIAdapter1> = None;

            loop {
                match dxgi_factory.EnumAdapterByGpuPreference::<IDXGIAdapter1>(
                    adapter_index,
                    DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
                ) {
                    Ok(dxgi_adapter) => {
                        let desc = dxgi_adapter.GetDesc1()?;
                        let name = wide_to_string(&desc.Description);

                        println!("Adapter {}: {}", adapter_index, name);

                        adapter_index += 1;

                        if selected_dxgi_adapter.is_none() {
                            selected_dxgi_adapter = Some(dxgi_adapter);
                        }
                    }
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(e),
                }
            }

            selected_dxgi_adapter.unwrap()
        };

        let d3d12_device = {
            let mut d3d12_device: Option<ID3D12Device> = None;
            D3D12CreateDevice(&dxgi_adapter, D3D_FEATURE_LEVEL_12_0, &mut d3d12_device)?;

            d3d12_device.unwrap().cast::<ID3D12Device4>()?
        };

        let mut d3d12_shader_model = D3D12_FEATURE_DATA_SHADER_MODEL {
            HighestShaderModel: D3D_SHADER_MODEL_6_6,
        };
        d3d12_device.CheckFeatureSupport(
            D3D12_FEATURE_SHADER_MODEL,
            std::ptr::addr_of_mut!(d3d12_shader_model) as _,
            size_of::<D3D12_FEATURE_DATA_SHADER_MODEL>() as u32,
        )?;

        println!(
            "Supported shader model: {}.{}",
            d3d12_shader_model.HighestShaderModel.0 / 16,
            d3d12_shader_model.HighestShaderModel.0 % 16
        );

        let d3d12_cmd_queue = {
            let desc = D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                Priority: D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
                ..Default::default()
            };

            d3d12_device.CreateCommandQueue::<ID3D12CommandQueue>(&desc)?
        };

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

            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: WIDTH,
                Height: TOUCH_HIT_TESTING_PROXIMITY_FARTHEST,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                Stereo: BOOL(0),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: FRAME_COUNT,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
                Flags: flags.0 as u32,
            };

            let dxgi_swap_chain = dxgi_factory.CreateSwapChainForHwnd::<&ID3D12CommandQueue, _>(
                &d3d12_cmd_queue,
                window_handle,
                &desc,
                None,
                None,
            )?;

            dxgi_swap_chain.cast::<IDXGISwapChain3>()?
        };

        let d3d12_back_buffers: [ID3D12Resource; FRAME_COUNT as usize] =
            std::array::from_fn(|i| dxgi_swap_chain.GetBuffer(i as u32).unwrap());

        let d3d12_cmd_allocators: [ID3D12CommandAllocator; FRAME_COUNT as usize] =
            std::array::from_fn(|_| {
                d3d12_device
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                    .unwrap()
            });

        let d3d12_cmd_list = d3d12_device.CreateCommandList1::<ID3D12GraphicsCommandList1>(
            0,
            D3D12_COMMAND_LIST_TYPE_DIRECT,
            D3D12_COMMAND_LIST_FLAG_NONE,
        )?;

        loop {
            let mut message = MSG::default();

            while PeekMessageA(&mut message, None, 0, 0, PM_REMOVE).into() {
                _ = TranslateMessage(&message);
                DispatchMessageA(&message);
            }

            if message.message == WM_QUIT {
                break;
            }

            let active_frame_index = dxgi_swap_chain.GetCurrentBackBufferIndex();
            let d3d12_cmd_allocator = &d3d12_cmd_allocators[active_frame_index as usize];

            d3d12_cmd_list.Reset(d3d12_cmd_allocator, None)?;
            d3d12_cmd_list.Close()?;

            let d3d12_cmd_lists: [Option<ID3D12CommandList>; 1] =
                [Some(d3d12_cmd_list.cast::<ID3D12CommandList>().unwrap())];
            d3d12_cmd_queue.ExecuteCommandLists(&d3d12_cmd_lists);

            let code: HRESULT = dxgi_swap_chain.Present(0, DXGI_PRESENT_ALLOW_TEARING);
            if code.is_err() {
                return Err(code.into());
            }
        }

        UnregisterClassA(class_registry_name, Some(exe_handle.into()))?;

        Ok(())
    }
}

extern "system" fn handle_window_message(
    window_handle: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
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
