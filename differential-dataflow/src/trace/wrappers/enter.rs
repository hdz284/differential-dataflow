//! Wrappers to provide trace access to nested scopes.

// use timely::progress::nested::product::Product;
use timely::progress::timestamp::Refines;
use timely::progress::{Antichain, frontier::AntichainRef};

use crate::lattice::Lattice;
use crate::trace::{TraceReader, BatchReader, Description};
use crate::trace::cursor::Cursor;

/// Wrapper to provide trace to nested scope.
pub struct TraceEnter<Tr: TraceReader, TInner> {
    trace: Tr,
    stash1: Antichain<Tr::Time>,
    stash2: Antichain<TInner>,
}

impl<Tr: TraceReader + Clone, TInner> Clone for TraceEnter<Tr, TInner> {
    fn clone(&self) -> Self {
        TraceEnter {
            trace: self.trace.clone(),
            stash1: Antichain::new(),
            stash2: Antichain::new(),
        }
    }
}

impl<Tr, TInner> WithLayout for TraceEnter<Tr, TInner>
where
    Tr: TraceReader<Batch: Clone>,
    TInner: Refines<Tr::Time>+Lattice,
{
    type Layout = (
        <Tr::Layout as Layout>::KeyContainer,
        <Tr::Layout as Layout>::ValContainer,
        Vec<TInner>,
        <Tr::Layout as Layout>::DiffContainer,
        <Tr::Layout as Layout>::OffsetContainer,
    );
}

impl<Tr, TInner> TraceReader for TraceEnter<Tr, TInner>
where
    Tr: TraceReader<Batch: Clone>,
    TInner: Refines<Tr::Time>+Lattice,
{
    type Batch = BatchEnter<Tr::Batch, TInner>;
    type Storage = Tr::Storage;
    type Cursor = CursorEnter<Tr::Cursor, TInner>;

    fn map_batches<F: FnMut(&Self::Batch)>(&self, mut f: F) {
        self.trace.map_batches(|batch| {
            f(&Self::Batch::make_from(batch.clone()));
        })
    }

    fn set_logical_compaction(&mut self, frontier: AntichainRef<TInner>) {
        self.stash1.clear();
        for time in frontier.iter() {
            self.stash1.insert(time.clone().to_outer());
        }
        self.trace.set_logical_compaction(self.stash1.borrow());
    }
    fn get_logical_compaction(&mut self) -> AntichainRef<TInner> {
        self.stash2.clear();
        for time in self.trace.get_logical_compaction().iter() {
            self.stash2.insert(TInner::to_inner(time.clone()));
        }
        self.stash2.borrow()
    }

    fn set_physical_compaction(&mut self, frontier: AntichainRef<TInner>) {
        self.stash1.clear();
        for time in frontier.iter() {
            self.stash1.insert(time.clone().to_outer());
        }
        self.trace.set_physical_compaction(self.stash1.borrow());
    }
    fn get_physical_compaction(&mut self) -> AntichainRef<TInner> {
        self.stash2.clear();
        for time in self.trace.get_physical_compaction().iter() {
            self.stash2.insert(TInner::to_inner(time.clone()));
        }
        self.stash2.borrow()
    }

    fn cursor_through(&mut self, upper: AntichainRef<TInner>) -> Option<(Self::Cursor, Self::Storage)> {
        self.stash1.clear();
        for time in upper.iter() {
            self.stash1.insert(time.clone().to_outer());
        }
        self.trace.cursor_through(self.stash1.borrow()).map(|(x,y)| (CursorEnter::new(x), y))
    }
}

impl<Tr, TInner> TraceEnter<Tr, TInner>
where
    Tr: TraceReader,
    TInner: Refines<Tr::Time>+Lattice,
{
    /// Makes a new trace wrapper
    pub fn make_from(trace: Tr) -> Self {
        TraceEnter {
            trace,
            stash1: Antichain::new(),
            stash2: Antichain::new(),
        }
    }
}


