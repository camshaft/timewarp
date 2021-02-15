use crate::stack::Stack;
use core::ops;

pub trait Entry: Sized {
    type Queue: Queue<Self>;
    type Storage: Storage<Self>;

    fn delay(&self) -> <Self::Storage as Storage<Self>>::Tick;
    fn start_tick(&self) -> <Self::Storage as Storage<Self>>::Tick;
    fn set_start_tick(&mut self, tick: <Self::Storage as Storage<Self>>::Tick);
}

pub trait Queue<E: Entry<Queue = Self>> {
    fn new() -> Self;
    fn is_empty(&self) -> bool;
    fn push(&mut self, entry: E);
    fn pop(&mut self) -> Option<E>;
    fn take(&mut self) -> Self;
    fn count(&self) -> usize;
    fn next_expiring(&self) -> <E::Storage as Storage<E>>::Tick;
}

pub trait Storage<E: Entry>: Default + AsRef<[Stack<E>]> + AsMut<[Stack<E>]> {
    type Tick: Tick;

    fn ticks(&self) -> Self::Tick;

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.as_ref().iter().fold(true, |acc, s| acc & s.is_empty())
    }

    #[inline(always)]
    fn len(&self) -> usize {
        self.as_ref().len()
    }

    #[inline(always)]
    fn get(&self, index: usize) -> &Stack<E> {
        debug_assert!(index < self.len());
        unsafe { self.as_ref().get_unchecked(index) }
    }

    #[inline(always)]
    fn get_mut(&mut self, index: usize) -> &mut Stack<E> {
        debug_assert!(index < self.len());
        unsafe { self.as_mut().get_unchecked_mut(index) }
    }
}

impl<E: Entry> Storage<E> for [Stack<E>; 4] {
    type Tick = u32;

    #[inline(always)]
    fn ticks(&self) -> Self::Tick {
        u32::from_le_bytes([
            self[0].current(),
            self[1].current(),
            self[2].current(),
            self[3].current(),
        ])
    }

    #[inline(always)]
    fn len(&self) -> usize {
        4
    }
}

impl<E: Entry> Storage<E> for [Stack<E>; 8] {
    type Tick = u64;

    #[inline(always)]
    fn ticks(&self) -> Self::Tick {
        u64::from_le_bytes([
            self[0].current(),
            self[1].current(),
            self[2].current(),
            self[3].current(),
            self[4].current(),
            self[5].current(),
            self[6].current(),
            self[7].current(),
        ])
    }

    #[inline(always)]
    fn len(&self) -> usize {
        8
    }
}

pub trait Tick
where
    Self: Copy
        + Default
        + Sized
        + ops::BitXor<Output = Self>
        + ops::Add<Output = Self>
        + ops::Sub<Output = Self>,
{
    type Bytes: AsRef<[u8]> + AsMut<[u8]> + Default;

    fn wrapping_add(self, rhs: Self) -> Self;
    fn checked_sub(self, rhs: Self) -> Option<Self>;
    fn to_be(self) -> Self;
    fn to_le_bytes(self) -> Self::Bytes;
    fn from_le_bytes(bytes: Self::Bytes) -> Self;
    fn is_zero(self) -> bool;
    fn leading_zeros(self) -> u32;
    fn elapsed_since(self, rhs: Self) -> Self;
}

impl Tick for u32 {
    type Bytes = [u8; 4];

    fn checked_sub(self, rhs: Self) -> Option<Self> {
        u32::checked_sub(self, rhs)
    }

    fn wrapping_add(self, rhs: Self) -> Self {
        u32::wrapping_add(self, rhs)
    }

    fn to_be(self) -> Self {
        u32::to_be(self)
    }

    fn to_le_bytes(self) -> Self::Bytes {
        u32::to_le_bytes(self)
    }

    fn from_le_bytes(bytes: Self::Bytes) -> Self {
        u32::from_le_bytes(bytes)
    }

    fn is_zero(self) -> bool {
        self == 0
    }

    fn leading_zeros(self) -> u32 {
        u32::leading_zeros(self)
    }

    fn elapsed_since(self, rhs: Self) -> Self {
        if let Some(d) = self.checked_sub(rhs) {
            d
        } else {
            self + (rhs - Self::MAX)
        }
    }
}

