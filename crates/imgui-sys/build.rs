use std::path::{Path, PathBuf};

fn main() -> anyhow::Result<()> {
    let imgui_dir = Path::new("../../vendor/imgui");
    let imgui_backends_dir = imgui_dir.join("backends");
    let dcimgui_dir = Path::new("../../vendor/dcimgui");

    println!("cargo:rerun-if-changed={}", imgui_dir.display());
    println!("cargo:rerun-if-changed={}", dcimgui_dir.display());

    let imgui_lib = "cimgui";

    cc::Build::new()
        .cpp(true)
        .flag_if_supported("-std=c++17")
        .include(imgui_dir)
        .include(&imgui_backends_dir)
        .include(dcimgui_dir)
        .file(imgui_dir.join("imgui.cpp"))
        .file(imgui_dir.join("imgui_demo.cpp"))
        .file(imgui_dir.join("imgui_draw.cpp"))
        .file(imgui_dir.join("imgui_tables.cpp"))
        .file(imgui_dir.join("imgui_widgets.cpp"))
        .file(imgui_backends_dir.join("imgui_impl_win32.cpp"))
        .file(imgui_backends_dir.join("imgui_impl_dx12.cpp"))
        .file(dcimgui_dir.join("dcimgui.cpp"))
        .file(dcimgui_dir.join("dcimgui_backends_c.cpp"))
        .compile(imgui_lib);

    println!("cargo:rustc-link-lib=static={}", imgui_lib);

    bindgen::Builder::default()
        .header(dcimgui_dir.join("dcimgui.h").to_str().unwrap())
        .clang_arg(format!("-I{}", imgui_dir.display()))
        .clang_arg(format!("-I{}", dcimgui_dir.display()))
        .allowlist_function("ImGui_.*")
        .allowlist_type("Im.*")
        .allowlist_var("ImGui.*")
        .prepend_enum_name(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()?
        .write_to_file(PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("imgui_bindings.rs"))?;

    Ok(())
}
