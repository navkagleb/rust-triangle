use std::cmp::Ordering;
use std::sync::Arc;

use glam::Vec2;
use noise::utils::{NoiseMapBuilder, PlaneMapBuilder};
use noise::{Fbm, MultiFractal, Perlin};

use super::config::*;
use super::patch::PatchKey;
use super::patch_queue::PatchQueue;

#[derive(Copy, Clone)]
pub struct PatchPriority {
    coverage_required: bool,
    distance_squared: f32,
    view_alignment: f32,
}

impl PartialEq for PatchPriority {
    fn eq(&self, other: &Self) -> bool {
        self.coverage_required == other.coverage_required
            && self.distance_squared == other.distance_squared
            && self.view_alignment == other.view_alignment
    }
}

impl Eq for PatchPriority {}

impl PartialOrd for PatchPriority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PatchPriority {
    fn cmp(&self, other: &Self) -> Ordering {
        self.coverage_required
            .cmp(&other.coverage_required)
            // Smaller distance means higher priority.
            .then_with(|| self.view_alignment.total_cmp(&other.view_alignment))
            // Larger alignment means higher priority.
            .then_with(|| other.distance_squared.total_cmp(&self.distance_squared))
    }
}

pub struct WantedPatch {
    pub(super) patch: PatchKey,
    pub(super) priority: PatchPriority,
}

impl WantedPatch {
    pub fn new(patch: PatchKey, coverage_required: bool, camera_pos: Vec2, camera_forward: Vec2) -> Self {
        let patch_center = patch.terrain_center().as_vec2();
        let offset = patch_center - camera_pos;
        let view_alignment = if offset.length_squared() >= 0.0001 {
            camera_forward.dot(offset.normalize()).clamp(-1.0, 1.0)
        } else {
            1.0
        };

        Self {
            patch,
            priority: PatchPriority {
                coverage_required,
                distance_squared: offset.length_squared(),
                view_alignment,
            },
        }
    }
}

pub struct GeneratedPatch {
    pub patch: PatchKey,
    pub heights: Vec<f32>,
    pub gradients: Vec<Vec2>,
}

pub struct PatchGenerator {
    workers: Vec<std::thread::JoinHandle<()>>,
    queue: Arc<PatchQueue>,
    completed_receiver: std::sync::mpsc::Receiver<GeneratedPatch>,
}

impl PatchGenerator {
    pub fn new() -> Self {
        let queue = Arc::new(PatchQueue::new());
        let (completed_sender, completed_receiver) = std::sync::mpsc::channel::<GeneratedPatch>();

        let fbm = Fbm::<Perlin>::new(123)
            .set_octaves(8)
            .set_frequency(1.0)
            .set_lacunarity(2.0)
            .set_persistence(0.5);

        let workers = (0..PATCH_GEN_THREAD_COUNT)
            .map(|_| {
                let queue = Arc::clone(&queue);
                let completed_sender = completed_sender.clone();
                let fbm = fbm.clone();

                std::thread::spawn(move || {
                    while let Some(patch) = queue.claim_blocking() {
                        let generated = Self::generate_patch(&fbm, patch);

                        if queue.complete(patch) && completed_sender.send(generated).is_err() {
                            break;
                        }
                    }
                })
            })
            .collect();

        Self {
            workers,
            queue,
            completed_receiver,
        }
    }

    pub fn update_wanted_patches(&self, wanted_patches: &[WantedPatch]) {
        self.queue.update_wanted_patches(wanted_patches);
    }

    pub fn drain_generated(&self) -> impl Iterator<Item = GeneratedPatch> + '_ {
        self.completed_receiver.try_iter()
    }

    fn generate_patch(fbm: &Fbm<Perlin>, patch: PatchKey) -> GeneratedPatch {
        let instant = std::time::Instant::now();

        let heights_with_border = Self::generate_heights_with_border(fbm, patch);
        let heights = Self::extract_patch_heights(&heights_with_border);
        let gradients = Self::generate_gradients(&heights_with_border, patch);

        println!(
            "Generated: index=[{:4}, {:4}], size={:4} ({:.2} ms)",
            patch.grid_index.x,
            patch.grid_index.y,
            patch.terrain_size(),
            instant.elapsed().as_secs_f32() * 1000.0
        );

        GeneratedPatch {
            patch,
            heights,
            gradients,
        }
    }

    fn generate_heights_with_border(fbm: &Fbm<Perlin>, patch: PatchKey) -> Vec<f32> {
        let fbm_pos = patch.terrain_origin().as_dvec2() / NOISE_WORLD_SCALE * NOISE_SCALE;
        let fbm_size = patch.terrain_size() as f64 / NOISE_WORLD_SCALE * NOISE_SCALE;
        let fbm_pixel_size = fbm_size / PATCH_PIXEL_SIZE as f64;

        PlaneMapBuilder::new(fbm)
            .set_size(ATLAS_PATCH_PIXEL_SIZE_WITH_BORDER, ATLAS_PATCH_PIXEL_SIZE_WITH_BORDER)
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
                    heights_with_border[(z + 1) * ATLAS_PATCH_PIXEL_SIZE_WITH_BORDER + (x + 1)];
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

                let hl = heights_with_border[sz * ATLAS_PATCH_PIXEL_SIZE_WITH_BORDER + (sx - 1)];
                let hr = heights_with_border[sz * ATLAS_PATCH_PIXEL_SIZE_WITH_BORDER + (sx + 1)];
                let hb = heights_with_border[(sz - 1) * ATLAS_PATCH_PIXEL_SIZE_WITH_BORDER + sx];
                let ht = heights_with_border[(sz + 1) * ATLAS_PATCH_PIXEL_SIZE_WITH_BORDER + sx];

                let dhdx = (hl - hr) / (2.0 * texel_terrain_size);
                let dhdz = (hb - ht) / (2.0 * texel_terrain_size);

                gradients[z * ATLAS_PATCH_PIXEL_SIZE + x] = Vec2::new(dhdx, dhdz);
            }
        }

        gradients
    }
}

impl Drop for PatchGenerator {
    fn drop(&mut self) {
        self.queue.shutdown();

        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
