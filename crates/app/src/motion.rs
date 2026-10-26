//! Spring-driven geometry for calendar blocks.
//!
//! Every block's target spot is computed from the data on each frame. Its on-screen spot
//! springs towards that target, so filling a day, moving a block or switching weeks
//! animates without each feature having to ask for it. Spots are in calendar terms, days
//! and minutes, not pixels, so resizing the window moves blocks with the grid at once
//! instead of sending them gliding after it.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use gpui::{Animated, Lerp, SpringConfig, spring};

/// Where a block is drawn, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub opacity: f32,
}

/// Where a block sits in the week.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    /// The day column, between two while moving from one to the other.
    pub day: f32,
    /// The left edge and width as parts of the column, for blocks sharing it.
    pub left: f32,
    pub width: f32,
    /// Minutes past midnight.
    pub start: f32,
    pub end: f32,
    /// How much of its height is drawn, for blocks pouring in.
    pub grown: f32,
    /// Pixels above its place, for blocks dropping into it.
    pub raised: f32,
    pub opacity: f32,
}

impl Spot {
    pub fn new(day: usize, left: f32, width: f32, start: f32, end: f32) -> Self {
        Spot { day: day as f32, left, width, start, end, grown: 1., raised: 0., opacity: 1. }
    }
}

impl Lerp for Spot {
    fn lerp(&self, to: &Self, delta: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * delta;
        Spot {
            day: mix(self.day, to.day),
            left: mix(self.left, to.left),
            width: mix(self.width, to.width),
            start: mix(self.start, to.start),
            end: mix(self.end, to.end),
            grown: mix(self.grown, to.grown).max(0.),
            raised: mix(self.raised, to.raised),
            opacity: mix(self.opacity, to.opacity).clamp(0., 1.),
        }
    }
}

/// How a block first appears.
#[derive(Clone, Copy)]
pub enum Entrance {
    /// Grows down from its top edge, like time being poured in.
    Pour,
    /// Fades in with a small drop.
    Fade,
    /// Already there, as for a block being drawn under the pointer.
    Instant,
}

impl Entrance {
    fn from(self, target: Spot) -> Spot {
        match self {
            Entrance::Pour => Spot { grown: 0., opacity: 0.4, ..target },
            Entrance::Fade => Spot { raised: 6., opacity: 0., ..target },
            Entrance::Instant => target,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BlockKey {
    Worklog(u64),
    /// A meeting or timer that isn't logged.
    Activity(u64),
    /// A worklog deleted here that Jira still has, keyed by a hash of its Jira id.
    Removed(u64),
    /// Time waiting for an issue, by its place among the drafts.
    Draft(usize),
}

struct Slot {
    value: Animated<Spot>,
    /// Staggered entrances hold the starting geometry until this moment.
    starts_at: Instant,
}

pub struct Blocks {
    slots: HashMap<BlockKey, Slot>,
    seen: HashSet<BlockKey>,
    active: bool,
    pub reduce_motion: bool,
}

const SPRING: SpringConfig = spring(420., 34., 1.);

impl Blocks {
    pub fn new() -> Self {
        Self {
            slots: HashMap::new(),
            seen: HashSet::new(),
            active: false,
            reduce_motion: false,
        }
    }

    pub fn begin_frame(&mut self) {
        self.seen.clear();
        self.active = false;
    }

    /// Where to draw `key` this frame. A key seen for the first time enters after `delay`.
    pub fn place(
        &mut self,
        key: BlockKey,
        target: Spot,
        entrance: Entrance,
        delay: Duration,
        now: Instant,
    ) -> Spot {
        self.seen.insert(key);
        let reduce = self.reduce_motion;
        let slot = self.slots.entry(key).or_insert_with(|| Slot {
            value: Animated::new(if reduce { target } else { entrance.from(target) }, SPRING),
            starts_at: now + delay,
        });
        if reduce {
            slot.value.jump_to(target);
            return target;
        }
        if now < slot.starts_at {
            self.active = true;
            return slot.value.sample(now).value;
        }
        slot.value.set(target, SPRING, now);
        let sample = slot.value.sample(now);
        self.active |= sample.is_active;
        sample.value
    }

    /// Forgets blocks that weren't drawn this frame and reports whether anything is still
    /// moving, in which case the caller requests another frame.
    pub fn end_frame(&mut self) -> bool {
        let seen = &self.seen;
        self.slots.retain(|key, _| seen.contains(key));
        self.active
    }
}
