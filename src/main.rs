use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, PCSTR, Result, s};

fn main() -> Result<()> {
    println!("Hello D3D12 Rust Triangle!");

    let width = 1280;
    let height = 720;

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
            right: width,
            bottom: height,
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

        println!("{:?}, width: {}, height: {}", window_handle, width, height);

        _ = ShowWindow(window_handle, SW_SHOW);
        _ = UpdateWindow(window_handle);

        let dxgi_factory: IDXGIFactory6 = {
            let dxgi_factory_2 = CreateDXGIFactory2::<IDXGIFactory2>(DXGI_CREATE_FACTORY_DEBUG)?;
            dxgi_factory_2.cast()?
        };

        let mut adapter_index = 0;
        loop {
            match dxgi_factory.EnumAdapterByGpuPreference::<IDXGIAdapter1>(
                adapter_index,
                DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
            ) {
                Ok(dxgi_adapter) => {
                    let adapter_desc = dxgi_adapter.GetDesc1()?;
                    let end = adapter_desc
                        .Description
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(adapter_desc.Description.len());
                    let adapter_name = String::from_utf16_lossy(&adapter_desc.Description[..end]);

                    println!("Adapter {}: {}", adapter_index, adapter_name);

                    adapter_index += 1;
                }
                Err(e) => {
                    if e.code() == DXGI_ERROR_NOT_FOUND {
                        break;
                    } else {
                        return Err(e);
                    }
                }
            }
        }

        loop {
            let mut message = MSG::default();

            while PeekMessageA(&mut message, None, 0, 0, PM_REMOVE).into() {
                _ = TranslateMessage(&message);
                DispatchMessageA(&message);
            }

            if message.message == WM_QUIT {
                break;
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
