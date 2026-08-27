use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::{Condvar, Mutex};

use glam::Vec2;

use super::patch::PatchKey;

#[derive(Clone, Copy, PartialEq, Eq)]
struct RequestId(pub u64);

struct PatchPriority {
    coverage_required: bool,
    ready_sibling_count: u8,
    visual_importance: f32,
    view_alignment: f32,
    age_frames: u32,
}

impl PartialEq for PatchPriority {
    fn eq(&self, other: &Self) -> bool {
        self.coverage_required == other.coverage_required
            && self.ready_sibling_count == other.ready_sibling_count
            && self.visual_importance.total_cmp(&other.visual_importance) == Ordering::Equal
            && self.view_alignment.total_cmp(&other.visual_importance) == Ordering::Equal
            && self.age_frames == other.age_frames
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
            .then_with(|| self.ready_sibling_count.cmp(&other.ready_sibling_count))
            .then_with(|| self.visual_importance.total_cmp(&other.visual_importance))
            .then_with(|| self.view_alignment.total_cmp(&other.view_alignment))
            .then_with(|| self.age_frames.cmp(&other.age_frames))
    }
}

pub struct DesiredPatch {
    patch: PatchKey,
    coverage_required: bool,
    ready_sibling_count: u8,
    visual_importance: f32,
    view_alignment: f32,
}

impl DesiredPatch {
    pub fn new(patch: PatchKey, ready_sibling_count: u8, camera_pos: Vec2, camera_forward: Vec2) -> Self {
        let patch_center = patch.terrain_center().as_vec2();

        Self {
            patch,
            coverage_required: ready_sibling_count == 0,
            ready_sibling_count,
            visual_importance: Self::calc_visual_importance(patch_center, patch.terrain_size(), camera_pos),
            view_alignment: Self::calc_view_alignment(patch_center, camera_pos, camera_forward),
        }
    }

    fn calc_visual_importance(patch_center: Vec2, patch_size: u32, camera_pos: Vec2) -> f32 {
        let distance = camera_pos.distance(patch_center).max(1.0);
        patch_size as f32 / distance
    }

    fn calc_view_alignment(patch_center: Vec2, camera_pos: Vec2, camera_forward: Vec2) -> f32 {
        let offset = patch_center - camera_pos;

        if offset.length_squared() >= 0.0001 {
            camera_forward.dot(offset.normalize()).clamp(-1.0, 1.0)
        } else {
            1.0
        }
    }

    fn priority(&self, age_frames: u32) -> PatchPriority {
        PatchPriority {
            coverage_required: self.coverage_required,
            ready_sibling_count: self.ready_sibling_count,
            visual_importance: self.visual_importance,
            view_alignment: self.view_alignment,
            age_frames,
        }
    }

    fn merge(&mut self, other: &Self) {
        self.coverage_required |= other.coverage_required;
        self.ready_sibling_count = self.ready_sibling_count.max(other.ready_sibling_count);
        self.visual_importance = self.visual_importance.max(other.visual_importance);
    }
}

pub struct PatchQueueEntry {
    patch: PatchKey,
    id: RequestId,
    priority: PatchPriority,
    sequence: u64,
}

impl PatchQueueEntry {
    pub fn patch(&self) -> PatchKey {
        self.patch
    }
}

impl PartialEq for PatchQueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.sequence == other.sequence
    }
}

impl Eq for PatchQueueEntry {}

impl PartialOrd for PatchQueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PatchQueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| self.sequence.cmp(&other.sequence))
    }
}

#[derive(Clone, Copy)]
struct PendingRequest {
    first_requested_frame: u64,
    id: RequestId,
    sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultDisposition {
    /// The patch is still wanted, so its result can be accepted
    Accept,

    /// The request was valid, but the patch is no longer wanted.
    Discard,

    /// The result does not correspond to the current in-flight request.
    Stale,
}

struct PriorityQueue {
    pending_queue: BinaryHeap<PatchQueueEntry>,
    pending_requests: HashMap<PatchKey, PendingRequest>,
    in_flight: HashMap<PatchKey, RequestId>,
    desired: HashSet<PatchKey>,

    next_request_id: RequestId,
    next_sequence: u64,

