use std::cmp::Ordering;
use std::sync::Arc;

use glam::Vec2;
use noise::utils::{NoiseMapBuilder, PlaneMapBuilder};
use noise::{Fbm, MultiFractal, Perlin};

use super::config::{
    ATLAS_PATCH_SIZE_IN_PIXELS, ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER, NOISE_WORLD_SCALE, PATCH_SIZE_IN_PIXELS,
    PATCH_WORKER_COUNT,
};
use super::patch::{PatchCoord, PatchPayload};
use super::queue::Queue;

#[derive(Copy, Clone)]
pub struct PatchPriority {
    coverage_required: bool,
    lod: u32,
    distance_squared: f32,
    view_alignment: f32,
}

impl PartialEq for PatchPriority {
    fn eq(&self, other: &Self) -> bool {
        self.coverage_required == other.coverage_required
            && self.lod == other.lod
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
            .then_with(|| self.lod.cmp(&other.lod))
            // Larger alignment means higher priority.
            .then_with(|| self.view_alignment.total_cmp(&other.view_alignment))
            // Smaller distance means higher priority.
            .then_with(|| other.distance_squared.total_cmp(&self.distance_squared))
    }
}

pub struct WantedPatch {
    pub coord: PatchCoord,
    pub priority: PatchPriority,
}

impl WantedPatch {
    pub fn new(coord: PatchCoord, coverage_required: bool, camera_pos: Vec2, camera_forward: Vec2) -> Self {
        let patch_center = coord.world_center();
        let offset = patch_center - camera_pos;
        let view_alignment = if offset.length_squared() >= 0.0001 {
            camera_forward.dot(offset.normalize()).clamp(-1.0, 1.0)
        } else {
            1.0
        };

        Self {
            coord,
            priority: PatchPriority {
                coverage_required,
                lod: coord.lod,
                distance_squared: offset.length_squared(),
                view_alignment,
            },
        }
    }
}

pub struct GeneratedPatch {
    pub coord: PatchCoord,
    pub payload: PatchPayload,
    pub height_range: Vec2,
}

pub struct Generator {
    workers: Vec<std::thread::JoinHandle<()>>,
    queue: Arc<Queue>,
    completed_receiver: std::sync::mpsc::Receiver<GeneratedPatch>,
}

impl Generator {
    pub fn new() -> Self {
        let noise = Arc::new(
            Fbm::<Perlin>::new(123)
                .set_octaves(12)
                .set_frequency(1.0)
                .set_lacunarity(2.0)
                .set_persistence(0.5),
        );
        let queue = Arc::new(Queue::new());
        let (completed_sender, completed_receiver) = std::sync::mpsc::channel::<GeneratedPatch>();

        let workers = (0..PATCH_WORKER_COUNT)
            .map(|_| {
                let noise = Arc::clone(&noise);
                let queue = Arc::clone(&queue);
                let completed_sender = completed_sender.clone();

                std::thread::spawn(move || {
                    while let Some(coord) = queue.claim_blocking() {
                        let generated = Self::generate_patch(&noise, coord);

                        if queue.complete(coord) && completed_sender.send(generated).is_err() {
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

    fn generate_patch(noise: &Fbm<Perlin>, coord: PatchCoord) -> GeneratedPatch {
        let instant = std::time::Instant::now();

        let heights_with_border = Self::generate_heights_with_border(noise, coord);
        let heights = Self::extract_patch_heights(&heights_with_border);
        let gradients = Self::generate_gradients(&heights_with_border, coord);

        let (min_height, max_height) = heights_with_border
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &h| {
                (lo.min(h), hi.max(h))
            });

        println!(
            "Generated: coord=[{:2}, {:3}, {:3}], height=[{:.3}, {:.3}] ({:.2} ms)",
            coord.lod,
            coord.x,
            coord.z,
            min_height,
            max_height,
            instant.elapsed().as_secs_f32() * 1000.0
        );

        GeneratedPatch {
            coord,
            payload: PatchPayload { heights, gradients },
            height_range: Vec2::new(min_height, max_height),
        }
    }

    fn generate_heights_with_border(noise: &Fbm<Perlin>, coord: PatchCoord) -> Vec<f32> {
        let noise_origin = coord.world_origin().as_dvec2() / NOISE_WORLD_SCALE;
        let noise_per_texel = coord.size_in_meters() as f64 / PATCH_SIZE_IN_PIXELS as f64 / NOISE_WORLD_SCALE;

        PlaneMapBuilder::new(noise)
            .set_size(
                ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER as usize,
                ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER as usize,
            )
            .set_x_bounds(
                noise_origin.x - noise_per_texel,
                noise_origin.x + (PATCH_SIZE_IN_PIXELS + 2) as f64 * noise_per_texel,
            )
            .set_y_bounds(
                noise_origin.y - noise_per_texel,
                noise_origin.y + (PATCH_SIZE_IN_PIXELS + 2) as f64 * noise_per_texel,
            )
            .build()
            .into_iter()
            .map(|h| h as f32 * 0.5 + 0.5)
            .collect()
    }

    fn extract_patch_heights(heights_with_border: &[f32]) -> Vec<f32> {
        let texels_per_side = ATLAS_PATCH_SIZE_IN_PIXELS as usize;
        let bordered_texels_per_side = ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER as usize;

        let mut heights = vec![0.0; texels_per_side.pow(2)];

        for z in 0..texels_per_side {
            for x in 0..texels_per_side {
                heights[z * texels_per_side + x] = heights_with_border[(z + 1) * bordered_texels_per_side + (x + 1)];
            }
        }

        heights
    }

    fn generate_gradients(heights_with_border: &[f32], coord: PatchCoord) -> Vec<Vec2> {
        let texels_per_side = ATLAS_PATCH_SIZE_IN_PIXELS as usize;
        let bordered_texels_per_side = ATLAS_PATCH_SIZE_IN_PIXELS_WITH_BORDER as usize;

        // World distance between two neighbouring height samples - the `dx` of the central
        // difference below. Grows with LOD: coarser patches cover more ground per texel.
        let meters_per_texel = coord.size_in_meters() as f32 / PATCH_SIZE_IN_PIXELS as f32;

        let mut gradients = vec![Vec2::ZERO; texels_per_side.pow(2)];

        for z in 0..texels_per_side {
            for x in 0..texels_per_side {
                let sx = x + 1;
                let sz = z + 1;

                let hl = heights_with_border[sz * bordered_texels_per_side + (sx - 1)];
                let hr = heights_with_border[sz * bordered_texels_per_side + (sx + 1)];
                let hb = heights_with_border[(sz - 1) * bordered_texels_per_side + sx];
                let ht = heights_with_border[(sz + 1) * bordered_texels_per_side + sx];

                // Store the horizontal part of the surface normal, `(-dh/dx, -dh/dz)`,
                // so the shader can use it as `(xz.x, 1, xz.y)` with no sign flips.
                // That is why these differences read backwards for a derivative.
                let normal_x = (hl - hr) / (2.0 * meters_per_texel);
                let normal_z = (hb - ht) / (2.0 * meters_per_texel);

                gradients[z * texels_per_side + x] = Vec2::new(normal_x, normal_z);
            }
        }

        gradients
    }
}

impl Drop for Generator {
    fn drop(&mut self) {
        self.queue.shutdown();

        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
