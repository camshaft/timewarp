use super::{
    bitset::Bitset,
    entry::{Entry, Queue},
};
use arr_macro::arr;
use core::{fmt, marker::PhantomData};

pub struct Stack<E: Entry> {
    slots: [E::Queue; 256],
    pub(crate) occupied: Bitset,
    current: u8,
    entry: PhantomData<E>,
}

impl<E: Entry> Default for Stack<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Entry> fmt::Debug for Stack<E> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let alternate = f.alternate();

        let mut s = f.debug_struct("Stack");

        s.field("current", &self.current);

        if alternate {
            s.field("occupied", &DebugQueues(self));
        } else {
            s.field("occupied", &self.occupied.len());
        }

        s.finish()
    }
}

struct DebugQueues<'a, E: Entry>(&'a Stack<E>);

impl<'a, E: Entry> fmt::Debug for DebugQueues<'a, E> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut s = f.debug_map();
        for i in 0..=255 {
            if self.0.occupied.get(i) {
                s.entry(&i, &self.0.slots[i as usize].count());
            }
        }

        s.finish()
    }
}

impl<E: Entry> Stack<E> {
    pub fn new() -> Self {
        let slots = arr![E::Queue::new(); 256];
        Self {
            slots,
            occupied: Default::default(),
            current: 0,
            entry: PhantomData,
        }
    }

    pub fn current(&self) -> u8 {
        self.current
    }

    pub fn is_empty(&self) -> bool {
        self.occupied.is_empty()
    }

    pub fn insert(&mut self, index: u8, entry: E) {
        self.occupied.insert(index);
        let list = self.slot_mut(index);
        list.push(entry);
    }

    fn next_occupied(&self, current: u8) -> (u8, bool) {
        if let Some(next) = self.occupied.next_occupied(current) {
            (next, false)
        } else {
            (0, true)
        }
    }

    pub fn tick(&mut self, can_skip: bool) -> (E::Queue, bool) {
        let (current, wrapped) = self.next_tick(can_skip);
        self.current = current;
        let slot = self.take();
        (slot, wrapped)
    }

    pub fn next_tick(&self, can_skip: bool) -> (u8, bool) {
        let (mut current, mut wrapped) = self.current.overflowing_add(1);
        if can_skip {
            let (next, did_wrap) = self.next_occupied(current);
            current = next;
            wrapped = did_wrap;
        }
        (current, wrapped)
    }

    pub fn take(&mut self) -> E::Queue {
        let current = self.current;
        self.occupied.remove(current);
        self.slot_mut(current).take()
    }

    fn slot_mut(&mut self, index: u8) -> &mut E::Queue {
        if cfg!(test) {
            assert!(self.slots.len() > index as usize);
        }
        unsafe { self.slots.get_unchecked_mut(index as usize) }
    }
}
