use std::fmt;
use std::fs::{create_dir_all, read_dir, read_to_string, remove_file};
use std::path::Path;

use anyhow::Context;

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ShaderType {
    Vs,
    Ps,
}

impl ShaderType {
    fn entry_point(&self) -> &str {
        match self {
            ShaderType::Vs => "vs_main",
            ShaderType::Ps => "ps_main",
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

fn main() -> anyhow::Result<()> {
    let dxc_exe = Path::new("../../tools/dxc/dxc.exe");
    let shaders_dir = Path::new("src/shaders");
    let dxil_dir = Path::new("../../target/dxil");

    println!("cargo:rerun-if-changed={}", shaders_dir.display());
    println!("cargo:rerun-if-changed={}", dxc_exe.display());

    create_dir_all(dxil_dir)?;

    let shaders_file = read_to_string("src/shaders/shaders.json")?;
    let shaders = serde_json::from_str::<std::collections::HashMap<String, Vec<ShaderType>>>(&shaders_file)?;

    for entry in read_dir(shaders_dir)?.flatten() {
        let source_path = entry.path();

        if source_path.extension().and_then(|e| e.to_str()) != Some("hlsl") {
            continue;
        }

        let shader_filename = source_path
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid shader filename")?;

        let shader_types = &shaders[shader_filename];

        for shader_type in shader_types {
            let dest_path = dxil_dir
                .join(shader_filename)
                .with_extension(format!("{}.dxil", shader_type));

            compile_shader(dxc_exe, &source_path, &dest_path, shader_type)?;
        }
    }

    Ok(())
}

fn compile_shader(dxc_exe: &Path, source: &Path, dest: &Path, shader_type: &ShaderType) -> anyhow::Result<()> {
    let result = std::process::Command::new(dxc_exe)
        .args([
            "-T",
            shader_type.target(),
            "-E",
            shader_type.entry_point(),
            "-Fo",
            dest.to_str().unwrap(),
        ])
        .arg(source)
        .output()?;

    if !result.status.success() {
        _ = remove_file(dest);

        anyhow::bail!(
            "Failed to compile shader {} + {}.\n{}",
            source.display(),
            shader_type,
            String::from_utf8_lossy(&result.stderr)
        );
    }

    Ok(())
}