impl Tick for u64 {
    type Bytes = [u8; 8];

    fn checked_sub(self, rhs: Self) -> Option<Self> {
        u64::checked_sub(self, rhs)
    }

    fn wrapping_add(self, rhs: Self) -> Self {
        u64::wrapping_add(self, rhs)
    }

    fn to_be(self) -> Self {
        u64::to_be(self)
    }

    fn to_le_bytes(self) -> Self::Bytes {
        u64::to_le_bytes(self)
    }

    fn from_le_bytes(bytes: Self::Bytes) -> Self {
        u64::from_le_bytes(bytes)
    }

    fn is_zero(self) -> bool {
        self == 0
    }

    fn leading_zeros(self) -> u32 {
        u64::leading_zeros(self)
    }

    fn elapsed_since(self, rhs: Self) -> Self {
        if let Some(d) = self.checked_sub(rhs) {
            d
        } else {
            self + (rhs - Self::MAX)
        }
    }
}

#[cfg(feature = "atomic-entry")]
pub mod atomic {
    use super::*;
    use alloc::sync::Arc;
    use core::{
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
        task::Waker,
    };
    use futures::task::AtomicWaker;
    use intrusive_collections::{intrusive_adapter, LinkedList, LinkedListLink};

    intrusive_adapter!(pub Adapter = ArcEntry: Entry { link: LinkedListLink });

    pub type ArcEntry = Arc<Entry>;

    #[derive(Debug)]
    pub struct Entry {
        waker: AtomicWaker,
        expired: AtomicBool,
        registered: AtomicBool,
        delay: u64,
        start_tick: AtomicU64,
        link: LinkedListLink,
    }

    unsafe impl Send for Entry {}
    unsafe impl Sync for Entry {}

    pub fn wake(entry: ArcEntry) {
        entry.wake();
    }

    impl Entry {
        pub fn new(delay: u64) -> Arc<Self> {
            Arc::new(Self {
                waker: AtomicWaker::new(),
                expired: AtomicBool::new(false),
                registered: AtomicBool::new(false),
                delay,
                start_tick: AtomicU64::new(0),
                link: LinkedListLink::new(),
            })
        }

        pub fn wake(&self) {
            self.expired.store(true, Ordering::SeqCst);
            self.registered.store(false, Ordering::SeqCst);

            if let Some(waker) = self.waker.take() {
                waker.wake();
            }
        }

        pub fn should_register(&self) -> bool {
            !self.registered.swap(true, Ordering::SeqCst)
        }

        pub fn cancel(&self) {
            self.waker.take();
        }

        pub fn take_expired(&self) -> bool {
            self.expired.swap(false, Ordering::SeqCst)
        }

        pub fn register(&self, waker: &Waker) {
            self.waker.register(waker)
        }

        fn start_tick(&self) -> u64 {
            self.start_tick.load(Ordering::SeqCst)
        }
    }

    impl super::Entry for Arc<Entry> {
        type Queue = LinkedList<Adapter>;
        type Storage = [Stack<Self>; 8];

        fn delay(&self) -> u64 {
            self.delay
        }

        fn start_tick(&self) -> u64 {
            Entry::start_tick(self)
        }

        fn set_start_tick(&mut self, tick: u64) {
            self.start_tick.store(tick, Ordering::SeqCst);
        }
    }

    impl Drop for Entry {
        fn drop(&mut self) {
            self.cancel();
        }
    }

    impl Queue<ArcEntry> for LinkedList<Adapter> {
        fn new() -> Self {
            LinkedList::new(Adapter::new())
        }

        fn is_empty(&self) -> bool {
            LinkedList::is_empty(self)
        }

        fn push(&mut self, entry: ArcEntry) {
            self.push_back(entry);
        }

        fn pop(&mut self) -> Option<ArcEntry> {
            self.pop_front()
        }

        fn take(&mut self) -> Self {
            LinkedList::take(self)
        }

        fn count(&self) -> usize {
            self.iter().count()
        }

        fn next_expiring(&self) -> u64 {
            self.iter()
                .map(|e| {
                    let start_tick = e.start_tick();
                    if let Some(end) = start_tick.checked_add(e.delay) {
                        end
                    } else {
                        e.delay - start_tick
                    }
                })
                .min()
                .unwrap_or(0)
        }
    }
}
