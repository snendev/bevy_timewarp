/// FrameBuffer<T> is a shortish buffer of the last N values of T indexed by FrameNumber
/// for example, we would like to store a Position component for the last N frames.
/// inserting the past, ie before (newest_frame - capacity) is not allowed
/// inserting in the future, ie >> newest_frame, is permitted - and the resulting gap is filled
/// with Nones.
///
use crate::*;
use bevy::prelude::*;
use std::{collections::VecDeque, fmt, ops::Range};

pub enum InsertResult {
    Identical,
    Replaced,
    New,
}

/// values for new frames are push_front'ed onto the vecdeque
#[derive(Resource, Clone)]
pub struct FrameBuffer<T>
where
    T: Clone + Send + Sync + PartialEq + std::fmt::Debug,
{
    /// Contains Option<T> because there can be gaps
    /// and we want to be able to store 'None' as a normal value in here.
    entries: VecDeque<Option<T>>,
    /// frame number of the first elem of vecdeque ie newest value. 0 = empty.
    front_frame: FrameNumber,
    capacity: usize,
    pub name: String,
}

impl<T> fmt::Debug for FrameBuffer<T>
where
    T: Clone + Send + Sync + PartialEq + std::fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "FrameBuffer[{}]<{}>{{front_frame:{:?}, capacity:{:?} entries:[{:?},...]}}",
            self.name,
            std::any::type_name::<T>(),
            self.front_frame,
            self.capacity,
            self.get(self.newest_frame()),
        )
    }
}

