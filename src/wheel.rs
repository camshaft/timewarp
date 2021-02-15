use crate::entry::{Entry, Queue, Storage, Tick};
use core::fmt;

pub struct Wheel<E: Entry> {
    stacks: E::Storage,
    pending_wake: E::Queue,
}

impl<E: Entry> Default for Wheel<E> {
    fn default() -> Self {
        Self {
            stacks: Default::default(),
            pending_wake: E::Queue::new(),
        }
    }
}

impl<E: Entry> fmt::Debug for Wheel<E>
where
    <E::Storage as Storage<E>>::Tick: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Wheel")
            .field("ticks", &self.ticks())
            .field("pending_wake", &self.pending_wake.count())
            .field("stacks", &<StacksDebug<E>>::new(&self.stacks))
            .finish()
    }
}

struct StacksDebug<'a, E: Entry>(&'a E::Storage);

impl<'a, E: Entry> StacksDebug<'a, E> {
    fn new(s: &'a E::Storage) -> Self {
        Self(s)
    }
}

impl<'a, E: Entry> fmt::Debug for StacksDebug<'a, E> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut l = f.debug_list();
        for i in 0..self.0.len() {
            l.entry(self.0.get(i));
        }
        l.finish()
    }
}

impl<E: Entry> Wheel<E> {
    pub fn ticks(&self) -> <E::Storage as Storage<E>>::Tick {
        self.stacks.ticks()
    }

    pub fn is_empty(&self) -> bool {
        self.stacks.is_empty()
    }

    pub fn insert(&mut self, mut entry: E) {
        let ticks = self.ticks();
        entry.set_start_tick(ticks);
        self.insert_at(entry, ticks, ticks);
    }

    fn insert_at(
        &mut self,
        entry: E,
        now: <E::Storage as Storage<E>>::Tick,
        start_tick: <E::Storage as Storage<E>>::Tick,
    ) -> bool {
        let delay = entry.delay();
        let absolute_time = delay.wrapping_add(start_tick);
        let zero_time = (absolute_time ^ now).to_be();

        // The entry should be woken up
        if zero_time.is_zero() {
            self.pending_wake.push(entry);
            return true;
        }

        // find the stack in which the entry belongs
        let absolute_bytes = absolute_time.to_le_bytes();
        let leading = zero_time.leading_zeros();

        let index = (leading / 8) as usize;
        let position = absolute_bytes.as_ref()[index];

        self.stacks.get_mut(index).insert(position, entry);

        false
    }

    pub fn next_expiration(&self) -> Option<<E::Storage as Storage<E>>::Tick> {
        if self.is_empty() {
            return None;
        }

        let mut next_time = self.ticks().to_le_bytes();
        let next_time_ref = next_time.as_mut();
        let mut can_skip = true;

        for (index, next_byte) in next_time_ref.iter_mut().enumerate() {
            let stack = self.stacks.get(index);

            let (current, did_wrap) = stack.next_tick(can_skip);

            *next_byte = current;

            // we can only proceed to the next stack if the current wrapped
            if !did_wrap {
                break;
            }

            // children can only skip if this is also empty
            can_skip &= stack.is_empty();
        }

        let ticks = <<E::Storage as Storage<E>>::Tick>::from_le_bytes(next_time);
        Some(ticks)
    }

    pub fn next_delta(&self) -> Option<<E::Storage as Storage<E>>::Tick> {
        let next = self.next_expiration()?;
        let now = self.ticks();

        Some(next.elapsed_since(now))
    }

    pub fn set_current_tick(&mut self, _ticks: <E::Storage as Storage<E>>::Tick) -> Option<bool> {
        if self.is_empty() {
            return None;
        }

        todo!()
    }

    /// Skips the timer to the next populated slot
    ///
    /// Returns
    /// * `Some(ticks)` where ticks is the number of ticks that
    ///   the wheel advanced
    /// * `None` when the wheel is empty
    pub fn skip(&mut self) -> Option<<E::Storage as Storage<E>>::Tick> {
        let start = self.ticks();
        let has_pending = !self.pending_wake.is_empty();

        if has_pending {
            return Some(Default::default());
        }

        if self.is_empty() {
            return None;
        }

        let mut iterations = 0;

        while !self.skip_once()? {
            if cfg!(test) {
                assert!(iterations < u16::MAX, "advance iterated too many times");
            }
            iterations += 1;
        }

        Some(self.ticks().elapsed_since(start))
    }

