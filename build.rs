use std::fs::{File, create_dir_all, read_dir, remove_file};
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=vendor/imgui");
    println!("cargo:rerun-if-changed=vendor/dcimgui");
    println!("cargo:rerun-if-changed=src/shaders");
    println!("cargo:rerun-if-changed=build.rs");

    gen_imgui_bindings();
    compile_shaders();
}

fn gen_imgui_bindings() {
    let imgui_dir = Path::new("vendor").join("imgui");
    let imgui_backends_dir = imgui_dir.join("backends");
    let dcimgui_dir = Path::new("vendor").join("dcimgui");

    cc::Build::new()
        .cpp(true)
        .flag_if_supported("-std=c++17")
        .include(&imgui_dir)
        .include(&imgui_backends_dir)
        .include(&dcimgui_dir)
        .file(imgui_dir.join("imgui.cpp"))
        .file(imgui_dir.join("imgui_demo.cpp"))
        .file(imgui_dir.join("imgui_draw.cpp"))
        .file(imgui_dir.join("imgui_tables.cpp"))
        .file(imgui_dir.join("imgui_widgets.cpp"))
        .file(imgui_backends_dir.join("imgui_impl_win32.cpp"))
        .file(imgui_backends_dir.join("imgui_impl_dx12.cpp"))
        .file(dcimgui_dir.join("dcimgui.cpp"))
        .file(dcimgui_dir.join("dcimgui_backends_c.cpp"))
        .compile("cimgui");

    println!("cargo:rustc-link-lib=static=cimgui");

    let bindings = bindgen::Builder::default()
        .header(dcimgui_dir.join("dcimgui.h").to_str().unwrap())
        .clang_arg(format!("-I{}", imgui_dir.display()))
        .clang_arg(format!("-I{}", dcimgui_dir.display()))
        .allowlist_function("ImGui_.*")
        .allowlist_type("Im.*")
        .allowlist_var("ImGui.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Bindgen failed");

    let dest_bindings_file = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("imgui_bindings.rs");

    bindings
        .write_to_file(dest_bindings_file)
        .expect("Failed to write ImGui bindings");
}

fn compile_shaders() {
    let dxc_exe = Path::new("tools").join("dxc").join("dxc.exe");
    let shaders_dir = Path::new("src").join("shaders");

    let dxil_dir = Path::new("target").join("dxil");
    create_dir_all(&dxil_dir).expect("Failed to create DXIL directory");

    let dir_iter = read_dir(shaders_dir).expect("Failed to read shaders directory");

    for source_path in dir_iter.flatten().map(|e| e.path()) {
        if source_path.extension().and_then(|e| e.to_str()) != Some("hlsl") {
            continue;
        }

        let shader_filename = source_path.file_name().unwrap().to_str().unwrap();
        let shader_type = shader_filename.split('.').rev().nth(1).unwrap();

        let target = match shader_type {
            "vs" => "vs_6_6",
            "ps" => "ps_6_6",
            _ => panic!("Unknown shader type in filename: {:?}", shader_filename),
        };

        let dest_path = dxil_dir.join(shader_filename).with_extension("dxil");
        File::create(&dest_path).expect("Failed to create DXIL file");

        compile_shader(&dxc_exe, &source_path, &dest_path, target);
    }
}

fn compile_shader(dxc_exe: &Path, source: &Path, dest: &Path, target: &str) {
    let result = Command::new(dxc_exe)
        .args(["-T", target, "-E", "Main", "-Fo", dest.to_str().unwrap()])
        .arg(source)
        .output()
        .expect("Failed to run dxc.exe");

    if !result.status.success() {
        _ = remove_file(dest);
        panic!(
            "Failed to compile shader {}.\n{}",
            source.display(),
            String::from_utf8_lossy(&result.stderr)
        );
    }

    println!("cargo:warning=[OK] {}", source.display());
}
