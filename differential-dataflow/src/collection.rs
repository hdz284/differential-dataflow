//! Types and traits associated with collections of data.
//!
//! The `Collection` type is differential dataflow's core abstraction for an updatable pile of data.
//!
//! Most differential dataflow programs are "collection-oriented", in the sense that they transform
//! one collection into another, using operators defined on collections. This contrasts with a more
//! imperative programming style, in which one might iterate through the contents of a collection
//! manually. The higher-level of programming allows differential dataflow to provide efficient
//! implementations, and to support efficient incremental updates to the collections.

use std::hash::Hash;

use timely::Container;
use timely::Data;
use timely::progress::Timestamp;
use timely::order::Product;
use timely::dataflow::scopes::{Child, child::Iterative};
use timely::dataflow::Scope;
use timely::dataflow::operators::*;
use timely::dataflow::StreamCore;

use crate::difference::{Semigroup, Abelian, Multiply};
use crate::lattice::Lattice;
use crate::hashable::Hashable;

/// A mutable collection of values of type `D`
///
/// The `Collection` type is the core abstraction in differential dataflow programs. As you write your
/// differential dataflow computation, you write as if the collection is a static dataset to which you
/// apply functional transformations, creating new collections. Once your computation is written, you
/// are able to mutate the collection (by inserting and removing elements); differential dataflow will
/// propagate changes through your functional computation and report the corresponding changes to the
/// output collections.
///
/// Each collection has three generic parameters. The parameter `G` is for the scope in which the
/// collection exists; as you write more complicated programs you may wish to introduce nested scopes
/// (e.g. for iteration) and this parameter tracks the scope (for timely dataflow's benefit). The `D`
/// parameter is the type of data in your collection, for example `String`, or `(u32, Vec<Option<()>>)`.
/// The `R` parameter represents the types of changes that the data undergo, and is most commonly (and
/// defaults to) `isize`, representing changes to the occurrence count of each record.
#[derive(Clone)]
pub struct Collection<G: Scope, D, R = isize, C = Vec<(D, <G as ScopeParent>::Timestamp, R)>> {
    /// The underlying timely dataflow stream.
    ///
    /// This field is exposed to support direct timely dataflow manipulation when required, but it is
    /// not intended to be the idiomatic way to work with the collection.
    ///
    /// The timestamp in the data is required to always be at least the timestamp _of_ the data, in
    /// the timely-dataflow sense. If this invariant is not upheld, differential operators may behave
    /// unexpectedly.
    pub inner: timely::dataflow::StreamCore<G, C>,
    /// Phantom data for unreferenced `D` and `R` types.
    phantom: std::marker::PhantomData<(D, R)>,
}

