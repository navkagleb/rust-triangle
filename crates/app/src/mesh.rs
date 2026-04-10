use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use glam::{Mat4, Quat, Vec3};
use rand::prelude::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::System::Threading::GetCurrentThreadId;

use crate::{D3D12ResourceExt, measure};

#[allow(dead_code)]
pub struct MeshVertex {
    position: (f32, f32, f32),
    normal: (f32, f32, f32),
}

#[derive(Copy, Clone)]
pub struct MeshDraw {
    pub vertex_offset: u32,
    pub vertex_count: u32,
    pub index_offset: u32,
    pub index_count: u32,
}

pub struct Mesh {
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u32>,
    pub draws: Vec<MeshDraw>,
}

impl Mesh {
    fn from(gltf: gltf::Document, buffers: Vec<gltf::buffer::Data>) -> Option<Self> {
        assert_eq!(gltf.scenes().len(), 1);
        assert_eq!(gltf.nodes().len(), 1);

        let mut vertex_count = 0;
        let mut index_count = 0;
        let mut draws = Vec::new();

        for mesh in gltf.meshes() {
            for primitive in mesh.primitives() {
                let position_accessor = primitive.get(&gltf::Semantic::Positions)?;
                let normal_accessor = primitive.get(&gltf::Semantic::Normals)?;

                assert_eq!(position_accessor.count(), normal_accessor.count());
                assert_eq!(primitive.mode(), gltf::mesh::Mode::Triangles);

                let draw = MeshDraw {
                    vertex_offset: vertex_count,
                    vertex_count: position_accessor.count() as u32,
                    index_offset: index_count,
                    index_count: primitive.indices()?.count() as u32,
                };

                vertex_count += draw.vertex_count;
                index_count += draw.index_count;
                draws.push(draw);
            }
        }

        let mut result = Self {
            vertices: Vec::with_capacity(vertex_count as usize),
            indices: Vec::with_capacity(index_count as usize),
            draws,
        };

        for mesh in gltf.meshes() {
            for primitive in mesh.primitives() {
                let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));

                result
                    .vertices
                    .extend(std::iter::zip(reader.read_positions()?, reader.read_normals()?).map(
                        |(position, normal)| MeshVertex {
                            position: position.into(),
                            normal: normal.into(),
                        },
                    ));

                result.indices.extend(reader.read_indices()?.into_u32());
            }
        }

        Some(result)
    }
}

pub struct GpuMesh {
    pub local_to_world: Mat4,
    pub draws: Vec<MeshDraw>,
    pub vertex_buffer: ID3D12Resource,
    pub index_buffer: ID3D12Resource,
    pub vbv: D3D12_VERTEX_BUFFER_VIEW,
    pub ibv: D3D12_INDEX_BUFFER_VIEW,
}

impl GpuMesh {
    fn new(device: &ID3D12Device, mesh: &Mesh, local_to_world: Mat4) -> Option<Self> {
        let vertex_buffer =
            ID3D12Resource::new_buf(device, D3D12_HEAP_TYPE_DEFAULT, size_of_val(mesh.vertices.as_slice())).ok()?;
        let index_buffer =
            ID3D12Resource::new_buf(device, D3D12_HEAP_TYPE_DEFAULT, size_of_val(mesh.indices.as_slice())).ok()?;

        Some(Self {
            local_to_world,
            draws: mesh.draws.clone(),
            vbv: D3D12_VERTEX_BUFFER_VIEW {
                BufferLocation: unsafe { vertex_buffer.GetGPUVirtualAddress() },
                SizeInBytes: size_of_val(mesh.vertices.as_slice()) as u32,
                StrideInBytes: size_of::<MeshVertex>() as u32,
            },
            ibv: D3D12_INDEX_BUFFER_VIEW {
                BufferLocation: unsafe { index_buffer.GetGPUVirtualAddress() },
                SizeInBytes: size_of_val(mesh.indices.as_slice()) as u32,
                Format: DXGI_FORMAT_R32_UINT,
            },
            vertex_buffer,
            index_buffer,
        })
    }
}

type GpuMeshJob = Box<dyn FnOnce() + Send + 'static>;

pub struct LoadedMesh(pub Mesh, pub GpuMesh);

pub struct LoadThreadPool {
    workers: Vec<std::thread::JoinHandle<()>>,
    job_sender: Option<Sender<GpuMeshJob>>,
    loaded_mesh_sender: Sender<Result<LoadedMesh>>,
}

impl LoadThreadPool {
    pub fn new(thread_count: usize, loaded_mesh_sender: Sender<Result<LoadedMesh>>) -> Self {
        let (job_sender, job_receiver) = std::sync::mpsc::channel::<GpuMeshJob>();
        let job_receiver = Arc::new(Mutex::new(job_receiver));

        let workers = (0..thread_count)
            .map(|i| {
                let job_receiver = Arc::clone(&job_receiver);

                std::thread::Builder::new()
                    .name(format!("worker-{}", i))
                    .spawn(move || {
                        loop {
                            let job = job_receiver.lock().unwrap().recv();

                            match job {
                                Ok(job) => job(),
                                Err(_) => break,
                            };
                        }
                    })
                    .unwrap()
            })
            .collect();

        Self {
            workers,
            job_sender: Some(job_sender),
            loaded_mesh_sender,
        }
    }

    pub fn submit(&self, device: &Arc<ID3D12Device4>, path: std::path::PathBuf) {
        let device = Arc::clone(device);
        let loaded_mesh_sender = self.loaded_mesh_sender.clone();

        let job = move || {
            let result = (|| {
                let (gltf_import, gltf_import_ms) = measure(|| gltf::import(path.as_path()));
                let (gltf, buffers, _) = gltf_import.with_context(|| format!("Failed to import gltf: {:?}", path))?;

                let (mesh, mesh_ms) = measure(|| Mesh::from(gltf, buffers));
                let mesh = mesh.context("Failed to parse mesh")?;

                let mut rng = rand::rng();

                let scale = Vec3::splat(rng.random_range(0.01..0.1));
                let rotation = Quat::from_euler(
                    glam::EulerRot::XYX,
                    rng.random_range(0.0..std::f32::consts::TAU),
                    rng.random_range(0.0..std::f32::consts::TAU),
                    rng.random_range(0.0..std::f32::consts::TAU),
                );
                let translation = Vec3::new(
                    rng.random_range(-50.0..50.0),
                    rng.random_range(-40.0..40.0),
                    rng.random_range(35.0..60.0),
                );

                let (gpu_mesh, gpu_mesh_ms) = measure(|| {
                    GpuMesh::new(
                        &device,
                        &mesh,
                        Mat4::from_scale_rotation_translation(scale, rotation, translation),
                    )
                });
                let gpu_mesh = gpu_mesh.context("Failed to create GpuMesh")?;

                println!(
                    "[{:6}][LT] gltf: {:.2}ms | parse: {:.2}ms | gpu: {:.2}ms",
                    unsafe { GetCurrentThreadId() },
                    gltf_import_ms,
                    mesh_ms,
                    gpu_mesh_ms,
                );

                Ok(LoadedMesh(mesh, gpu_mesh))
            })();

            loaded_mesh_sender.send(result).unwrap();
        };

        self.job_sender.as_ref().unwrap().send(Box::new(job)).unwrap();
    }
}

impl Drop for LoadThreadPool {
    fn drop(&mut self) {
        drop(self.job_sender.take());

        for worker in self.workers.drain(..) {
            worker.join().unwrap();
        }
    }
}