/// Wrapper to provide batch to nested scope.
#[derive(Clone)]
pub struct BatchEnter<B, TInner> {
    batch: B,
    description: Description<TInner>,
}

impl<B, TInner> WithLayout for BatchEnter<B, TInner>
where
    B: BatchReader,
    TInner: Refines<B::Time>+Lattice,
{
    type Layout = (
        <B::Layout as Layout>::KeyContainer,
        <B::Layout as Layout>::ValContainer,
        Vec<TInner>,
        <B::Layout as Layout>::DiffContainer,
        <B::Layout as Layout>::OffsetContainer,
    );
}

impl<B, TInner> BatchReader for BatchEnter<B, TInner>
where
    B: BatchReader,
    TInner: Refines<B::Time>+Lattice,
{
    type Cursor = BatchCursorEnter<B::Cursor, TInner>;

    fn cursor(&self) -> Self::Cursor {
        BatchCursorEnter::new(self.batch.cursor())
    }
    fn len(&self) -> usize { self.batch.len() }
    fn description(&self) -> &Description<TInner> { &self.description }
}

impl<B, TInner> BatchEnter<B, TInner>
where
    B: BatchReader,
    TInner: Refines<B::Time>+Lattice,
{
    /// Makes a new batch wrapper
    pub fn make_from(batch: B) -> Self {
        let lower: Vec<_> = batch.description().lower().elements().iter().map(|x| TInner::to_inner(x.clone())).collect();
        let upper: Vec<_> = batch.description().upper().elements().iter().map(|x| TInner::to_inner(x.clone())).collect();
        let since: Vec<_> = batch.description().since().elements().iter().map(|x| TInner::to_inner(x.clone())).collect();

        BatchEnter {
            batch,
            description: Description::new(Antichain::from(lower), Antichain::from(upper), Antichain::from(since))
        }
    }
}

/// Wrapper to provide cursor to nested scope.
pub struct CursorEnter<C, TInner> {
    phantom: ::std::marker::PhantomData<TInner>,
    cursor: C,
}

use crate::trace::implementations::{Layout, WithLayout};
impl<C, TInner> WithLayout for CursorEnter<C, TInner>
where
    C: Cursor,
    TInner: Refines<C::Time>+Lattice,
{
    type Layout = (
        <C::Layout as Layout>::KeyContainer,
        <C::Layout as Layout>::ValContainer,
        Vec<TInner>,
        <C::Layout as Layout>::DiffContainer,
        <C::Layout as Layout>::OffsetContainer,
    );
}

impl<C, TInner> CursorEnter<C, TInner> {
    fn new(cursor: C) -> Self {
        CursorEnter {
            phantom: ::std::marker::PhantomData,
            cursor,
        }
    }
}

impl<C, TInner> Cursor for CursorEnter<C, TInner>
where
    C: Cursor,
    TInner: Refines<C::Time>+Lattice,
{
    type Storage = C::Storage;

    #[inline] fn key_valid(&self, storage: &Self::Storage) -> bool { self.cursor.key_valid(storage) }
    #[inline] fn val_valid(&self, storage: &Self::Storage) -> bool { self.cursor.val_valid(storage) }

    #[inline] fn key<'a>(&self, storage: &'a Self::Storage) -> Self::Key<'a> { self.cursor.key(storage) }
    #[inline] fn val<'a>(&self, storage: &'a Self::Storage) -> Self::Val<'a> { self.cursor.val(storage) }

    #[inline] fn get_key<'a>(&self, storage: &'a Self::Storage) -> Option<Self::Key<'a>> { self.cursor.get_key(storage) }
    #[inline] fn get_val<'a>(&self, storage: &'a Self::Storage) -> Option<Self::Val<'a>> { self.cursor.get_val(storage) }

    #[inline]
    fn map_times<L: FnMut(&TInner, Self::DiffGat<'_>)>(&mut self, storage: &Self::Storage, mut logic: L) {
        self.cursor.map_times(storage, |time, diff| {
            logic(&TInner::to_inner(C::owned_time(time)), diff)
        })
    }

    #[inline] fn step_key(&mut self, storage: &Self::Storage) { self.cursor.step_key(storage) }
    #[inline] fn seek_key(&mut self, storage: &Self::Storage, key: Self::Key<'_>) { self.cursor.seek_key(storage, key) }

    #[inline] fn step_val(&mut self, storage: &Self::Storage) { self.cursor.step_val(storage) }
    #[inline] fn seek_val(&mut self, storage: &Self::Storage, val: Self::Val<'_>) { self.cursor.seek_val(storage, val) }

    #[inline] fn rewind_keys(&mut self, storage: &Self::Storage) { self.cursor.rewind_keys(storage) }
    #[inline] fn rewind_vals(&mut self, storage: &Self::Storage) { self.cursor.rewind_vals(storage) }
}