impl<G: Scope, D, R, C> Collection<G, D, R, C> {
    /// Creates a new Collection from a timely dataflow stream.
    ///
    /// This method seems to be rarely used, with the `as_collection` method on streams being a more
    /// idiomatic approach to convert timely streams to collections. Also, the `input::Input` trait
    /// provides a `new_collection` method which will create a new collection for you without exposing
    /// the underlying timely stream at all.
    ///
    /// This stream should satisfy the timestamp invariant as documented on [Collection]; this
    /// method does not check it.
    pub fn new(stream: StreamCore<G, C>) -> Collection<G, D, R, C> {
        Collection { inner: stream, phantom: std::marker::PhantomData }
    }
}
impl<G: Scope, D, R, C: Container + Clone + 'static> Collection<G, D, R, C> {
    /// Creates a new collection accumulating the contents of the two collections.
    ///
    /// Despite the name, differential dataflow collections are unordered. This method is so named because the
    /// implementation is the concatenation of the stream of updates, but it corresponds to the addition of the
    /// two collections.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///
    ///     let data = scope.new_collection_from(1 .. 10).1;
    ///
    ///     let odds = data.filter(|x| x % 2 == 1);
    ///     let evens = data.filter(|x| x % 2 == 0);
    ///
    ///     odds.concat(&evens)
    ///         .assert_eq(&data);
    /// });
    /// ```
    pub fn concat(&self, other: &Self) -> Self {
        self.inner
            .concat(&other.inner)
            .as_collection()
    }
    /// Creates a new collection accumulating the contents of the two collections.
    ///
    /// Despite the name, differential dataflow collections are unordered. This method is so named because the
    /// implementation is the concatenation of the stream of updates, but it corresponds to the addition of the
    /// two collections.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///
    ///     let data = scope.new_collection_from(1 .. 10).1;
    ///
    ///     let odds = data.filter(|x| x % 2 == 1);
    ///     let evens = data.filter(|x| x % 2 == 0);
    ///
    ///     odds.concatenate(Some(evens))
    ///         .assert_eq(&data);
    /// });
    /// ```
    pub fn concatenate<I>(&self, sources: I) -> Self
    where
        I: IntoIterator<Item=Self>
    {
        self.inner
            .concatenate(sources.into_iter().map(|x| x.inner))
            .as_collection()
    }
    // Brings a Collection into a nested region.
    ///
    /// This method is a specialization of `enter` to the case where the nested scope is a region.
    /// It removes the need for an operator that adjusts the timestamp.
    pub fn enter_region<'a>(&self, child: &Child<'a, G, <G as ScopeParent>::Timestamp>) -> Collection<Child<'a, G, <G as ScopeParent>::Timestamp>, D, R, C> {
        self.inner
            .enter(child)
            .as_collection()
    }
    /// Applies a supplied function to each batch of updates.
    ///
    /// This method is analogous to `inspect`, but operates on batches and reveals the timestamp of the
    /// timely dataflow capability associated with the batch of updates. The observed batching depends
    /// on how the system executes, and may vary run to run.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///     scope.new_collection_from(1 .. 10).1
    ///          .map_in_place(|x| *x *= 2)
    ///          .filter(|x| x % 2 == 1)
    ///          .inspect_container(|event| println!("event: {:?}", event));
    /// });
    /// ```
    pub fn inspect_container<F>(&self, func: F) -> Self
    where
        F: FnMut(Result<(&G::Timestamp, &C), &[G::Timestamp]>)+'static,
    {
        self.inner
            .inspect_container(func)
            .as_collection()
    }
    /// Attaches a timely dataflow probe to the output of a Collection.
    ///
    /// This probe is used to determine when the state of the Collection has stabilized and can
    /// be read out.
    pub fn probe(&self) -> probe::Handle<G::Timestamp> {
        self.inner
            .probe()
    }
    /// Attaches a timely dataflow probe to the output of a Collection.
    ///
    /// This probe is used to determine when the state of the Collection has stabilized and all updates observed.
    /// In addition, a probe is also often use to limit the number of rounds of input in flight at any moment; a
    /// computation can wait until the probe has caught up to the input before introducing more rounds of data, to
    /// avoid swamping the system.
    pub fn probe_with(&self, handle: &probe::Handle<G::Timestamp>) -> Self {
        Self::new(self.inner.probe_with(handle))
    }
    /// The scope containing the underlying timely dataflow stream.
    pub fn scope(&self) -> G {
        self.inner.scope()
    }

    /// Creates a new collection whose counts are the negation of those in the input.
    ///
    /// This method is most commonly used with `concat` to get those element in one collection but not another.
    /// However, differential dataflow computations are still defined for all values of the difference type `R`,
    /// including negative counts.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///
    ///     let data = scope.new_collection_from(1 .. 10).1;
    ///
    ///     let odds = data.filter(|x| x % 2 == 1);
    ///     let evens = data.filter(|x| x % 2 == 0);
    ///
    ///     odds.negate()
    ///         .concat(&data)
    ///         .assert_eq(&evens);
    /// });
    /// ```
    // TODO: Removing this function is possible, but breaks existing callers of `negate` who expect
    //       an inherent method on `Collection`.
    pub fn negate(&self) -> Collection<G, D, R, C>
    where 
        StreamCore<G, C>: crate::operators::Negate<G, C>
    {
        crate::operators::Negate::negate(&self.inner).as_collection()
    }
}

