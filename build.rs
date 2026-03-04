use std::fs::{File, create_dir_all, read_dir, remove_file};
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let shaders_dir = Path::new("src/");
    let dxil_dir = Path::new("target").join("dxil");
    create_dir_all(&dxil_dir).expect("Failed to create DXIL directory");

    let dxc_exe = ensure_dxc()?;

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

        match compile_shader(&dxc_exe, &source_path, &dest_path, target) {
            Ok(_) => println!("cargo:warning=[OK] {}", source_path.display()),
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}

fn ensure_dxc() -> std::io::Result<PathBuf> {
    const DXC_VERSION: &str = "v1.9.2602";
    const DXC_ZIP_NAME: &str = "dxc_2026_02_20.zip";
    const DXC_DOWNLOAD_PATH: &str = "https://github.com/microsoft/DirectXShaderCompiler/releases/download";

    let dxc_dir = Path::new("target").join("dxc").join(DXC_VERSION);

    if !dxc_dir.exists() {
        create_dir_all(&dxc_dir)?;

        println!("cargo:warning=Download DXC zip archive...");

        let download_url = format!("{}/{}/{}", DXC_DOWNLOAD_PATH, DXC_VERSION, DXC_ZIP_NAME);
        let raw_zip = Command::new("curl")
            .args([
                "--silent",
                "--fail",
                "--location",
                "--output",
                "-",
                download_url.as_str(),
            ])
            .output()?;

        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(raw_zip.stdout))?;
        for i in 0..zip.len() {
            let mut source_file = zip.by_index(i)?;
            let name = source_file.name().to_string();

            eprintln!("file: {}", source_file.name());

            if source_file.is_dir() || !name.starts_with("bin/x64") {
                continue;
            }

            let filename = name.strip_prefix("bin/x64/").unwrap();
            let mut dest_file = File::create(dxc_dir.join(filename))?;
            std::io::copy(&mut source_file, &mut dest_file)?;
        }
    }

    Ok(dxc_dir.join("dxc.exe"))
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
