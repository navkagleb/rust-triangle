use core::fmt;
use std::collections::HashMap;
use std::fs::{File, create_dir_all, read_dir, read_to_string, remove_file};
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

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ShaderType {
    Vs,
    Ps,
}

impl ShaderType {
    fn entry_point(&self) -> &str {
        match self {
            ShaderType::Vs => "VsMain",
            ShaderType::Ps => "PsMain",
        }
    }

    fn target(&self) -> &str {
        match self {
            ShaderType::Vs => "vs_6_6",
            ShaderType::Ps => "ps_6_6",
        }
    }
}

impl fmt::Display for ShaderType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ShaderType::Vs => write!(f, "vs"),
            ShaderType::Ps => write!(f, "ps"),
        }
    }
}

fn compile_shaders() {
    let dxc_exe = Path::new("tools").join("dxc").join("dxc.exe");
    let shaders_dir = Path::new("src").join("shaders");

    let dxil_dir = Path::new("target").join("dxil");
    create_dir_all(&dxil_dir).expect("Failed to create DXIL directory");

    let shaders_file = read_to_string("src/shaders/shaders.json").expect("No shaders.json file");
    let shaders = serde_json::from_str::<HashMap<String, Vec<ShaderType>>>(&shaders_file)
        .expect("Failed to deserialize shaders.json");

    let dir_iter = read_dir(shaders_dir).expect("Failed to read shaders directory");

    for source_path in dir_iter.flatten().map(|e| e.path()) {
        if source_path.extension().and_then(|e| e.to_str()) != Some("hlsl") {
            continue;
        }

        let shader_filename = source_path.file_name().unwrap().to_str().unwrap();
        let shader_types = &shaders[shader_filename];

        for shader_type in shader_types {
            let dest_path = dxil_dir
                .join(shader_filename)
                .with_extension(format!("{}.dxil", shader_type));
            File::create(&dest_path).expect("Failed to create DXIL file");

            compile_shader(&dxc_exe, &source_path, &dest_path, shader_type);
        }
    }
}

fn compile_shader(dxc_exe: &Path, source: &Path, dest: &Path, shader_type: &ShaderType) {
    let result = Command::new(dxc_exe)
        .args([
            "-T",
            shader_type.target(),
            "-E",
            shader_type.entry_point(),
            "-Fo",
            dest.to_str().unwrap(),
        ])
        .arg(source)
        .output()
        .expect("Failed to run dxc.exe");

    if !result.status.success() {
        _ = remove_file(dest);
        panic!(
            "Failed to compile shader {} + {}.\n{}",
            source.display(),
            shader_type,
            String::from_utf8_lossy(&result.stderr)
        );
    }

    println!(
        "cargo:warning=[OK] {} + {} -> {}",
        source.display(),
        shader_type,
        dest.display()
    );
}