impl<G: Scope, D: Clone+'static, R: Clone+'static> Collection<G, D, R> {
    /// Creates a new collection by applying the supplied function to each input element.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///     scope.new_collection_from(1 .. 10).1
    ///          .map(|x| x * 2)
    ///          .filter(|x| x % 2 == 1)
    ///          .assert_empty();
    /// });
    /// ```
    pub fn map<D2, L>(&self, mut logic: L) -> Collection<G, D2, R>
    where
        D2: Data,
        L: FnMut(D) -> D2 + 'static,
    {
        self.inner
            .map(move |(data, time, delta)| (logic(data), time, delta))
            .as_collection()
    }
    /// Creates a new collection by applying the supplied function to each input element.
    ///
    /// Although the name suggests in-place mutation, this function does not change the source collection,
    /// but rather re-uses the underlying allocations in its implementation. The method is semantically
    /// equivalent to `map`, but can be more efficient.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///     scope.new_collection_from(1 .. 10).1
    ///          .map_in_place(|x| *x *= 2)
    ///          .filter(|x| x % 2 == 1)
    ///          .assert_empty();
    /// });
    /// ```
    pub fn map_in_place<L>(&self, mut logic: L) -> Collection<G, D, R>
    where
        L: FnMut(&mut D) + 'static,
    {
        self.inner
            .map_in_place(move |&mut (ref mut data, _, _)| logic(data))
            .as_collection()
    }
    /// Creates a new collection by applying the supplied function to each input element and accumulating the results.
    ///
    /// This method extracts an iterator from each input element, and extracts the full contents of the iterator. Be
    /// warned that if the iterators produce substantial amounts of data, they are currently fully drained before
    /// attempting to consolidate the results.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///     scope.new_collection_from(1 .. 10).1
    ///          .flat_map(|x| 0 .. x);
    /// });
    /// ```
    pub fn flat_map<I, L>(&self, mut logic: L) -> Collection<G, I::Item, R>
    where
        G::Timestamp: Clone,
        I: IntoIterator<Item: Data>,
        L: FnMut(D) -> I + 'static,
    {
        self.inner
            .flat_map(move |(data, time, delta)| logic(data).into_iter().map(move |x| (x, time.clone(), delta.clone())))
            .as_collection()
    }
    /// Creates a new collection containing those input records satisfying the supplied predicate.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///     scope.new_collection_from(1 .. 10).1
    ///          .map(|x| x * 2)
    ///          .filter(|x| x % 2 == 1)
    ///          .assert_empty();
    /// });
    /// ```
    pub fn filter<L>(&self, mut logic: L) -> Collection<G, D, R>
    where
        L: FnMut(&D) -> bool + 'static,
    {
        self.inner
            .filter(move |(data, _, _)| logic(data))
            .as_collection()
    }
    /// Replaces each record with another, with a new difference type.
    ///
    /// This method is most commonly used to take records containing aggregatable data (e.g. numbers to be summed)
    /// and move the data into the difference component. This will allow differential dataflow to update in-place.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///
    ///     let nums = scope.new_collection_from(0 .. 10).1;
    ///     let x1 = nums.flat_map(|x| 0 .. x);
    ///     let x2 = nums.map(|x| (x, 9 - x))
    ///                  .explode(|(x,y)| Some((x,y)));
    ///
    ///     x1.assert_eq(&x2);
    /// });
    /// ```
    pub fn explode<D2, R2, I, L>(&self, mut logic: L) -> Collection<G, D2, <R2 as Multiply<R>>::Output>
    where
        D2: Data,
        R2: Semigroup+Multiply<R, Output: Semigroup+'static>,
        I: IntoIterator<Item=(D2,R2)>,
        L: FnMut(D)->I+'static,
    {
        self.inner
            .flat_map(move |(x, t, d)| logic(x).into_iter().map(move |(x,d2)| (x, t.clone(), d2.multiply(&d))))
            .as_collection()
    }

