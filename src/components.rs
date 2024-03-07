use crate::{
    prelude::{InsertResult, TimewarpError},
    FrameBuffer, FrameNumber, TimewarpComponent,
};
use bevy::prelude::*;

/// entities with NoRollback are ignored, even if they have components which
/// have been registered for rollback.
#[derive(Component)]
pub struct NoRollback;

/// Added to every entity for metrics
#[derive(Component, Debug)]
pub struct TimewarpStatus {
    /// Deduct `last_snapshot_frame` from the current frame to determine how many frames this
    /// entity is predicted ahead for.
    last_snapshot_frame: FrameNumber,
    /// Incremented when an update to this entity caused a rollback to be requested
    rollback_triggers: u32,
}

impl TimewarpStatus {
    pub fn new(last_snapshot_frame: FrameNumber) -> Self {
        Self {
            last_snapshot_frame,
            rollback_triggers: 0,
        }
    }
    /// returns the frame of the most recent snapshot,
    /// telling you when any component of this entity was most recently updated.
    pub fn last_snap_frame(&self) -> FrameNumber {
        self.last_snapshot_frame
    }
    /// how many times did this entity cause a rollback to be requested
    pub fn rollback_triggers(&self) -> u32 {
        self.rollback_triggers
    }

    pub fn set_snapped_at(&mut self, frame: FrameNumber) {
        self.last_snapshot_frame = self.last_snapshot_frame.max(frame);
    }
    pub fn increment_rollback_triggers(&mut self) {
        self.rollback_triggers += 1;
    }
}

/// Used when you want to insert a component T, but for an older frame.
/// insert this to an entity for an older frame will trigger a rollback.
///
/// Note: this is for timewarp-registered components.
///
/// eg:
/// ```rust,ignore
/// commands.entity(e).insert(InsertComponentAtFrame::<Shield>(shield_comp, past_frame))
/// ```
#[derive(Component, Debug)]
pub struct InsertComponentAtFrame<T: TimewarpComponent> {
    pub component: T,
    pub frame: FrameNumber,
}
impl<T: TimewarpComponent> InsertComponentAtFrame<T> {
    pub fn new(frame: FrameNumber, component: T) -> Self {
        Self { component, frame }
    }
}

/// When you want to rollback to insert a non-timewarp component at a specific frame, insert this.
///
/// Note: this is for non-timewarp registered components. (ones without ComponentHistory)
///
/// I use this for blueprints. The blueprint assembly function runs during rollback and
/// adds the various timewarp-registered (and other) components to the entity during rollback.
#[derive(Component, Debug)]
pub struct AssembleBlueprintAtFrame<T: Component + std::fmt::Debug + Clone> {
    pub component: T,
    pub frame: FrameNumber,
}
impl<T: Component + std::fmt::Debug + Clone> AssembleBlueprintAtFrame<T> {
    pub fn new(frame: FrameNumber, component: T) -> Self {
        Self { component, frame }
    }
    pub fn type_name(&self) -> &str {
        std::any::type_name::<T>()
    }
}

/// entities with components that were registered with error correction logging will receive
/// one of these components, updated with before/after values when a simulation correction
/// resulting from a rollback and resimulate causes a snap.
/// ie, the values before and after the rollback differ.
/// in your game, look for Changed<TimewarpCorrection<T>> and use for any visual smoothing/interp stuff.
#[derive(Component, Debug, Clone)]
pub struct TimewarpCorrection<T: TimewarpComponent> {
    pub before: T,
    pub after: T,
    pub frame: FrameNumber,
}

/// Buffers the last few authoritative component values received from the server
#[derive(Component)]
pub struct ServerSnapshot<T: TimewarpComponent> {
    pub values: FrameBuffer<T>,
}
impl<T: TimewarpComponent> ServerSnapshot<T> {
    pub fn with_capacity(len: usize) -> Self {
        Self {
            values: FrameBuffer::with_capacity(len, "SS"),
        }
    }
    pub fn at_frame(&self, frame: FrameNumber) -> Option<&T> {
        self.values.get(frame)
    }
    pub fn insert(&mut self, frame: FrameNumber, val: T) -> Result<InsertResult, TimewarpError> {
        self.values.insert(frame, val)
    }
    pub fn type_name(&self) -> &str {
        std::any::type_name::<T>()
    }
    pub fn newest_snap_frame(&self) -> Option<FrameNumber> {
        let nf = self.values.newest_frame();
        if nf == 0 {
            None
        } else {
            Some(nf)
        }
    }
}

