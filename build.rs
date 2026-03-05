use std::fs::{File, create_dir_all, read_dir, remove_file};
use std::path::Path;
use std::process::Command;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let shaders_dir = Path::new("src");
    let dxil_dir = Path::new("target/dxil");
    create_dir_all(dxil_dir).expect("Failed to create DXIL directory");

    let dxc_exe = Path::new("tools/dxc/dxc.exe");

    for source_path in read_dir(shaders_dir)?.flatten().map(|e| e.path()) {
        if source_path.extension().and_then(|e| e.to_str()) != Some("hlsl") {
            continue;
        }

        let shader_filename = source_path.file_name().unwrap().to_str().unwrap();
        let shader_type = shader_filename.split('.').rev().nth(1).unwrap();

        let target = match shader_type {
            "vs" => "vs_6_6",
            "ps" => "ps_6_6",
            _ => return Err(format!("Unknown shader type in filename: {:?}", shader_filename).into()),
        };

        let dest_path = dxil_dir.join(shader_filename).with_extension("dxil");
        File::create(&dest_path)?;

        match compile_shader(dxc_exe, &source_path, &dest_path, target) {
            Ok(_) => println!("cargo:warning=[OK] {}", source_path.display()),
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}

fn compile_shader(dxc_exe: &Path, source: &Path, dest: &Path, target: &str) -> std::result::Result<(), String> {
    let result = Command::new(dxc_exe)
        .args(["-T", target, "-E", "Main", "-Fo", dest.to_str().unwrap()])
        .arg(source)
        .output()
        .map_err(|e| e.to_string())?;

    if !result.status.success() {
        remove_file(dest).map_err(|e| e.to_string())?;
        return Err(format!("{}", String::from_utf8_lossy(&result.stderr)));
    }

    Ok(())
}