    /// (udf) Brute-force replaces each record with another w/ a new difference type.
    ///
    pub fn lift<D2, R2, I, L>(&self, mut logic: L) -> Collection<G, D2, R2>
    where
        D2: Data,
        R2: Semigroup + 'static,
        I: IntoIterator<Item = (D2, R2)>,
        L: FnMut(D) -> I + 'static,
    {
        self.inner
            .flat_map(move |(x, t, _)| logic(x).into_iter().map(move |(x, d2)| (x, t.clone(), d2)))
            .as_collection()
    }

    /// Joins each record against a collection defined by the function `logic`.
    ///
    /// This method performs what is essentially a join with the collection of records `(x, logic(x))`.
    /// Rather than materialize this second relation, `logic` is applied to each record and the appropriate
    /// modifications made to the results, namely joining timestamps and multiplying differences.
    ///
    /// #Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///     // creates `x` copies of `2*x` from time `3*x` until `4*x`,
    ///     // for x from 0 through 9.
    ///     scope.new_collection_from(0 .. 10isize).1
    ///          .join_function(|x|
    ///              //   data      time      diff
    ///              vec![(2*x, (3*x) as u64,  x),
    ///                   (2*x, (4*x) as u64, -x)]
    ///           );
    /// });
    /// ```
    pub fn join_function<D2, R2, I, L>(&self, mut logic: L) -> Collection<G, D2, <R2 as Multiply<R>>::Output>
    where
        G::Timestamp: Lattice,
        D2: Data,
        R2: Semigroup+Multiply<R, Output: Semigroup+'static>,
        I: IntoIterator<Item=(D2,G::Timestamp,R2)>,
        L: FnMut(D)->I+'static,
    {
        self.inner
            .flat_map(move |(x, t, d)| logic(x).into_iter().map(move |(x,t2,d2)| (x, t.join(&t2), d2.multiply(&d))))
            .as_collection()
    }

