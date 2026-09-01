use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::sync::{Condvar, Mutex};

use super::patch::PatchKey;
use super::patch_generator::{PatchPriority, WantedPatch};

pub struct PatchQueue {
    state: Mutex<QueueState>,
    available: Condvar,
}

impl PatchQueue {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                pending: BinaryHeap::new(),
                in_flight: HashSet::new(),
                wanted: HashSet::new(),
                shutdown: false,
            }),
            available: Condvar::new(),
        }
    }

    pub fn update_wanted_patches<I>(&self, wanted_patches: I)
    where
        I: IntoIterator<Item = WantedPatch>,
    {
        let is_empty = {
            let mut state = self.state.lock().unwrap();

            if state.shutdown {
                return;
            }

            state.pending.clear();
            state.wanted.clear();

            for wanted in wanted_patches {
                state.wanted.insert(wanted.patch);

                if state.in_flight.contains(&wanted.patch) {
                    continue;
                }

                state.pending.push(QueueEntry {
                    patch: wanted.patch,
                    priority: wanted.priority,
                });
            }

            state.pending.is_empty()
        };

        if !is_empty {
            self.available.notify_all();
        }
    }

    pub fn claim_blocking(&self) -> Option<PatchKey> {
        let mut state = self.state.lock().unwrap();

        loop {
            if state.shutdown {
                return None;
            }

            while let Some(entry) = state.pending.pop() {
                if state.in_flight.insert(entry.patch) {
                    return Some(entry.patch);
                }
            }

            state = self.available.wait(state).unwrap();
        }
    }

    pub fn complete(&self, patch: PatchKey) -> bool {
        let mut state = self.state.lock().unwrap();
        let present = state.in_flight.remove(&patch);
        debug_assert!(present);

        state.wanted.contains(&patch)
    }

    pub fn shutdown(&self) {
        {
            let mut state = self.state.lock().unwrap();

            state.shutdown = true;
            state.pending.clear();
            state.wanted.clear();
        }

        self.available.notify_all();
    }
}

struct QueueState {
    pending: BinaryHeap<QueueEntry>,
    in_flight: HashSet<PatchKey>,
    wanted: HashSet<PatchKey>,
    shutdown: bool,
}

struct QueueEntry {
    patch: PatchKey,
    priority: PatchPriority,
}

impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
    }
}

impl Eq for QueueEntry {}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority.cmp(&other.priority)
    }
}
