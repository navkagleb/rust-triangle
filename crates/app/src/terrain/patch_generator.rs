use std::cmp::Ordering;
use std::sync::Arc;

use glam::Vec2;
use noise::utils::{NoiseMapBuilder, PlaneMapBuilder};
use noise::{Fbm, MultiFractal, Perlin};

use super::config::{
    ATLAS_PATCH_SIZE_IN_PIXELS, ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER, NOISE_WORLD_SCALE, PATCH_SIZE_IN_PIXELS,
    PATCH_WORKER_COUNT,
};
use super::patch::{PatchData, PatchKey};
use super::patch_queue::PatchQueue;

#[derive(Copy, Clone)]
pub struct PatchPriority {
    coverage_required: bool,
    lod_index: u32,
    distance_squared: f32,
    view_alignment: f32,
}

impl PartialEq for PatchPriority {
    fn eq(&self, other: &Self) -> bool {
        self.coverage_required == other.coverage_required
            && self.lod_index == other.lod_index
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
            // Larger LOD means higher priority.
            .then_with(|| self.lod_index.cmp(&other.lod_index))
            // Larger alignment means higher priority.
            .then_with(|| self.view_alignment.total_cmp(&other.view_alignment))
            // Smaller distance means higher priority.
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
                lod_index: patch.lod_index,
                distance_squared: offset.length_squared(),
                view_alignment,
            },
        }
    }
}

pub struct GeneratedPatch {
    pub patch: PatchKey,
    pub data: PatchData,
}

pub struct PatchGenerator {
    workers: Vec<std::thread::JoinHandle<()>>,
    queue: Arc<PatchQueue>,
    completed_receiver: std::sync::mpsc::Receiver<GeneratedPatch>,
}

impl PatchGenerator {
    pub fn new() -> Self {
        let noise = Arc::new(
            Fbm::<Perlin>::new(123)
                .set_octaves(8)
                .set_frequency(1.0)
                .set_lacunarity(2.0)
                .set_persistence(0.5),
        );
        let queue = Arc::new(PatchQueue::new());
        let (completed_sender, completed_receiver) = std::sync::mpsc::channel::<GeneratedPatch>();

        let workers = (0..PATCH_WORKER_COUNT)
            .map(|_| {
                let noise = Arc::clone(&noise);
                let queue = Arc::clone(&queue);
                let completed_sender = completed_sender.clone();

                std::thread::spawn(move || {
                    while let Some(patch) = queue.claim_blocking() {
                        let generated = Self::generate_patch(&noise, patch);

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

    pub fn update_wanted_patches<I>(&self, wanted_patches: I)
    where
        I: IntoIterator<Item = WantedPatch>,
    {
        self.queue.update_wanted_patches(wanted_patches);
    }

    pub fn drain_generated(&self) -> impl Iterator<Item = GeneratedPatch> + '_ {
        self.completed_receiver.try_iter()
    }

    fn generate_patch(noise: &Fbm<Perlin>, patch: PatchKey) -> GeneratedPatch {
        let instant = std::time::Instant::now();

        let heights_with_border = Self::generate_heights_with_border(noise, patch);
        let heights = Self::extract_patch_heights(&heights_with_border);
        let gradients = Self::generate_gradients(&heights_with_border, patch);

        let (min_height, max_height) = heights_with_border
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &h| {
                (lo.min(h), hi.max(h))
            });

        println!(
            "Generated: index=[{:4}, {:4}], size={:4}, height=[{:.3}, {:.3}] ({:.2} ms)",
            patch.grid_index.x,
            patch.grid_index.y,
            patch.terrain_size(),
            min_height,
            max_height,
            instant.elapsed().as_secs_f32() * 1000.0
        );

        GeneratedPatch {
            patch,
            data: PatchData { heights, gradients },
        }
    }

    fn generate_heights_with_border(noise: &Fbm<Perlin>, patch: PatchKey) -> Vec<f32> {
        let noise_origin = patch.terrain_origin().as_dvec2() / NOISE_WORLD_SCALE;
        let noise_texel = patch.terrain_size() as f64 / PATCH_SIZE_IN_PIXELS as f64 / NOISE_WORLD_SCALE;

        PlaneMapBuilder::new(noise)
            .set_size(
                ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER,
                ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER,
            )
            .set_x_bounds(
                noise_origin.x - noise_texel,
                noise_origin.x + (PATCH_SIZE_IN_PIXELS + 2) as f64 * noise_texel,
            )
            .set_y_bounds(
                noise_origin.y - noise_texel,
                noise_origin.y + (PATCH_SIZE_IN_PIXELS + 2) as f64 * noise_texel,
            )
            .build()
            .into_iter()
            .map(|h| h as f32 * 0.5 + 0.5)
            .collect()
    }

    fn extract_patch_heights(heights_with_border: &[f32]) -> Vec<f32> {
        let mut heights = vec![0.0; ATLAS_PATCH_SIZE_IN_PIXELS.pow(2)];

        for z in 0..ATLAS_PATCH_SIZE_IN_PIXELS {
            for x in 0..ATLAS_PATCH_SIZE_IN_PIXELS {
                heights[z * ATLAS_PATCH_SIZE_IN_PIXELS + x] =
                    heights_with_border[(z + 1) * ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER + (x + 1)];
            }
        }

        heights
    }

    fn generate_gradients(heights_with_border: &[f32], patch: PatchKey) -> Vec<Vec2> {
        let texel_terrain_size = patch.terrain_size() as f32 / PATCH_SIZE_IN_PIXELS as f32;

        let mut gradients = vec![Vec2::ZERO; ATLAS_PATCH_SIZE_IN_PIXELS.pow(2)];

        for z in 0..ATLAS_PATCH_SIZE_IN_PIXELS {
            for x in 0..ATLAS_PATCH_SIZE_IN_PIXELS {
                let sx = x + 1;
                let sz = z + 1;

                let hl = heights_with_border[sz * ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER + (sx - 1)];
                let hr = heights_with_border[sz * ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER + (sx + 1)];
                let hb = heights_with_border[(sz - 1) * ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER + sx];
                let ht = heights_with_border[(sz + 1) * ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER + sx];

                let dhdx = (hl - hr) / (2.0 * texel_terrain_size);
                let dhdz = (hb - ht) / (2.0 * texel_terrain_size);

                gradients[z * ATLAS_PATCH_SIZE_IN_PIXELS + x] = Vec2::new(dhdx, dhdz);
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