    /// Brings a Collection into a nested scope.
    ///
    /// # Examples
    ///
    /// ```
    /// use timely::dataflow::Scope;
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///
    ///     let data = scope.new_collection_from(1 .. 10).1;
    ///
    ///     let result = scope.region(|child| {
    ///         data.enter(child)
    ///             .leave()
    ///     });
    ///
    ///     data.assert_eq(&result);
    /// });
    /// ```
    pub fn enter<'a, T>(&self, child: &Child<'a, G, T>) -> Collection<Child<'a, G, T>, D, R>
    where
        T: Refines<<G as ScopeParent>::Timestamp>,
    {
        self.inner
            .enter(child)
            .map(|(data, time, diff)| (data, T::to_inner(time), diff))
            .as_collection()
    }

    /// Brings a Collection into a nested scope, at varying times.
    ///
    /// The `initial` function indicates the time at which each element of the Collection should appear.
    ///
    /// # Examples
    ///
    /// ```
    /// use timely::dataflow::Scope;
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///
    ///     let data = scope.new_collection_from(1 .. 10).1;
    ///
    ///     let result = scope.iterative::<u64,_,_>(|child| {
    ///         data.enter_at(child, |x| *x)
    ///             .leave()
    ///     });
    ///
    ///     data.assert_eq(&result);
    /// });
    /// ```
    pub fn enter_at<'a, T, F>(&self, child: &Iterative<'a, G, T>, mut initial: F) -> Collection<Iterative<'a, G, T>, D, R>
    where
        T: Timestamp+Hash,
        F: FnMut(&D) -> T + Clone + 'static,
        G::Timestamp: Hash,
    {
        self.inner
            .enter(child)
            .map(move |(data, time, diff)| {
                let new_time = Product::new(time, initial(&data));
                (data, new_time, diff)
            })
            .as_collection()
    }

    /// Delays each difference by a supplied function.
    ///
    /// It is assumed that `func` only advances timestamps; this is not verified, and things may go horribly
    /// wrong if that assumption is incorrect. It is also critical that `func` be monotonic: if two times are
    /// ordered, they should have the same order or compare equal once `func` is applied to them (this
    /// is because we advance the timely capability with the same logic, and it must remain `less_equal`
    /// to all of the data timestamps).
    pub fn delay<F>(&self, func: F) -> Collection<G, D, R>
    where
        F: FnMut(&G::Timestamp) -> G::Timestamp + Clone + 'static,
    {
        let mut func1 = func.clone();
        let mut func2 = func.clone();

        self.inner
            .delay_batch(move |x| func1(x))
            .map_in_place(move |x| x.1 = func2(&x.1))
            .as_collection()
    }

    /// Applies a supplied function to each update.
    ///
    /// This method is most commonly used to report information back to the user, often for debugging purposes.
    /// Any function can be used here, but be warned that the incremental nature of differential dataflow does
    /// not guarantee that it will be called as many times as you might expect.
    ///
    /// The `(data, time, diff)` triples indicate a change `diff` to the frequency of `data` which takes effect
    /// at the logical time `time`. When times are totally ordered (for example, `usize`), these updates reflect
    /// the changes along the sequence of collections. For partially ordered times, the mathematics are more
    /// interesting and less intuitive, unfortunately.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///     scope.new_collection_from(1 .. 10).1
    ///          .map_in_place(|x| *x *= 2)
    ///          .filter(|x| x % 2 == 1)
    ///          .inspect(|x| println!("error: {:?}", x));
    /// });
    /// ```
    pub fn inspect<F>(&self, func: F) -> Collection<G, D, R>
    where
        F: FnMut(&(D, G::Timestamp, R))+'static,
    {
        self.inner
            .inspect(func)
            .as_collection()
    }
    /// Applies a supplied function to each batch of updates.
    ///
    /// This method is analogous to `inspect`, but operates on batches and reveals the timestamp of the
    /// timely dataflow capability associated with the batch of updates. The observed batching depends
    /// on how the system executes, and may vary run to run.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///     scope.new_collection_from(1 .. 10).1
    ///          .map_in_place(|x| *x *= 2)
    ///          .filter(|x| x % 2 == 1)
    ///          .inspect_batch(|t,xs| println!("errors @ {:?}: {:?}", t, xs));
    /// });
    /// ```
    pub fn inspect_batch<F>(&self, mut func: F) -> Collection<G, D, R>
    where
        F: FnMut(&G::Timestamp, &[(D, G::Timestamp, R)])+'static,
    {
        self.inner
            .inspect_batch(move |time, data| func(time, data))
            .as_collection()
    }

    /// Assert if the collection is ever non-empty.
    ///
    /// Because this is a dataflow fragment, the test is only applied as the computation is run. If the computation
    /// is not run, or not run to completion, there may be un-exercised times at which the collection could be
    /// non-empty. Typically, a timely dataflow computation runs to completion on drop, and so clean exit from a
    /// program should indicate that this assertion never found cause to complain.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///     scope.new_collection_from(1 .. 10).1
    ///          .map(|x| x * 2)
    ///          .filter(|x| x % 2 == 1)
    ///          .assert_empty();
    /// });
    /// ```
    pub fn assert_empty(&self)
    where
        D: crate::ExchangeData+Hashable,
        R: crate::ExchangeData+Hashable + Semigroup,
        G::Timestamp: Lattice+Ord,
    {
        self.consolidate()
            .inspect(|x| panic!("Assertion failed: non-empty collection: {:?}", x));
    }
}

use timely::dataflow::scopes::ScopeParent;
use timely::progress::timestamp::Refines;

