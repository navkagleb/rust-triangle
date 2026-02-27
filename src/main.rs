use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::ValidateRect;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Result, s};

fn main() -> Result<()> {
    println!("Hello D3D12 Rust Triangle!");

    unsafe {
        let exe_handle: HMODULE = GetModuleHandleA(None)?;
        let window_class = s!("rust-window");

        let wc = WNDCLASSA {
            hInstance: exe_handle.into(),
            lpszClassName: window_class,

            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(message_handler),

            ..Default::default()
        };

        let atom = RegisterClassA(&wc);
        assert!(atom != 0);

        CreateWindowExA(
            WINDOW_EX_STYLE::default(),
            window_class,
            s!("Hello Rust Triangle"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            Some(exe_handle.into()),
            None,
        )?;

        let mut message = MSG::default();

        while GetMessageA(&mut message, None, 0, 0).into() {
            DispatchMessageA(&message);
        }

        UnregisterClassA(window_class, Some(exe_handle.into()))?;

        Ok(())
    }
}

extern "system" fn message_handler(
    window_handle: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_PAINT => {
                println!("WM_PAINT");
                _ = ValidateRect(Some(window_handle), None);
                LRESULT(0)
            }
            WM_DESTROY => {
                println!("WM_DESTROY");
                PostQuitMessage(0);
                LRESULT(0)
            }
            // DestroyWindow is handled by DefWindowProcA
            _ => DefWindowProcA(window_handle, message, wparam, lparam),
        }
    }
}