/// Wrapper to provide cursor to nested scope.
pub struct BatchCursorEnter<C, TInner> {
    phantom: ::std::marker::PhantomData<TInner>,
    cursor: C,
}

impl<C, TInner> BatchCursorEnter<C, TInner> {
    fn new(cursor: C) -> Self {
        BatchCursorEnter {
            phantom: ::std::marker::PhantomData,
            cursor,
        }
    }
}

impl<C, TInner> WithLayout for BatchCursorEnter<C, TInner>
where
    C: Cursor,
    TInner: Refines<C::Time>+Lattice,
{
    type Layout = (
        <C::Layout as Layout>::KeyContainer,
        <C::Layout as Layout>::ValContainer,
        Vec<TInner>,
        <C::Layout as Layout>::DiffContainer,
        <C::Layout as Layout>::OffsetContainer,
    );
}

impl<TInner, C: Cursor> Cursor for BatchCursorEnter<C, TInner>
where
    TInner: Refines<C::Time>+Lattice,
{
    type Storage = BatchEnter<C::Storage, TInner>;

    #[inline] fn key_valid(&self, storage: &Self::Storage) -> bool { self.cursor.key_valid(&storage.batch) }
    #[inline] fn val_valid(&self, storage: &Self::Storage) -> bool { self.cursor.val_valid(&storage.batch) }

    #[inline] fn key<'a>(&self, storage: &'a Self::Storage) -> Self::Key<'a> { self.cursor.key(&storage.batch) }
    #[inline] fn val<'a>(&self, storage: &'a Self::Storage) -> Self::Val<'a> { self.cursor.val(&storage.batch) }

    #[inline] fn get_key<'a>(&self, storage: &'a Self::Storage) -> Option<Self::Key<'a>> { self.cursor.get_key(&storage.batch) }
    #[inline] fn get_val<'a>(&self, storage: &'a Self::Storage) -> Option<Self::Val<'a>> { self.cursor.get_val(&storage.batch) }

    #[inline]
    fn map_times<L: FnMut(&TInner, Self::DiffGat<'_>)>(&mut self, storage: &Self::Storage, mut logic: L) {
        self.cursor.map_times(&storage.batch, |time, diff| {
            logic(&TInner::to_inner(C::owned_time(time)), diff)
        })
    }

    #[inline] fn step_key(&mut self, storage: &Self::Storage) { self.cursor.step_key(&storage.batch) }
    #[inline] fn seek_key(&mut self, storage: &Self::Storage, key: Self::Key<'_>) { self.cursor.seek_key(&storage.batch, key) }

    #[inline] fn step_val(&mut self, storage: &Self::Storage) { self.cursor.step_val(&storage.batch) }
    #[inline] fn seek_val(&mut self, storage: &Self::Storage, val: Self::Val<'_>) { self.cursor.seek_val(&storage.batch, val) }

    #[inline] fn rewind_keys(&mut self, storage: &Self::Storage) { self.cursor.rewind_keys(&storage.batch) }
    #[inline] fn rewind_vals(&mut self, storage: &Self::Storage) { self.cursor.rewind_vals(&storage.batch) }
}
