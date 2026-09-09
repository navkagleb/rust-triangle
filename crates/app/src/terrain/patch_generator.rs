use std::cmp::Ordering;
use std::sync::Arc;

use glam::{DVec2, Vec2};
use noise::{NoiseFn, Perlin};

use super::config::*;
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
        let noise = Arc::new(TerrainNoise::new(123));
        let queue = Arc::new(PatchQueue::new());
        let (completed_sender, completed_receiver) = std::sync::mpsc::channel::<GeneratedPatch>();

        let workers = (0..PATCH_GEN_THREAD_COUNT)
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

    fn generate_patch(noise: &TerrainNoise, patch: PatchKey) -> GeneratedPatch {
        let instant = std::time::Instant::now();

        let heights_with_border = Self::generate_heights_with_border(noise, patch);
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
            data: PatchData { heights, gradients },
        }
    }

    fn generate_heights_with_border(noise: &TerrainNoise, patch: PatchKey) -> Vec<f32> {
        let noise_origin = patch.terrain_origin().as_dvec2() / NOISE_WORLD_SCALE * NOISE_SCALE;
        let noise_texel = patch.terrain_size() as f64 / PATCH_PIXEL_SIZE as f64 / NOISE_WORLD_SCALE * NOISE_SCALE;
        let octave_count = (9 - patch.lod_index as usize).min(OCTAVE_COUNT);

        let mut heights = Vec::with_capacity(ATLAS_PATCH_PIXEL_SIZE_WITH_BORDER.pow(2));

        for z in 0..ATLAS_PATCH_PIXEL_SIZE_WITH_BORDER {
            for x in 0..ATLAS_PATCH_PIXEL_SIZE_WITH_BORDER {
                // -1 skips the border texel, so index 1 lands exactly on the patch origin.
                let offset = DVec2::new(x as f64 - 1.0, z as f64 - 1.0) * noise_texel;
                heights.push(noise.height(noise_origin + offset, octave_count));
            }
        }

        heights
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

const OCTAVE_COUNT: usize = 8;
const PERSISTENCE: f64 = 0.5;
const LACUNARITY: f64 = 2.0;
const MEADOW_BIAS: f32 = 1.0; // Higher values confine mountains to fewer, more exceptional regions
const MEADOW_RELIEF: f32 = 0.25; // Fraction of the height range that meadow terrain is allowed to use

pub struct TerrainNoise {
    octaves: Vec<Perlin>,
    // Always normalizes by the FULL octave amplitude sum, so summing fewer
    // octaves is a strict low-pass of the same field, not a rescaled one.
    inv_amplitude_sum: f64,
}

impl TerrainNoise {
    pub fn new(seed: u32) -> Self {
        let octaves = (0..OCTAVE_COUNT).map(|i| Perlin::new(seed + i as u32)).collect();
        let amplitude_sum: f64 = (0..OCTAVE_COUNT).map(|i| PERSISTENCE.powi(i as i32)).sum();

        Self {
            octaves,
            inv_amplitude_sum: 1.0 / amplitude_sum,
        }
    }

    fn fbm(&self, mut p: DVec2, octave_count: usize) -> f64 {
        let mut result = 0.0;
        let mut amplitude = 1.0;

        for perlin in &self.octaves[..octave_count.min(OCTAVE_COUNT)] {
            result += perlin.get(p.to_array()) * amplitude;
            amplitude *= PERSISTENCE;
            p *= LACUNARITY;
        }

        result * self.inv_amplitude_sum
    }

    pub fn height(&self, p: DVec2, octave_count: usize) -> f32 {
        // Large-scale elevation: where land is generally high or low.
        let continent = self.fbm(p * 0.25, octave_count.min(4)) as f32;

        // 0 = meadow, 1 = mountain. powf leaves full peaks alone (1^k == 1) while
        // pulling the whole transition band toward meadow.
        let mountainness = smoothstep(0.05, 0.45, continent).powf(MEADOW_BIAS);

        // Gentle rolling fields: one octave fewer than the mountains, since the
        // doubled frequency would otherwise sit right at Nyquist.
        let meadow = (self.fbm(p * 2.0, octave_count.saturating_sub(1)) as f32 * 0.5 + 0.5).powf(1.5) * MEADOW_RELIEF;

        // |fbm| folds the field into sharp crests; squaring sharpens them further.
        let ridge = 1.0 - self.fbm(p, octave_count).abs() as f32;
        let mountain = ridge * ridge;

        let base = (continent * 0.5 + 0.5) * 0.35;
        let detail = (meadow + (mountain - meadow) * mountainness) * 0.65;

        base + detail // [0, 1]
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
