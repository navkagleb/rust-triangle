use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::sync::{Condvar, Mutex};

use super::generator::{PatchPriority, WantedPatch};
use super::patch::PatchCoord;

pub struct Queue {
    state: Mutex<State>,
    available: Condvar,
}

impl Queue {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
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
                state.wanted.insert(wanted.coord);

                if state.in_flight.contains(&wanted.coord) {
                    continue;
                }

                state.pending.push(Entry {
                    coord: wanted.coord,
                    priority: wanted.priority,
                });
            }

            state.pending.is_empty()
        };

        if !is_empty {
            self.available.notify_all();
        }
    }

    pub fn claim_blocking(&self) -> Option<PatchCoord> {
        let mut state = self.state.lock().unwrap();

        loop {
            if state.shutdown {
                return None;
            }

            while let Some(entry) = state.pending.pop() {
                if state.in_flight.insert(entry.coord) {
                    return Some(entry.coord);
                }
            }

            state = self.available.wait(state).unwrap();
        }
    }

    pub fn complete(&self, coord: PatchCoord) -> bool {
        let mut state = self.state.lock().unwrap();
        let present = state.in_flight.remove(&coord);
        debug_assert!(present);

        state.wanted.contains(&coord)
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

struct State {
    pending: BinaryHeap<Entry>,
    in_flight: HashSet<PatchCoord>,
    wanted: HashSet<PatchCoord>,
    shutdown: bool,
}

struct Entry {
    coord: PatchCoord,
    priority: PatchPriority,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
    }
}

impl Eq for Entry {}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority.cmp(&other.priority)
    }
}