    shutdown: bool,
}

impl PriorityQueue {
    fn new() -> Self {
        Self {
            pending_queue: BinaryHeap::new(),
            pending_requests: HashMap::new(),
            in_flight: HashMap::new(),
            desired: HashSet::new(),
            next_request_id: RequestId(1),
            next_sequence: 1,
            shutdown: false,
        }
    }

    fn allocate_request_id(&mut self) -> RequestId {
        let id = self.next_request_id;

        self.next_request_id
            .0
            .checked_add(1)
            .expect("Patch request ID overflow");

        id
    }

    fn allocate_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence;

        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("Patch request sequence overflow");

        sequence
    }

    fn rebuild<I>(&mut self, frame_index: u64, desired_patches: I)
    where
        I: IntoIterator<Item = DesiredPatch>,
    {
        if self.shutdown {
            return;
        }

        let mut desired_by_key = HashMap::<PatchKey, DesiredPatch>::new();

        for desired in desired_patches {
            desired_by_key
                .entry(desired.patch)
                .and_modify(|existing| existing.merge(&desired))
                .or_insert(desired);
        }

        self.desired = desired_by_key.keys().copied().collect();
        self.pending_requests.retain(|patch, _| self.desired.contains(patch));

        self.pending_queue.clear();
        for desired in desired_by_key.into_values() {
            if self.in_flight.contains_key(&desired.patch) {
                continue;
            }

            if !self.pending_requests.contains_key(&desired.patch) {
                let pending = PendingRequest {
                    id: self.allocate_request_id(),
                    sequence: self.allocate_sequence(),
                    first_requested_frame: frame_index,
                };

                self.pending_requests.insert(desired.patch, pending);
            }

            let pending = self.pending_requests[&desired.patch];
            let age_frames = frame_index.saturating_sub(pending.first_requested_frame) as u32;

            self.pending_queue.push(PatchQueueEntry {
                id: pending.id,
                patch: desired.patch,
                priority: desired.priority(age_frames),
                sequence: pending.sequence,
            })
        }
    }

    fn pop(&mut self) -> Option<PatchQueueEntry> {
        while let Some(entry) = self.pending_queue.pop() {
            let Some(pending) = self.pending_requests.get(&entry.patch) else {
                continue;
            };

            if pending.id != entry.id {
                continue;
            }

            self.pending_requests.remove(&entry.patch);
            self.in_flight.insert(entry.patch, entry.id);

            return Some(entry);
        }

        None
    }

    fn finish_result(&mut self, entry: &PatchQueueEntry) -> ResultDisposition {
        let Some(current_id) = self.in_flight.get(&entry.patch) else {
            return ResultDisposition::Stale;
        };

        if *current_id != entry.id {
            return ResultDisposition::Stale;
        }

        self.in_flight.remove(&entry.patch);

        if self.desired.contains(&entry.patch) {
            ResultDisposition::Accept
        } else {
            ResultDisposition::Discard
        }
    }
}

pub struct PatchPriorityQueue {
    state: Mutex<PriorityQueue>,
    available: Condvar,
}

impl PatchPriorityQueue {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(PriorityQueue::new()),
            available: Condvar::new(),
        }
    }

    pub fn rebuild<I>(&self, frame_index: u64, desired_patches: I)
    where
        I: IntoIterator<Item = DesiredPatch>,
    {
        let has_work = {
            let mut queue = self.state.lock().unwrap();
            queue.rebuild(frame_index, desired_patches);
            !queue.pending_queue.is_empty()
        };

        if has_work {
            self.available.notify_all();
        }
    }

    pub fn pop_blocking(&self) -> Option<PatchQueueEntry> {
        let mut queue = self.state.lock().unwrap();

        loop {
            if queue.shutdown {
                return None;
            }

            if let Some(entry) = queue.pop() {
                return Some(entry);
            }

            queue = self.available.wait(queue).unwrap();
        }
    }

    pub fn finish_result(&self, entry: &PatchQueueEntry) -> ResultDisposition {
        let mut queue = self.state.lock().unwrap();
        queue.finish_result(entry)
    }

    pub fn shutdown(&self) {
        {
            let mut queue = self.state.lock().unwrap();

            queue.shutdown = true;
            queue.pending_queue.clear();
            queue.pending_requests.clear();
            queue.desired.clear();
        }

        self.available.notify_all();
    }
}