/// used to record component birth/death ranges in ComponentHistory.
/// (start, end) – can be open-ended if end is None.
pub type FrameRange = (FrameNumber, Option<FrameNumber>);

/// Buffers component values for the last few frames.
#[derive(Component)]
pub struct ComponentHistory<T: TimewarpComponent> {
    pub values: FrameBuffer<T>,        // not pub!
    pub alive_ranges: Vec<FrameRange>, // inclusive! unlike std:range
    pub correction_logging_enabled: bool,
}

// lazy first version - don't need a clone each frame if value hasn't changed!
// just store once and reference from each unchanged frame number.
impl<T: TimewarpComponent> ComponentHistory<T> {
    /// The entity param is just for logging.
    pub fn with_capacity(
        len: usize,
        birth_frame: FrameNumber,
        component: T,
        entity: &Entity,
    ) -> Self {
        let mut this = Self {
            values: FrameBuffer::with_capacity(len, "CH"),
            alive_ranges: vec![(birth_frame, None)],
            correction_logging_enabled: false,
        };
        trace!("CH.new {entity:?} {birth_frame} = {component:?}");
        // can't error on a brand new buffer:
        _ = this.values.insert(birth_frame, component);
        this
    }
    pub fn type_name(&self) -> &str {
        std::any::type_name::<T>()
    }
    /// will compute and insert `TimewarpCorrection`s when snapping
    pub fn enable_correction_logging(&mut self) {
        self.correction_logging_enabled = true;
    }
    pub fn at_frame(&self, frame: FrameNumber) -> Option<&T> {
        self.values.get(frame)
    }
    // adding entity just for debugging print outs.
    pub fn insert(
        &mut self,
        frame: FrameNumber,
        val: T,
        entity: &Entity,
    ) -> Result<InsertResult, TimewarpError> {
        trace!("CH.Insert {entity:?} {frame} = {val:?}");
        // TODO this is inefficient atm
        let ret = self.values.insert(frame, val)?;
        if !self.alive_at_frame(frame) {
            self.report_birth_at_frame(frame);
        }
        Ok(ret)
    }

    /// removes values buffered for this frame, and greater frames.
    #[allow(dead_code)]
    pub fn remove_frame_and_beyond(&mut self, frame: FrameNumber) {
        self.values
            .remove_entries_newer_than(frame.saturating_sub(1));
    }
    pub fn alive_at_frame(&self, frame: FrameNumber) -> bool {
        // self.values.get(frame).is_some()
        for (start, maybe_end) in &self.alive_ranges {
            if *start <= frame && (maybe_end.is_none() || maybe_end.unwrap() > frame) {
                return true;
            }
        }
        false
    }
    pub fn report_birth_at_frame(&mut self, frame: FrameNumber) {
        trace!("component birth @ {frame} {:?}", std::any::type_name::<T>());
        if self.alive_at_frame(frame) {
            trace!("Can't report birth of component already alive");
            return;
        }
        assert!(
            self.values.get(frame).is_some(),
            "No stored component value when reporting birth @ {frame}"
        );
        if self.alive_ranges.last().unwrap().1 == Some(frame) {
            return;
        }
        self.alive_ranges.push((frame, None));
    }
    pub fn report_death_at_frame(&mut self, frame: FrameNumber) {
        // currently after rollback we get (harmless?) erroneous RemovedComponent<> reports
        // so we just supress here for now.
        //
        // need to consider whether it's worth wiping alive_ranges on rolling back,
        // and having them repopulate during fast-fwd.
        if !self.alive_at_frame(frame) {
            return;
        }

        assert!(
            self.alive_at_frame(frame),
            "Can't report death of component not alive"
        );
        if self.alive_ranges.last().unwrap().1 == Some(frame) {
            return;
        }
        self.alive_ranges.last_mut().unwrap().1 = Some(frame);
        trace!(
            "component death @ {frame} {:?} --> {:?}",
            std::any::type_name::<T>(),
            self.alive_ranges
        );
    }
}