/// Methods requiring a nested scope.
impl<'a, G: Scope, T: Timestamp, D: Clone+'static, R: Clone+'static> Collection<Child<'a, G, T>, D, R>
where
    T: Refines<<G as ScopeParent>::Timestamp>,
{
    /// Returns the final value of a Collection from a nested scope to its containing scope.
    ///
    /// # Examples
    ///
    /// ```
    /// use timely::dataflow::Scope;
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///
    ///    let data = scope.new_collection_from(1 .. 10).1;
    ///
    ///    let result = scope.region(|child| {
    ///         data.enter(child)
    ///             .leave()
    ///     });
    ///
    ///     data.assert_eq(&result);
    /// });
    /// ```
    pub fn leave(&self) -> Collection<G, D, R> {
        self.inner
            .leave()
            .map(|(data, time, diff)| (data, time.to_outer(), diff))
            .as_collection()
    }
}

/// Methods requiring a region as the scope.
impl<G: Scope, D, R, C: Container+Data> Collection<Child<'_, G, G::Timestamp>, D, R, C>
{
    /// Returns the value of a Collection from a nested region to its containing scope.
    ///
    /// This method is a specialization of `leave` to the case that of a nested region.
    /// It removes the need for an operator that adjusts the timestamp.
    pub fn leave_region(&self) -> Collection<G, D, R, C> {
        self.inner
            .leave()
            .as_collection()
    }
}

/// Methods requiring an Abelian difference, to support negation.
impl<G: Scope<Timestamp: Data>, D: Clone+'static, R: Abelian+'static> Collection<G, D, R> {
    /// Assert if the collections are ever different.
    ///
    /// Because this is a dataflow fragment, the test is only applied as the computation is run. If the computation
    /// is not run, or not run to completion, there may be un-exercised times at which the collections could vary.
    /// Typically, a timely dataflow computation runs to completion on drop, and so clean exit from a program should
    /// indicate that this assertion never found cause to complain.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    ///
    /// ::timely::example(|scope| {
    ///
    ///     let data = scope.new_collection_from(1 .. 10).1;
    ///
    ///     let odds = data.filter(|x| x % 2 == 1);
    ///     let evens = data.filter(|x| x % 2 == 0);
    ///
    ///     odds.concat(&evens)
    ///         .assert_eq(&data);
    /// });
    /// ```
    pub fn assert_eq(&self, other: &Self)
    where
        D: crate::ExchangeData+Hashable,
        R: crate::ExchangeData+Hashable,
        G::Timestamp: Lattice+Ord,
    {
        self.negate()
            .concat(other)
            .assert_empty();
    }
}

/// Conversion to a differential dataflow Collection.
pub trait AsCollection<G: Scope, D, R, C> {
    /// Converts the type to a differential dataflow collection.
    fn as_collection(&self) -> Collection<G, D, R, C>;
}

impl<G: Scope, D, R, C: Clone> AsCollection<G, D, R, C> for StreamCore<G, C> {
    /// Converts the type to a differential dataflow collection.
    ///
    /// By calling this method, you guarantee that the timestamp invariant (as documented on
    /// [Collection]) is upheld. This method will not check it.
    fn as_collection(&self) -> Collection<G, D, R, C> {
        Collection::<G,D,R,C>::new(self.clone())
    }
}

/// Concatenates multiple collections.
///
/// This method has the effect of a sequence of calls to `concat`, but it does
/// so in one operator rather than a chain of many operators.
///
/// # Examples
///
/// ```
/// use differential_dataflow::input::Input;
///
/// ::timely::example(|scope| {
///
///     let data = scope.new_collection_from(1 .. 10).1;
///
///     let odds = data.filter(|x| x % 2 == 1);
///     let evens = data.filter(|x| x % 2 == 0);
///
///     differential_dataflow::collection::concatenate(scope, vec![odds, evens])
///         .assert_eq(&data);
/// });
/// ```
pub fn concatenate<G, D, R, C, I>(scope: &mut G, iterator: I) -> Collection<G, D, R, C>
where
    G: Scope,
    D: Data,
    R: Semigroup + 'static,
    C: Container + Clone + 'static,
    I: IntoIterator<Item=Collection<G, D, R, C>>,
{
    scope
        .concatenate(iterator.into_iter().map(|x| x.inner))
        .as_collection()
}
