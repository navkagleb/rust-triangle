#include "imgui.h"
#include "imgui_impl_win32.h"
#include "imgui_impl_dx12.h"

// Thin C wrappers — these are the only functions Rust needs to call directly.
// The backend files are compiled as normal C++ separately.

extern "C"
{
    bool cimgui_implwin32_init(void *hwnd)
    {
        return ImGui_ImplWin32_Init(hwnd);
    }

    void cimgui_implwin32_shutdown()
    {
        ImGui_ImplWin32_Shutdown();
    }

    void cimgui_implwin32_new_frame()
    {
        ImGui_ImplWin32_NewFrame();
    }

    bool cimgui_impldx12_init(ImGui_ImplDX12_InitInfo *info)
    {
        return ImGui_ImplDX12_Init(info);
    }

    void cimgui_impldx12_shutdown()
    {
        ImGui_ImplDX12_Shutdown();
    }

    void cimgui_impldx12_new_frame()
    {
        ImGui_ImplDX12_NewFrame();
    }

    void cimgui_impldx12_render_draw_data(ImDrawData *draw_data, ID3D12GraphicsCommandList *cmd_list)
    {
        ImGui_ImplDX12_RenderDrawData(draw_data, cmd_list);
    }
}