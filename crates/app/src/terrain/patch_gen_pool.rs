use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use glam::Vec2;
use noise::utils::{NoiseMapBuilder, PlaneMapBuilder};
use noise::{Fbm, MultiFractal, Perlin};

use super::config::*;
use super::patch::PatchKey;

type PatchGenRequest = PatchKey;

pub(super) struct PatchGenResult {
    pub(super) patch: PatchKey,
    pub(super) heights: Vec<f32>,
    pub(super) gradients: Vec<Vec2>,
}

pub(super) struct PatchGenPool {
    workers: Vec<std::thread::JoinHandle<()>>,
    request_sender: Option<Sender<PatchGenRequest>>,
    result_receiver: Receiver<PatchGenResult>,
}

impl PatchGenPool {
    pub(super) fn new() -> Self {
        let (request_sender, request_receiver) = std::sync::mpsc::channel::<PatchGenRequest>();
        let (result_sender, result_receiver) = std::sync::mpsc::channel::<PatchGenResult>();

        let request_receiver = Arc::new(Mutex::new(request_receiver));

        let fbm = Fbm::<Perlin>::new(123)
            .set_octaves(8)
            .set_frequency(1.0)
            .set_lacunarity(2.0)
            .set_persistence(0.5);

        let workers = (0..PATCH_GEN_WORKER_COUNT)
            .map(|i| {
                let request_receiver = Arc::clone(&request_receiver);
                let result_sender = result_sender.clone();
                let fbm = fbm.clone();

                std::thread::Builder::new()
                    .name(format!("tile-generator-{}", i))
                    .spawn(move || {
                        loop {
                            let request = request_receiver.lock().unwrap().recv();
                            let Ok(patch) = request else {
                                break;
                            };

                            let instant = std::time::Instant::now();

                            let heights_with_border = Self::generate_heights_with_border(&fbm, patch);
                            let heights = Self::extract_patch_heights(&heights_with_border);
                            let gradients = Self::generate_gradients(&heights_with_border, patch);

                            {
                                let ms = instant.elapsed().as_secs_f32() * 1000.0;
                                let min = heights_with_border.iter().cloned().fold(f32::INFINITY, f32::min);
                                let max = heights_with_border.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

                                println!(
                                    "grid={}, lod={}, min={}, max={} ({:.2} ms)",
                                    patch.grid_index, patch.lod_index, min, max, ms
                                );
                            }

                            result_sender
                                .send(PatchGenResult {
                                    patch,
                                    heights,
                                    gradients,
                                })
                                .unwrap();
                        }
                    })
                    .unwrap()
            })
            .collect();

        Self {
            workers,
            request_sender: Some(request_sender),
            result_receiver,
        }
    }

    pub(super) fn request_patch_generation(&self, request: PatchGenRequest) {
        self.request_sender.as_ref().unwrap().send(request).unwrap()
    }

    pub(super) fn drain_results(&self) -> impl Iterator<Item = PatchGenResult> + '_ {
        self.result_receiver.try_iter()
    }

    fn generate_heights_with_border(fbm: &Fbm<Perlin>, patch: PatchKey) -> Vec<f32> {
        let fbm_pos = patch.terrain_origin().as_dvec2() / NOISE_WORLD_SCALE * NOISE_SCALE;
        let fbm_size = patch.terrain_size() as f64 / NOISE_WORLD_SCALE * NOISE_SCALE;
        let fbm_pixel_size = fbm_size / PATCH_PIXEL_SIZE as f64;

        PlaneMapBuilder::new(fbm)
            .set_size(ALTAS_PATCH_PIXEL_SIZE_WITH_BORDER, ALTAS_PATCH_PIXEL_SIZE_WITH_BORDER)
            .set_x_bounds(fbm_pos.x - fbm_pixel_size, fbm_pos.x + fbm_size + fbm_pixel_size * 2.0)
            .set_y_bounds(fbm_pos.y - fbm_pixel_size, fbm_pos.y + fbm_size + fbm_pixel_size * 2.0)
            .build()
            .into_iter()
            .map(|h| {
                let h = h as f32 * 1.15 + 0.4;
                h.clamp(0.0, 1.0)
            })
            .collect()
    }

    fn extract_patch_heights(heights_with_border: &[f32]) -> Vec<f32> {
        let mut heights = vec![0.0; ATLAS_PATCH_PIXEL_SIZE.pow(2)];

        for z in 0..ATLAS_PATCH_PIXEL_SIZE {
            for x in 0..ATLAS_PATCH_PIXEL_SIZE {
                heights[z * ATLAS_PATCH_PIXEL_SIZE + x] =
                    heights_with_border[(z + 1) * ALTAS_PATCH_PIXEL_SIZE_WITH_BORDER + (x + 1)];
            }
        }

        heights
    }

    fn generate_gradients(heights_with_border: &[f32], patch: PatchKey) -> Vec<Vec2> {
        let texel_terrain_size = patch.terrain_size() as f32 / PATCH_PIXEL_SIZE as f32;

        let mut gradients = vec![Vec2::ZERO; ATLAS_PATCH_PIXEL_SIZE.pow(2)];

        for z in 0..ATLAS_PATCH_PIXEL_SIZE {
            for x in 0..ATLAS_PATCH_PIXEL_SIZE {
                let sx = x + 1;
                let sz = z + 1;

                let hl = heights_with_border[sz * ALTAS_PATCH_PIXEL_SIZE_WITH_BORDER + (sx - 1)];
                let hr = heights_with_border[sz * ALTAS_PATCH_PIXEL_SIZE_WITH_BORDER + (sx + 1)];
                let hb = heights_with_border[(sz - 1) * ALTAS_PATCH_PIXEL_SIZE_WITH_BORDER + sx];
                let ht = heights_with_border[(sz + 1) * ALTAS_PATCH_PIXEL_SIZE_WITH_BORDER + sx];

                let dhdx = (hl - hr) / (2.0 * texel_terrain_size);
                let dhdz = (hb - ht) / (2.0 * texel_terrain_size);

                gradients[z * ATLAS_PATCH_PIXEL_SIZE + x] = Vec2::new(dhdx, dhdz);
            }
        }

        gradients
    }
}

impl Drop for PatchGenPool {
    fn drop(&mut self) {
        drop(self.request_sender.take());

        for worker in self.workers.drain(..) {
            worker.join().unwrap();
        }
    }
}