impl<T> FrameBuffer<T>
where
    T: Clone + Send + Sync + PartialEq + std::fmt::Debug,
{
    pub fn with_capacity(len: usize, name: &str) -> Self {
        Self {
            entries: VecDeque::with_capacity(len),
            capacity: len,
            front_frame: 0,
            name: name.into(),
        }
    }

    pub fn current_range(&self) -> Range<FrameNumber> {
        Range {
            start: self.oldest_frame(),
            end: self.newest_frame() + 1, // end is exclusive for Ranges
        }
    }

    /// Greatest frame number with a buffered value.
    pub fn newest_frame(&self) -> FrameNumber {
        self.front_frame
    }

    /// Smallest frame number with a buffered value.
    /// Theoretically.. value could be None if not inserted yet.
    pub fn oldest_frame(&self) -> FrameNumber {
        self.front_frame
            .saturating_sub(self.entries.len().saturating_sub(1) as FrameNumber)
    }

    /// removes entries for frames larger than `frame`
    /// buffer could contain fewer than `capacity` values after this operation.
    pub fn remove_entries_newer_than(&mut self, frame: FrameNumber) {
        if frame >= self.front_frame {
            return;
        }
        if let Some(index) = self.index(frame) {
            self.entries.drain(0..index.min(self.entries.len()));
            self.front_frame = frame;
        } else {
            error!("remove_entries_newer_than {frame} failed, no index.");
        }
    }

    /// value at frame, or None if out of the range of values currently stored in the buffer
    /// by design, at the moment, we don't distinguish between returning a stored None value,
    /// and returning None because the requested frame is out of range. (because we don't care)
    pub fn get(&self, frame: FrameNumber) -> Option<&T> {
        if let Some(index) = self.index(frame) {
            // a value is stored for this frame
            if let Some(val) = self.entries.get(index) {
                // and the value is a Some(T)
                val.as_ref()
            } else {
                // the value is a None
                None
            }
        } else {
            // no value found because frame is out of range
            None
        }
    }

    /// like get, but mut
    pub fn get_mut(&mut self, frame: FrameNumber) -> Option<&mut T> {
        if let Some(index) = self.index(frame) {
            // a value is stored for this frame
            if let Some(val) = self.entries.get_mut(index) {
                // and the value is a Some(T)
                val.as_mut()
            } else {
                // the value is a None
                None
            }
        } else {
            // no value found because frame is out of range
            None
        }
    }

    pub fn insert_blanks(&mut self, num_blanks: usize) {
        for _ in 0..num_blanks {
            self.entries.push_front(None);
        }
    }

    // which frames have values?
    pub fn frame_occupancy(&self) -> Vec<bool> {
        self.entries.iter().map(|e| e.is_some()).collect::<Vec<_>>()
    }

    /// insert value at given frame.
    /// It is permitted to insert at old frames that are still in the range, but
    /// not allowed to insert at a frame older than the oldest existing frame.
    ///
    /// Is is permitted to insert at any future frame, any gaps will be made None.
    /// so if you insert at newest_frame() + a gazillion, you gets a buffer containing your
    /// one new value and a bunch of Nones after it.
    pub fn insert(&mut self, frame: FrameNumber, value: T) -> Result<InsertResult, TimewarpError> {
        // is this frame too old to be accepted?
        // consider that past-frame inserts happen in PreUpdate, after which the frame is incremented
        // so we use <= here to ensure we don't insert at the boundary, which is then immediately
        // outside the window after an increment.
        if frame <= self.oldest_frame() {
            return Err(TimewarpError::FrameTooOld);
        }
        // are we replacing a potential existing value, ie no change in buffer range
        if let Some(index) = self.index(frame) {
            if let Some(val) = self.entries.get_mut(index) {
                let v = val.as_ref();
                // TODO should we test if we are we replacing with same-val that already exists,
                // and bail out here? would still need to avoid mutably derefing the SS somehow.
                if val.is_none() {
                    *val = Some(value);
                    return Ok(InsertResult::New);
                }
                if v.is_some() && *v.unwrap() == value {
                    return Ok(InsertResult::Identical);
                }

                *val = Some(value);
                return Ok(InsertResult::Replaced);
            }
            panic!("Shouldn't get here");
        }

        if self.front_frame == 0 || frame == self.front_frame + 1 {
            // no gaps.
        } else {
            // so we are inserting a frame greater than front_frame.
            // any gaps between current `front_frame` and `frame` need to be created as None
            // warn!(
            //     "inserting f {frame}, front_frame currently: {} {self:?}",
            //     self.front_frame
            // );
            for _f in (self.front_frame + 1)..frame {
                self.entries.push_front(None);
            }
        }

        self.entries.push_front(Some(value));
        self.front_frame = frame;
        self.entries.truncate(self.capacity);
        Ok(InsertResult::New)
    }

    /// gets index into vecdeq for frame number, or None if out of range.
    fn index(&self, frame: FrameNumber) -> Option<usize> {
        /*
           Eg, capacity = 5
               front_frame = 10
               entries: [a,b,c,d,e]
               equates to frame values being
               [10=a, 9=b, 8=c, 7=d, 6=e]
        */
        if frame > self.front_frame {
            return None;
        }
        if frame
            <= self
                .front_frame
                .saturating_sub(self.capacity as FrameNumber)
        {
            return None;
        }
        Some(self.front_frame as usize - frame as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oldest_frame() {
        let mut fb = FrameBuffer::<u32>::with_capacity(5, "");
        assert_eq!(fb.oldest_frame(), 0);
        fb.insert(1, 1).unwrap();
        assert_eq!(fb.get(1), Some(&1));
        assert_eq!(fb.oldest_frame(), 1);

        fb.insert(2, 2).unwrap();
        assert_eq!(fb.get(2), Some(&2));
        assert_eq!(fb.oldest_frame(), 1);

        fb.insert(3, 3).unwrap();
        assert_eq!(fb.get(3), Some(&3));
        assert_eq!(fb.oldest_frame(), 1);

        fb.insert(4, 4).unwrap();
        assert_eq!(fb.get(4), Some(&4));
        assert_eq!(fb.oldest_frame(), 1);

        fb.insert(5, 5).unwrap();
        assert_eq!(fb.get(5), Some(&5));
        assert_eq!(fb.oldest_frame(), 1);

        fb.insert(6, 6).unwrap();
        assert_eq!(fb.get(6), Some(&6));
        assert_eq!(fb.oldest_frame(), 2);
    }

    #[test]
    fn test_frame_buffer() {
        let mut fb = FrameBuffer::<u32>::with_capacity(5, "");
        fb.insert(1, 1).unwrap();
        assert_eq!(fb.get(1), Some(&1));

        fb.insert(2, 2).unwrap();
        // print!("{fb:?}");
        fb.insert(3, 3).unwrap();
        fb.insert(4, 4).unwrap();
        fb.insert(5, 5).unwrap();
        assert_eq!(fb.get(1), Some(&1));
        assert_eq!(fb.get(3), Some(&3));
        assert_eq!(fb.get(5), Some(&5));
        assert_eq!(fb.get(6), None);
        fb.insert(6, 6).unwrap();
        assert_eq!(fb.get(6), Some(&6));
        // 1 should be dropped now
        assert_eq!(fb.get(1), None);
        // now test modifying a val by inserting over
        assert_eq!(fb.get(3), Some(&3));
        fb.insert(3, 33).unwrap();
        assert_eq!(fb.get(3), Some(&33));
        // test modifying by get_mut
        let v2 = fb.get_mut(2).unwrap();
        *v2 = 22;

        assert!(fb
            .insert(2, 22)
            .is_err_and(|e| e == TimewarpError::FrameTooOld));

        assert_eq!(fb.newest_frame(), 6);
        // inserting with a gap should fill with nones
        fb.insert(8, 8).unwrap();
        assert_eq!(fb.get(7), None);
        assert_eq!(fb.get(8), Some(&8));
        assert_eq!(fb.newest_frame(), 8);
        fb.remove_entries_newer_than(5);
        assert_eq!(fb.newest_frame(), 5);
        assert_eq!(fb.get(6), None);
        assert_eq!(fb.get(4), Some(&4));
        assert_eq!(fb.get(3), None);
    }
}