    fn skip_once(&mut self) -> Option<bool> {
        let mut can_skip = true;
        let mut is_empty = true;
        let mut has_pending = false;

        for index in 0..self.stacks.len() {
            let (mut list, did_wrap) = self.stacks.get_mut(index).tick(can_skip);

            let now = self.ticks();

            while let Some(entry) = list.pop() {
                let start_tick = entry.start_tick();
                if self.insert_at(entry, now, start_tick) {
                    // A pending item is ready
                    has_pending = true;
                } else {
                    // the item was pushed above the current stack so
                    // we can't skip anymore
                    can_skip = false;
                }

                // in either case we know there's some available entry
                is_empty = false;
            }

            // we can only proceed to the next stack if the current wrapped
            if !did_wrap {
                return Some(has_pending);
            }

            // children can only skip if this is also empty
            can_skip &= self.stacks.get(index).is_empty();
            is_empty &= can_skip;
        }

        if is_empty {
            return None;
        }

        Some(has_pending)
    }

    /// Wakes all of the entries that have expires
    pub fn wake<F: FnMut(E)>(&mut self, mut wake: F) -> usize {
        let mut count = 0;

        let mut pending = self.pending_wake.take();

        while let Some(entry) = pending.pop() {
            count += 1;
            wake(entry);
        }

        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::atomic;
    use alloc::{vec, vec::Vec};
    use bolero::{check, generator::*};
    use core::time::Duration;
    use std::prelude::v1::*;

    #[test]
    fn size_snapshot() {
        assert_eq!(core::mem::size_of::<Wheel<atomic::ArcEntry>>(), 33104);
    }

    #[test]
    fn insert_advance_wake_check() {
        let max_ticks = Duration::from_secs(1_000_000_000).as_nanos() as u64;

        let entry = gen::<Vec<u64>>().with().values(0..max_ticks);
        let entries = gen::<Vec<_>>().with().values(entry);

        check!().with_generator(entries).for_each(|entries| {
            test_helper(&entries[..]);
        });
    }

    fn test_helper<T: AsRef<[u64]>>(entries: &[T]) {
        let mut wheel = Wheel::default();
        let mut sorted = vec![];

        let mut total_ticks = 0;

        for entries in entries.iter().map(AsRef::as_ref) {
            sorted.extend_from_slice(entries);
            sorted.sort_unstable();

            let mut should_wake = false;
            for entry in entries.iter().copied() {
                // adding a 0-tick will immediately wake the entry
                should_wake |= entry == 0;
                wheel.insert(atomic::Entry::new(entry));
            }

            let mut sorted = sorted.drain(..);

            let woken = wheel.wake(atomic::wake);

            assert_eq!(woken > 0, should_wake);

            for _ in 0..woken {
                sorted.next();
            }

            let mut elapsed = 0;

            while let Some(expected) = sorted.next() {
                let delta = expected - elapsed;
                // TODO
                //let next_delta = wheel.next_delta().unwrap();
                //assert!(
                //    1 <= next_delta && next_delta <= delta,
                //    "delta: {}, next_delta(): {}",
                //    delta,
                //    next_delta
                //);
                assert_eq!(wheel.skip(), Some(delta));
                elapsed += delta;

                assert_eq!(
                    wheel.skip(),
                    Some(0),
                    "the wheel should not advance while there are pending items"
                );

                for _ in (0..wheel.wake(atomic::wake)).skip(1) {
                    assert_eq!(
                        sorted.next(),
                        Some(expected),
                        "any additional items should be equal"
                    );
                }
            }

            assert!(wheel.is_empty());
            assert_eq!(wheel.skip(), None);
            assert_eq!(wheel.wake(atomic::wake), 0);
            assert!(wheel.is_empty());

            total_ticks += elapsed;

            assert_eq!(wheel.ticks(), total_ticks);
        }
    }

    #[test]
    fn empty_test() {
        let mut wheel = Wheel::default();
        assert_eq!(wheel.ticks(), 0);
        assert!(wheel.is_empty());
        assert_eq!(wheel.skip(), None);
        assert_eq!(wheel.wake(atomic::wake), 0);
    }

    #[test]
    fn crossing_test() {
        for t in [250..260, 510..520, 65790..65800].iter().cloned().flatten() {
            test_helper(&[[t, t + 1]]);
        }
    }

    #[test]
    fn duplicate_test() {
        test_helper(&[&[1, 489][..], &[24, 279][..]]);
    }

    #[test]
    fn overflow_test() {
        test_helper(&[
            &[3588254211306][..],
            &[799215800378, 10940666347][..],
            &[][..],
        ]);
    }
}
