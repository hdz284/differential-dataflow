//! Applies a reduction function on records grouped by key.
//!
//! The `reduce` operator acts on `(key, val)` data.
//! Records with the same key are grouped together, and a user-supplied reduction function is applied
//! to the key and the list of values.
//! The function is expected to populate a list of output values.

use timely::Container;
use timely::container::PushInto;
use crate::hashable::Hashable;
use crate::{Data, ExchangeData, Collection};
use crate::difference::{Semigroup, Abelian};

use timely::order::PartialOrder;
use timely::progress::frontier::Antichain;
use timely::progress::Timestamp;
use timely::dataflow::*;
use timely::dataflow::operators::Operator;
use timely::dataflow::channels::pact::Pipeline;
use timely::dataflow::operators::Capability;

use crate::operators::arrange::{Arranged, ArrangeByKey, ArrangeBySelf, TraceAgent};
use crate::lattice::Lattice;
use crate::trace::{BatchReader, Cursor, Trace, Builder, ExertionLogic, Description};
use crate::trace::cursor::CursorList;
use crate::trace::implementations::{KeySpine, KeyBuilder, ValSpine, ValBuilder};
use crate::trace::implementations::containers::BatchContainer;

use crate::trace::TraceReader;

/// Extension trait for the `reduce` differential dataflow method.
pub trait Reduce<G: Scope<Timestamp: Lattice+Ord>, K: Data, V: Data, R: Semigroup> {
    /// Applies a reduction function on records grouped by key.
    ///
    /// Input data must be structured as `(key, val)` pairs.
    /// The user-supplied reduction function takes as arguments
    ///
    /// 1. a reference to the key,
    /// 2. a reference to the slice of values and their accumulated updates,
    /// 3. a mutuable reference to a vector to populate with output values and accumulated updates.
    ///
    /// The user logic is only invoked for non-empty input collections, and it is safe to assume that the
    /// slice of input values is non-empty. The values are presented in sorted order, as defined by their
    /// `Ord` implementations.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    /// use differential_dataflow::operators::Reduce;
    ///
    /// ::timely::example(|scope| {
    ///     // report the smallest value for each group
    ///     scope.new_collection_from(1 .. 10).1
    ///          .map(|x| (x / 3, x))
    ///          .reduce(|_key, input, output| {
    ///              output.push((*input[0].0, 1))
    ///          });
    /// });
    /// ```
    fn reduce<L, V2: Data, R2: Ord+Abelian+'static>(&self, logic: L) -> Collection<G, (K, V2), R2>
    where L: FnMut(&K, &[(&V, R)], &mut Vec<(V2, R2)>)+'static {
        self.reduce_named("Reduce", logic)
    }

    /// As `reduce` with the ability to name the operator.
    fn reduce_named<L, V2: Data, R2: Ord+Abelian+'static>(&self, name: &str, logic: L) -> Collection<G, (K, V2), R2>
    where L: FnMut(&K, &[(&V, R)], &mut Vec<(V2, R2)>)+'static;
}

impl<G, K, V, R> Reduce<G, K, V, R> for Collection<G, (K, V), R>
    where
        G: Scope<Timestamp: Lattice+Ord>,
        K: ExchangeData+Hashable,
        V: ExchangeData,
        R: ExchangeData+Semigroup,
 {
    fn reduce_named<L, V2: Data, R2: Ord+Abelian+'static>(&self, name: &str, logic: L) -> Collection<G, (K, V2), R2>
        where L: FnMut(&K, &[(&V, R)], &mut Vec<(V2, R2)>)+'static {
        self.arrange_by_key_named(&format!("Arrange: {}", name))
            .reduce_named(name, logic)
    }
}

impl<G, K: Data, V: Data, T1, R: Ord+Semigroup+'static> Reduce<G, K, V, R> for Arranged<G, T1>
where
    G: Scope<Timestamp=T1::Time>,
    T1: for<'a> TraceReader<Key<'a>=&'a K, KeyOwn = K, Val<'a>=&'a V, Diff=R>+Clone+'static,
{
    fn reduce_named<L, V2: Data, R2: Ord+Abelian+'static>(&self, name: &str, logic: L) -> Collection<G, (K, V2), R2>
        where L: FnMut(&K, &[(&V, R)], &mut Vec<(V2, R2)>)+'static {
        self.reduce_abelian::<_,K,V2,ValBuilder<_,_,_,_>,ValSpine<_,_,_,_>>(name, logic)
            .as_collection(|k,v| (k.clone(), v.clone()))
    }
}

/// Extension trait for the `threshold` and `distinct` differential dataflow methods.
pub trait Threshold<G: Scope<Timestamp: Lattice+Ord>, K: Data, R1: Semigroup> {
    /// Transforms the multiplicity of records.
    ///
    /// The `threshold` function is obliged to map `R1::zero` to `R2::zero`, or at
    /// least the computation may behave as if it does. Otherwise, the transformation
    /// can be nearly arbitrary: the code does not assume any properties of `threshold`.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    /// use differential_dataflow::operators::Threshold;
    ///
    /// ::timely::example(|scope| {
    ///     // report at most one of each key.
    ///     scope.new_collection_from(1 .. 10).1
    ///          .map(|x| x / 3)
    ///          .threshold(|_,c| c % 2);
    /// });
    /// ```
    fn threshold<R2: Ord+Abelian+'static, F: FnMut(&K, &R1)->R2+'static>(&self, thresh: F) -> Collection<G, K, R2> {
        self.threshold_named("Threshold", thresh)
    }

    /// A `threshold` with the ability to name the operator.
    fn threshold_named<R2: Ord+Abelian+'static, F: FnMut(&K, &R1)->R2+'static>(&self, name: &str, thresh: F) -> Collection<G, K, R2>;

    /// Reduces the collection to one occurrence of each distinct element.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    /// use differential_dataflow::operators::Threshold;
    ///
    /// ::timely::example(|scope| {
    ///     // report at most one of each key.
    ///     scope.new_collection_from(1 .. 10).1
    ///          .map(|x| x / 3)
    ///          .distinct();
    /// });
    /// ```
    fn distinct(&self) -> Collection<G, K, isize> {
        self.distinct_core()
    }

    /// Distinct for general integer differences.
    ///
    /// This method allows `distinct` to produce collections whose difference
    /// type is something other than an `isize` integer, for example perhaps an
    /// `i32`.
    fn distinct_core<R2: Ord+Abelian+'static+From<i8>>(&self) -> Collection<G, K, R2> {
        self.threshold_named("Distinct", |_,_| R2::from(1i8))
    }
}

impl<G: Scope<Timestamp: Lattice+Ord>, K: ExchangeData+Hashable, R1: ExchangeData+Semigroup> Threshold<G, K, R1> for Collection<G, K, R1> {
    fn threshold_named<R2: Ord+Abelian+'static, F: FnMut(&K,&R1)->R2+'static>(&self, name: &str, thresh: F) -> Collection<G, K, R2> {
        self.arrange_by_self_named(&format!("Arrange: {}", name))
            .threshold_named(name, thresh)
    }
}

impl<G, K: Data, T1, R1: Semigroup> Threshold<G, K, R1> for Arranged<G, T1>
where
    G: Scope<Timestamp=T1::Time>,
    T1: for<'a> TraceReader<Key<'a>=&'a K, KeyOwn = K, Val<'a>=&'a (), Diff=R1>+Clone+'static,
{
    fn threshold_named<R2: Ord+Abelian+'static, F: FnMut(&K,&R1)->R2+'static>(&self, name: &str, mut thresh: F) -> Collection<G, K, R2> {
        self.reduce_abelian::<_,K,(),KeyBuilder<K,G::Timestamp,R2>,KeySpine<K,G::Timestamp,R2>>(name, move |k,s,t| t.push(((), thresh(k, &s[0].1))))
            .as_collection(|k,_| k.clone())
    }
}

/// Extension trait for the `count` differential dataflow method.
pub trait Count<G: Scope<Timestamp: Lattice+Ord>, K: Data, R: Semigroup> {
    /// Counts the number of occurrences of each element.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    /// use differential_dataflow::operators::Count;
    ///
    /// ::timely::example(|scope| {
    ///     // report the number of occurrences of each key
    ///     scope.new_collection_from(1 .. 10).1
    ///          .map(|x| x / 3)
    ///          .count();
    /// });
    /// ```
    fn count(&self) -> Collection<G, (K, R), isize> {
        self.count_core()
    }

    /// Count for general integer differences.
    ///
    /// This method allows `count` to produce collections whose difference
    /// type is something other than an `isize` integer, for example perhaps an
    /// `i32`.
    fn count_core<R2: Ord + Abelian + From<i8> + 'static>(&self) -> Collection<G, (K, R), R2>;
}

impl<G: Scope<Timestamp: Lattice+Ord>, K: ExchangeData+Hashable, R: ExchangeData+Semigroup> Count<G, K, R> for Collection<G, K, R> {
    fn count_core<R2: Ord + Abelian + From<i8> + 'static>(&self) -> Collection<G, (K, R), R2> {
        self.arrange_by_self_named("Arrange: Count")
            .count_core()
    }
}

impl<G, K: Data, T1, R: Data+Semigroup> Count<G, K, R> for Arranged<G, T1>
where
    G: Scope<Timestamp=T1::Time>,
    T1: for<'a> TraceReader<Key<'a>=&'a K, KeyOwn = K, Val<'a>=&'a (), Diff=R>+Clone+'static,
{
    fn count_core<R2: Ord + Abelian + From<i8> + 'static>(&self) -> Collection<G, (K, R), R2> {
        self.reduce_abelian::<_,K,R,ValBuilder<K,R,G::Timestamp,R2>,ValSpine<K,R,G::Timestamp,R2>>("Count", |_k,s,t| t.push((s[0].1.clone(), R2::from(1i8))))
            .as_collection(|k,c| (k.clone(), c.clone()))
    }
}

/// Extension trait for the `reduce_core` differential dataflow method.
pub trait ReduceCore<G: Scope<Timestamp: Lattice+Ord>, K: ToOwned + ?Sized, V: Data, R: Semigroup> {
    /// Applies `reduce` to arranged data, and returns an arrangement of output data.
    ///
    /// This method is used by the more ergonomic `reduce`, `distinct`, and `count` methods, although
    /// it can be very useful if one needs to manually attach and re-use existing arranged collections.
    ///
    /// # Examples
    ///
    /// ```
    /// use differential_dataflow::input::Input;
    /// use differential_dataflow::operators::reduce::ReduceCore;
    /// use differential_dataflow::trace::Trace;
    /// use differential_dataflow::trace::implementations::{ValBuilder, ValSpine};
    ///
    /// ::timely::example(|scope| {
    ///
    ///     let trace =
    ///     scope.new_collection_from(1 .. 10u32).1
    ///          .map(|x| (x, x))
    ///          .reduce_abelian::<_,ValBuilder<_,_,_,_>,ValSpine<_,_,_,_>>(
    ///             "Example",
    ///              move |_key, src, dst| dst.push((*src[0].0, 1))
    ///          )
    ///          .trace;
    /// });
    /// ```
    fn reduce_abelian<L, Bu, T2>(&self, name: &str, mut logic: L) -> Arranged<G, TraceAgent<T2>>
        where
            T2: for<'a> Trace<
                Key<'a>= &'a K,
                KeyOwn = K,
                ValOwn = V,
                Time=G::Timestamp,
                Diff: Abelian,
            >+'static,
            Bu: Builder<Time=T2::Time, Input = Vec<((K::Owned, V), T2::Time, T2::Diff)>, Output = T2::Batch>,
            L: FnMut(&K, &[(&V, R)], &mut Vec<(V, T2::Diff)>)+'static,
        {
            self.reduce_core::<_,Bu,T2>(name, move |key, input, output, change| {
                if !input.is_empty() {
                    logic(key, input, change);
                }
                change.extend(output.drain(..).map(|(x,mut d)| { d.negate(); (x, d) }));
                crate::consolidation::consolidate(change);
            })
        }

    /// Solves for output updates when presented with inputs and would-be outputs.
    ///
    /// Unlike `reduce_arranged`, this method may be called with an empty `input`,
    /// and it may not be safe to index into the first element.
    /// At least one of the two collections will be non-empty.
    fn reduce_core<L, Bu, T2>(&self, name: &str, logic: L) -> Arranged<G, TraceAgent<T2>>
        where
            T2: for<'a> Trace<
                Key<'a>=&'a K,
                KeyOwn = K,
                ValOwn = V,
                Time=G::Timestamp,
            >+'static,
            Bu: Builder<Time=T2::Time, Input = Vec<((K::Owned, V), T2::Time, T2::Diff)>, Output = T2::Batch>,
            L: FnMut(&K, &[(&V, R)], &mut Vec<(V,T2::Diff)>, &mut Vec<(V, T2::Diff)>)+'static,
            ;
}

impl<G, K, V, R> ReduceCore<G, K, V, R> for Collection<G, (K, V), R>
where
    G: Scope,
    G::Timestamp: Lattice+Ord,
    K: ExchangeData+Hashable,
    V: ExchangeData,
    R: ExchangeData+Semigroup,
{
    fn reduce_core<L, Bu, T2>(&self, name: &str, logic: L) -> Arranged<G, TraceAgent<T2>>
        where
            V: Data,
            T2: for<'a> Trace<
                Key<'a>=&'a K,
                KeyOwn = K,
                ValOwn = V,
                Time=G::Timestamp,
            >+'static,
            Bu: Builder<Time=T2::Time, Input = Vec<((K, V), T2::Time, T2::Diff)>, Output = T2::Batch>,
            L: FnMut(&K, &[(&V, R)], &mut Vec<(V,T2::Diff)>, &mut Vec<(V, T2::Diff)>)+'static,
    {
        self.arrange_by_key_named(&format!("Arrange: {}", name))
            .reduce_core::<_,_,_,Bu,_>(name, logic)
    }
}

/// A key-wise reduction of values in an input trace.
///
/// This method exists to provide reduce functionality without opinions about qualifying trace types.
pub fn reduce_trace<G, T1, Bu, T2, K, V, L>(trace: &Arranged<G, T1>, name: &str, mut logic: L) -> Arranged<G, TraceAgent<T2>>
where
    G: Scope<Timestamp=T1::Time>,
    T1: for<'a> TraceReader<KeyOwn = K> + Clone + 'static,
    T2: for<'a> Trace<Key<'a>=T1::Key<'a>, ValOwn = V, Time=T1::Time> + 'static,
    K: Ord + 'static,
    V: Data,
    Bu: Builder<Time=T2::Time, Output = T2::Batch, Input: Container + PushInto<((K, V), T2::Time, T2::Diff)>>,
    L: FnMut(T1::Key<'_>, &[(T1::Val<'_>, T1::Diff)], &mut Vec<(V,T2::Diff)>, &mut Vec<(V, T2::Diff)>)+'static,
{
    let mut result_trace = None;

    // fabricate a data-parallel operator using the `unary_notify` pattern.
    let stream = {

        let result_trace = &mut result_trace;
        trace.stream.unary_frontier(Pipeline, name, move |_capability, operator_info| {

            // Acquire a logger for arrange events.
            let logger = trace.stream.scope().logger_for::<crate::logging::DifferentialEventBuilder>("differential/arrange").map(Into::into);

            let activator = Some(trace.stream.scope().activator_for(operator_info.address.clone()));
            let mut empty = T2::new(operator_info.clone(), logger.clone(), activator);
            // If there is default exert logic set, install it.
            if let Some(exert_logic) = trace.stream.scope().config().get::<ExertionLogic>("differential/default_exert_logic").cloned() {
                empty.set_exert_logic(exert_logic);
            }


            let mut source_trace = trace.trace.clone();

            let (mut output_reader, mut output_writer) = TraceAgent::new(empty, operator_info, logger);

            // let mut output_trace = TraceRc::make_from(agent).0;
            *result_trace = Some(output_reader.clone());

            // let mut thinker1 = history_replay_prior::HistoryReplayer::<V, V2, G::Timestamp, R, R2>::new();
            // let mut thinker = history_replay::HistoryReplayer::<V, V2, G::Timestamp, R, R2>::new();
            let mut new_interesting_times = Vec::<G::Timestamp>::new();

            // Our implementation maintains a list of outstanding `(key, time)` synthetic interesting times,
            // as well as capabilities for these times (or their lower envelope, at least).
            let mut interesting = Vec::<(K, G::Timestamp)>::new();
            let mut capabilities = Vec::<Capability<G::Timestamp>>::new();

            // buffers and logic for computing per-key interesting times "efficiently".
            let mut interesting_times = Vec::<G::Timestamp>::new();

            // Upper and lower frontiers for the pending input and output batches to process.
            let mut upper_limit = Antichain::from_elem(<G::Timestamp as timely::progress::Timestamp>::minimum());
            let mut lower_limit = Antichain::from_elem(<G::Timestamp as timely::progress::Timestamp>::minimum());

            // Output batches may need to be built piecemeal, and these temp storage help there.
            let mut output_upper = Antichain::from_elem(<G::Timestamp as timely::progress::Timestamp>::minimum());
            let mut output_lower = Antichain::from_elem(<G::Timestamp as timely::progress::Timestamp>::minimum());

            let id = trace.stream.scope().index();

            move |input, output| {

                // The `reduce` operator receives fully formed batches, which each serve as an indication
                // that the frontier has advanced to the upper bound of their description.
                //
                // Although we could act on each individually, several may have been sent, and it makes
                // sense to accumulate them first to coordinate their re-evaluation. We will need to pay
                // attention to which times need to be collected under which capability, so that we can
                // assemble output batches correctly. We will maintain several builders concurrently, and
                // place output updates into the appropriate builder.
                //
                // It turns out we must use notificators, as we cannot await empty batches from arrange to
                // indicate progress, as the arrange may not hold the capability to send such. Instead, we
                // must watch for progress here (and the upper bound of received batches) to tell us how
                // far we can process work.
                //
                // We really want to retire all batches we receive, so we want a frontier which reflects
                // both information from batches as well as progress information. I think this means that
                // we keep times that are greater than or equal to a time in the other frontier, deduplicated.

                let mut batch_cursors = Vec::new();
                let mut batch_storage = Vec::new();

                // Downgrade previous upper limit to be current lower limit.
                lower_limit.clear();
                lower_limit.extend(upper_limit.borrow().iter().cloned());

                // Drain the input stream of batches, validating the contiguity of the batch descriptions and
                // capturing a cursor for each of the batches as well as ensuring we hold a capability for the
                // times in the batch.
                input.for_each(|capability, batches| {

                    for batch in batches.drain(..) {
                        upper_limit.clone_from(batch.upper());
                        batch_cursors.push(batch.cursor());
                        batch_storage.push(batch);
                    }

                    // Ensure that `capabilities` covers the capability of the batch.
                    capabilities.retain(|cap| !capability.time().less_than(cap.time()));
                    if !capabilities.iter().any(|cap| cap.time().less_equal(capability.time())) {
                        capabilities.push(capability.retain());
                    }
                });

                // Pull in any subsequent empty batches we believe to exist.
                source_trace.advance_upper(&mut upper_limit);

                // Only if our upper limit has advanced should we do work.
                if upper_limit != lower_limit {

                    // If we have no capabilities, then we (i) should not produce any outputs and (ii) could not send
                    // any produced outputs even if they were (incorrectly) produced. We cannot even send empty batches
                    // to indicate forward progress, and must hope that downstream operators look at progress frontiers
                    // as well as batch descriptions.
                    //
                    // We can (and should) advance source and output traces if `upper_limit` indicates this is possible.
                    if capabilities.iter().any(|c| !upper_limit.less_equal(c.time())) {

                        // `interesting` contains "warnings" about keys and times that may need to be re-considered.
                        // We first extract those times from this list that lie in the interval we will process.
                        sort_dedup(&mut interesting);
                        // `exposed` contains interesting (key, time)s now below `upper_limit`
                        let mut exposed_keys = T1::KeyContainer::with_capacity(0);
                        let mut exposed_time = T1::TimeContainer::with_capacity(0);
                        // Keep pairs greater or equal to `upper_limit`, and "expose" other pairs.
                        interesting.retain(|(key, time)| {
                            if upper_limit.less_equal(time) { true } else {
                                exposed_keys.push_own(key);
                                exposed_time.push_own(time);
                                false
                            }
                        });

                        // Prepare an output buffer and builder for each capability.
                        //
                        // We buffer and build separately, as outputs are produced grouped by time, whereas the
                        // builder wants to see outputs grouped by value. While the per-key computation could
                        // do the re-sorting itself, buffering per-key outputs lets us double check the results
                        // against other implementations for accuracy.
                        //
                        // TODO: It would be better if all updates went into one batch, but timely dataflow prevents
                        //       this as long as it requires that there is only one capability for each message.
                        let mut buffers = Vec::<(G::Timestamp, Vec<(V, G::Timestamp, T2::Diff)>)>::new();
                        let mut builders = Vec::new();
                        for cap in capabilities.iter() {
                            buffers.push((cap.time().clone(), Vec::new()));
                            builders.push(Bu::new());
                        }

                        let mut buffer = Bu::Input::default();

                        // cursors for navigating input and output traces.
                        let (mut source_cursor, source_storage): (T1::Cursor, _) = source_trace.cursor_through(lower_limit.borrow()).expect("failed to acquire source cursor");
                        let source_storage = &source_storage;
                        let (mut output_cursor, output_storage): (T2::Cursor, _) = output_reader.cursor_through(lower_limit.borrow()).expect("failed to acquire output cursor");
                        let output_storage = &output_storage;
                        let (mut batch_cursor, batch_storage) = (CursorList::new(batch_cursors, &batch_storage), batch_storage);
                        let batch_storage = &batch_storage;

                        let mut thinker = history_replay::HistoryReplayer::new();

                        // We now march through the keys we must work on, drawing from `batch_cursors` and `exposed`.
                        //
                        // We only keep valid cursors (those with more data) in `batch_cursors`, and so its length
                        // indicates whether more data remain. We move through `exposed` using (index) `exposed_position`.
                        // There could perhaps be a less provocative variable name.
                        let mut exposed_position = 0;
                        while batch_cursor.key_valid(batch_storage) || exposed_position < exposed_keys.len() {

                            // Determine the next key we will work on; could be synthetic, could be from a batch.
                            let key1 = exposed_keys.get(exposed_position);
                            let key2 = batch_cursor.get_key(batch_storage);
                            let key = match (key1, key2) {
                                (Some(key1), Some(key2)) => ::std::cmp::min(key1, key2),
                                (Some(key1), None)       => key1,
                                (None, Some(key2))       => key2,
                                (None, None)             => unreachable!(),
                            };

                            // `interesting_times` contains those times between `lower_issued` and `upper_limit`
                            // that we need to re-consider. We now populate it, but perhaps this should be left
                            // to the per-key computation, which may be able to avoid examining the times of some
                            // values (for example, in the case of min/max/topk).
                            interesting_times.clear();

                            // Populate `interesting_times` with synthetic interesting times (below `upper_limit`) for this key.
                            while exposed_keys.get(exposed_position) == Some(key) {
                                interesting_times.push(T1::owned_time(exposed_time.index(exposed_position)));
                                exposed_position += 1;
                            }

                            // tidy up times, removing redundancy.
                            sort_dedup(&mut interesting_times);

                            // do the per-key computation.
                            let _counters = thinker.compute(
                                key,
                                (&mut source_cursor, source_storage),
                                (&mut output_cursor, output_storage),
                                (&mut batch_cursor, batch_storage),
                                &mut interesting_times,
                                &mut logic,
                                &upper_limit,
                                &mut buffers[..],
                                &mut new_interesting_times,
                            );

                            if batch_cursor.get_key(batch_storage) == Some(key) {
                                batch_cursor.step_key(batch_storage);
                            }

                            // Record future warnings about interesting times (and assert they should be "future").
                            for time in new_interesting_times.drain(..) {
                                debug_assert!(upper_limit.less_equal(&time));
                                interesting.push((T1::owned_key(key), time));
                            }

                            // Sort each buffer by value and move into the corresponding builder.
                            // TODO: This makes assumptions about at least one of (i) the stability of `sort_by`,
                            //       (ii) that the buffers are time-ordered, and (iii) that the builders accept
                            //       arbitrarily ordered times.
                            for index in 0 .. buffers.len() {
                                buffers[index].1.sort_by(|x,y| x.0.cmp(&y.0));
                                for (val, time, diff) in buffers[index].1.drain(..) {
                                    buffer.push_into(((T1::owned_key(key), val), time, diff));
                                    builders[index].push(&mut buffer);
                                    buffer.clear();
                                }
                            }
                        }

                        // We start sealing output batches from the lower limit (previous upper limit).
                        // In principle, we could update `lower_limit` itself, and it should arrive at
                        // `upper_limit` by the end of the process.
                        output_lower.clear();
                        output_lower.extend(lower_limit.borrow().iter().cloned());

                        // build and ship each batch (because only one capability per message).
                        for (index, builder) in builders.drain(..).enumerate() {

                            // Form the upper limit of the next batch, which includes all times greater
                            // than the input batch, or the capabilities from i + 1 onward.
                            output_upper.clear();
                            output_upper.extend(upper_limit.borrow().iter().cloned());
                            for capability in &capabilities[index + 1 ..] {
                                output_upper.insert(capability.time().clone());
                            }

                            if output_upper.borrow() != output_lower.borrow() {

                                let description = Description::new(output_lower.clone(), output_upper.clone(), Antichain::from_elem(G::Timestamp::minimum()));
                                let batch = builder.done(description);

                                // ship batch to the output, and commit to the output trace.
                                output.session(&capabilities[index]).give(batch.clone());
                                output_writer.insert(batch, Some(capabilities[index].time().clone()));

                                output_lower.clear();
                                output_lower.extend(output_upper.borrow().iter().cloned());
                            }
                        }

                        // This should be true, as the final iteration introduces no capabilities, and
                        // uses exactly `upper_limit` to determine the upper bound. Good to check though.
                        assert!(output_upper.borrow() == upper_limit.borrow());

                        // Determine the frontier of our interesting times.
                        let mut frontier = Antichain::<G::Timestamp>::new();
                        for (_, time) in &interesting {
                            frontier.insert_ref(time);
                        }

                        // Update `capabilities` to reflect interesting pairs described by `frontier`.
                        let mut new_capabilities = Vec::new();
                        for time in frontier.borrow().iter() {
                            if let Some(cap) = capabilities.iter().find(|c| c.time().less_equal(time)) {
                                new_capabilities.push(cap.delayed(time));
                            }
                            else {
                                println!("{}:\tfailed to find capability less than new frontier time:", id);
                                println!("{}:\t  time: {:?}", id, time);
                                println!("{}:\t  caps: {:?}", id, capabilities);
                                println!("{}:\t  uppr: {:?}", id, upper_limit);
                            }
                        }
                        capabilities = new_capabilities;

                        // ensure that observed progress is reflected in the output.
                        output_writer.seal(upper_limit.clone());
                    }
                    else {
                        output_writer.seal(upper_limit.clone());
                    }

                    // We only anticipate future times in advance of `upper_limit`.
                    source_trace.set_logical_compaction(upper_limit.borrow());
                    output_reader.set_logical_compaction(upper_limit.borrow());

                    // We will only slice the data between future batches.
                    source_trace.set_physical_compaction(upper_limit.borrow());
                    output_reader.set_physical_compaction(upper_limit.borrow());
                }

                // Exert trace maintenance if we have been so requested.
                output_writer.exert();
            }
        }
    )
    };

    Arranged { stream, trace: result_trace.unwrap() }
}


#[inline(never)]
fn sort_dedup<T: Ord>(list: &mut Vec<T>) {
    list.dedup();
    list.sort();
    list.dedup();
}

trait PerKeyCompute<'a, C1, C2, C3, V>
where
    C1: Cursor,
    C2: for<'b> Cursor<Key<'a> = C1::Key<'a>, ValOwn = V, Time = C1::Time>,
    C3: Cursor<Key<'a> = C1::Key<'a>, Val<'a> = C1::Val<'a>, Time = C1::Time, Diff = C1::Diff>,
    V: Clone + Ord,
{
    fn new() -> Self;
    fn compute<L>(
        &mut self,
        key: C1::Key<'a>,
        source_cursor: (&mut C1, &'a C1::Storage),
        output_cursor: (&mut C2, &'a C2::Storage),
        batch_cursor: (&mut C3, &'a C3::Storage),
        times: &mut Vec<C1::Time>,
        logic: &mut L,
        upper_limit: &Antichain<C1::Time>,
        outputs: &mut [(C2::Time, Vec<(V, C2::Time, C2::Diff)>)],
        new_interesting: &mut Vec<C1::Time>) -> (usize, usize)
    where
        L: FnMut(
            C1::Key<'a>,
            &[(C1::Val<'a>, C1::Diff)],
            &mut Vec<(V, C2::Diff)>,
            &mut Vec<(V, C2::Diff)>,
        );
}


/// Implementation based on replaying historical and new updates together.
mod history_replay {

    use timely::progress::Antichain;
    use timely::PartialOrder;

    use crate::lattice::Lattice;
    use crate::trace::Cursor;
    use crate::operators::ValueHistory;

    use super::{PerKeyCompute, sort_dedup};

    /// The `HistoryReplayer` is a compute strategy based on moving through existing inputs, interesting times, etc in
    /// time order, maintaining consolidated representations of updates with respect to future interesting times.
    pub struct HistoryReplayer<'a, C1, C2, C3, V>
    where
        C1: Cursor,
        C2: Cursor<Key<'a> = C1::Key<'a>, Time = C1::Time>,
        C3: Cursor<Key<'a> = C1::Key<'a>, Val<'a> = C1::Val<'a>, Time = C1::Time, Diff = C1::Diff>,
        V: Clone + Ord,
    {
        input_history: ValueHistory<'a, C1>,
        output_history: ValueHistory<'a, C2>,
        batch_history: ValueHistory<'a, C3>,
        input_buffer: Vec<(C1::Val<'a>, C1::Diff)>,
        output_buffer: Vec<(V, C2::Diff)>,
        update_buffer: Vec<(V, C2::Diff)>,
        output_produced: Vec<((V, C2::Time), C2::Diff)>,
        synth_times: Vec<C1::Time>,
        meets: Vec<C1::Time>,
        times_current: Vec<C1::Time>,
        temporary: Vec<C1::Time>,
    }

    impl<'a, C1, C2, C3, V> PerKeyCompute<'a, C1, C2, C3, V> for HistoryReplayer<'a, C1, C2, C3, V>
    where
        C1: Cursor,
        C2: for<'b> Cursor<Key<'a> = C1::Key<'a>, ValOwn = V, Time = C1::Time>,
        C3: Cursor<Key<'a> = C1::Key<'a>, Val<'a> = C1::Val<'a>, Time = C1::Time, Diff = C1::Diff>,
        V: Clone + Ord,
    {
        fn new() -> Self {
            HistoryReplayer {
                input_history: ValueHistory::new(),
                output_history: ValueHistory::new(),
                batch_history: ValueHistory::new(),
                input_buffer: Vec::new(),
                output_buffer: Vec::new(),
                update_buffer: Vec::new(),
                output_produced: Vec::new(),
                synth_times: Vec::new(),
                meets: Vec::new(),
                times_current: Vec::new(),
                temporary: Vec::new(),
            }
        }
        #[inline(never)]
        fn compute<L>(
            &mut self,
            key: C1::Key<'a>,
            (source_cursor, source_storage): (&mut C1, &'a C1::Storage),
            (output_cursor, output_storage): (&mut C2, &'a C2::Storage),
            (batch_cursor, batch_storage): (&mut C3, &'a C3::Storage),
            times: &mut Vec<C1::Time>,
            logic: &mut L,
            upper_limit: &Antichain<C1::Time>,
            outputs: &mut [(C2::Time, Vec<(V, C2::Time, C2::Diff)>)],
            new_interesting: &mut Vec<C1::Time>) -> (usize, usize)
        where
            L: FnMut(
                C1::Key<'a>,
                &[(C1::Val<'a>, C1::Diff)],
                &mut Vec<(V, C2::Diff)>,
                &mut Vec<(V, C2::Diff)>,
            )
        {

            // The work we need to perform is at times defined principally by the contents of `batch_cursor`
            // and `times`, respectively "new work we just received" and "old times we were warned about".
            //
            // Our first step is to identify these times, so that we can use them to restrict the amount of
            // information we need to recover from `input` and `output`; as all times of interest will have
            // some time from `batch_cursor` or `times`, we can compute their meet and advance all other
            // loaded times by performing the lattice `join` with this value.

            // Load the batch contents.
            let mut batch_replay = self.batch_history.replay_key(batch_cursor, batch_storage, key, |time| C3::owned_time(time));

            // We determine the meet of times we must reconsider (those from `batch` and `times`). This meet
            // can be used to advance other historical times, which may consolidate their representation. As
            // a first step, we determine the meets of each *suffix* of `times`, which we will use as we play
            // history forward.

            self.meets.clear();
            self.meets.extend(times.iter().cloned());
            for index in (1 .. self.meets.len()).rev() {
                self.meets[index-1] = self.meets[index-1].meet(&self.meets[index]);
            }

            // Determine the meet of times in `batch` and `times`.
            let mut meet = None;
            update_meet(&mut meet, self.meets.get(0));
            update_meet(&mut meet, batch_replay.meet());
            // if let Some(time) = self.meets.get(0) {
            //     meet = match meet {
            //         None => Some(self.meets[0].clone()),
            //         Some(x) => Some(x.meet(&self.meets[0])),
            //     };
            // }
            // if let Some(time) = batch_replay.meet() {
            //     meet = match meet {
            //         None => Some(time.clone()),
            //         Some(x) => Some(x.meet(&time)),
            //     };
            // }

            // Having determined the meet, we can load the input and output histories, where we
            // advance all times by joining them with `meet`. The resulting times are more compact
            // and guaranteed to accumulate identically for times greater or equal to `meet`.

            // Load the input and output histories.
            let mut input_replay = if let Some(meet) = meet.as_ref() {
                self.input_history.replay_key(source_cursor, source_storage, key, |time| {
                    let mut time = C1::owned_time(time);
                    time.join_assign(meet);
                    time
                })
            }
            else {
                self.input_history.replay_key(source_cursor, source_storage, key, |time| C1::owned_time(time))
            };
            let mut output_replay = if let Some(meet) = meet.as_ref() {
                self.output_history.replay_key(output_cursor, output_storage, key, |time| {
                    let mut time = C2::owned_time(time);
                    time.join_assign(meet);
                    time
                })
            }
            else {
                self.output_history.replay_key(output_cursor, output_storage, key, |time| C2::owned_time(time))
            };

            self.synth_times.clear();
            self.times_current.clear();
            self.output_produced.clear();

            // The frontier of times we may still consider.
            // Derived from frontiers of our update histories, supplied times, and synthetic times.

            let mut times_slice = &times[..];
            let mut meets_slice = &self.meets[..];

            let mut compute_counter = 0;
            let mut output_counter = 0;

            // We have candidate times from `batch` and `times`, as well as times identified by either
            // `input` or `output`. Finally, we may have synthetic times produced as the join of times
            // we consider in the course of evaluation. As long as any of these times exist, we need to
            // keep examining times.
            while let Some(next_time) = [   batch_replay.time(),
                                            times_slice.first(),
                                            input_replay.time(),
                                            output_replay.time(),
                                            self.synth_times.last(),
                                        ].iter().cloned().flatten().min().cloned() {

                // Advance input and output history replayers. This marks applicable updates as active.
                input_replay.step_while_time_is(&next_time);
                output_replay.step_while_time_is(&next_time);

                // One of our goals is to determine if `next_time` is "interesting", meaning whether we
                // have any evidence that we should re-evaluate the user logic at this time. For a time
                // to be "interesting" it would need to be the join of times that include either a time
                // from `batch`, `times`, or `synth`. Neither `input` nor `output` times are sufficient.

                // Advance batch history, and capture whether an update exists at `next_time`.
                let mut interesting = batch_replay.step_while_time_is(&next_time);
                if interesting {
                    if let Some(meet) = meet.as_ref() {
                        batch_replay.advance_buffer_by(meet);
                    }
                }

                // advance both `synth_times` and `times_slice`, marking this time interesting if in either.
                while self.synth_times.last() == Some(&next_time) {
                    // We don't know enough about `next_time` to avoid putting it in to `times_current`.
                    // TODO: If we knew that the time derived from a canceled batch update, we could remove the time.
                    self.times_current.push(self.synth_times.pop().expect("failed to pop from synth_times")); // <-- TODO: this could be a min-heap.
                    interesting = true;
                }
                while times_slice.first() == Some(&next_time) {
                    // We know nothing about why we were warned about `next_time`, and must include it to scare future times.
                    self.times_current.push(times_slice[0].clone());
                    times_slice = &times_slice[1..];
                    meets_slice = &meets_slice[1..];
                    interesting = true;
                }

                // Times could also be interesting if an interesting time is less than them, as they would join
                // and become the time itself. They may not equal the current time because whatever frontier we
                // are tracking may not have advanced far enough.
                // TODO: `batch_history` may or may not be super compact at this point, and so this check might
                //       yield false positives if not sufficiently compact. Maybe we should into this and see.
                interesting = interesting || batch_replay.buffer().iter().any(|&((_, ref t),_)| t.less_equal(&next_time));
                interesting = interesting || self.times_current.iter().any(|t| t.less_equal(&next_time));

                // We should only process times that are not in advance of `upper_limit`.
                //
                // We have no particular guarantee that known times will not be in advance of `upper_limit`.
                // We may have the guarantee that synthetic times will not be, as we test against the limit
                // before we add the time to `synth_times`.
                if !upper_limit.less_equal(&next_time) {

                    // We should re-evaluate the computation if this is an interesting time.
                    // If the time is uninteresting (and our logic is sound) it is not possible for there to be
                    // output produced. This sounds like a good test to have for debug builds!
                    if interesting {

                        compute_counter += 1;

                        // Assemble the input collection at `next_time`. (`self.input_buffer` cleared just after use).
                        debug_assert!(self.input_buffer.is_empty());
                        meet.as_ref().map(|meet| input_replay.advance_buffer_by(meet));
                        for &((value, ref time), ref diff) in input_replay.buffer().iter() {
                            if time.less_equal(&next_time) {
                                self.input_buffer.push((value, diff.clone()));
                            }
                            else {
                                self.temporary.push(next_time.join(time));
                            }
                        }
                        for &((value, ref time), ref diff) in batch_replay.buffer().iter() {
                            if time.less_equal(&next_time) {
                                self.input_buffer.push((value, diff.clone()));
                            }
                            else {
                                self.temporary.push(next_time.join(time));
                            }
                        }
                        crate::consolidation::consolidate(&mut self.input_buffer);

                        meet.as_ref().map(|meet| output_replay.advance_buffer_by(meet));
                        for &((value, ref time), ref diff) in output_replay.buffer().iter() {
                            if time.less_equal(&next_time) {
                                self.output_buffer.push((C2::owned_val(value), diff.clone()));
                            }
                            else {
                                self.temporary.push(next_time.join(time));
                            }
                        }
                        for &((ref value, ref time), ref diff) in self.output_produced.iter() {
                            if time.less_equal(&next_time) {
                                self.output_buffer.push(((*value).to_owned(), diff.clone()));
                            }
                            else {
                                self.temporary.push(next_time.join(time));
                            }
                        }
                        crate::consolidation::consolidate(&mut self.output_buffer);

                        // Apply user logic if non-empty input and see what happens!
                        if !self.input_buffer.is_empty() || !self.output_buffer.is_empty() {
                            logic(key, &self.input_buffer[..], &mut self.output_buffer, &mut self.update_buffer);
                            self.input_buffer.clear();
                            self.output_buffer.clear();
                        }

                        // output_replay.advance_buffer_by(&meet);
                        // for &((ref value, ref time), diff) in output_replay.buffer().iter() {
                        //     if time.less_equal(&next_time) {
                        //         self.output_buffer.push(((*value).clone(), -diff));
                        //     }
                        //     else {
                        //         self.temporary.push(next_time.join(time));
                        //     }
                        // }
                        // for &((ref value, ref time), diff) in self.output_produced.iter() {
                        //     if time.less_equal(&next_time) {
                        //         self.output_buffer.push(((*value).clone(), -diff));
                        //     }
                        //     else {
                        //         self.temporary.push(next_time.join(&time));
                        //     }
                        // }

                        // Having subtracted output updates from user output, consolidate the results to determine
                        // if there is anything worth reporting. Note: this also orders the results by value, so
                        // that could make the above merging plan even easier.
                        crate::consolidation::consolidate(&mut self.update_buffer);

                        // Stash produced updates into both capability-indexed buffers and `output_produced`.
                        // The two locations are important, in that we will compact `output_produced` as we move
                        // through times, but we cannot compact the output buffers because we need their actual
                        // times.
                        if !self.update_buffer.is_empty() {

                            output_counter += 1;

                            // We *should* be able to find a capability for `next_time`. Any thing else would
                            // indicate a logical error somewhere along the way; either we release a capability
                            // we should have kept, or we have computed the output incorrectly (or both!)
                            let idx = outputs.iter().rev().position(|(time, _)| time.less_equal(&next_time));
                            let idx = outputs.len() - idx.expect("failed to find index") - 1;
                            for (val, diff) in self.update_buffer.drain(..) {
                                self.output_produced.push(((val.clone(), next_time.clone()), diff.clone()));
                                outputs[idx].1.push((val, next_time.clone(), diff));
                            }

                            // Advance times in `self.output_produced` and consolidate the representation.
                            // NOTE: We only do this when we add records; it could be that there are situations
                            //       where we want to consolidate even without changes (because an initially
                            //       large collection can now be collapsed).
                            if let Some(meet) = meet.as_ref() {
                                for entry in &mut self.output_produced {
                                    (entry.0).1 = (entry.0).1.join(meet);
                                }
                            }
                            crate::consolidation::consolidate(&mut self.output_produced);
                        }
                    }

                    // Determine synthetic interesting times.
                    //
                    // Synthetic interesting times are produced differently for interesting and uninteresting
                    // times. An uninteresting time must join with an interesting time to become interesting,
                    // which means joins with `self.batch_history` and  `self.times_current`. I think we can
                    // skip `self.synth_times` as we haven't gotten to them yet, but we will and they will be
                    // joined against everything.

                    // Any time, even uninteresting times, must be joined with the current accumulation of
                    // batch times as well as the current accumulation of `times_current`.
                    for &((_, ref time), _) in batch_replay.buffer().iter() {
                        if !time.less_equal(&next_time) {
                            self.temporary.push(time.join(&next_time));
                        }
                    }
                    for time in self.times_current.iter() {
                        if !time.less_equal(&next_time) {
                            self.temporary.push(time.join(&next_time));
                        }
                    }

                    sort_dedup(&mut self.temporary);

                    // Introduce synthetic times, and re-organize if we add any.
                    let synth_len = self.synth_times.len();
                    for time in self.temporary.drain(..) {
                        // We can either service `join` now, or must delay for the future.
                        if upper_limit.less_equal(&time) {
                            debug_assert!(outputs.iter().any(|(t,_)| t.less_equal(&time)));
                            new_interesting.push(time);
                        }
                        else {
                            self.synth_times.push(time);
                        }
                    }
                    if self.synth_times.len() > synth_len {
                        self.synth_times.sort_by(|x,y| y.cmp(x));
                        self.synth_times.dedup();
                    }
                }
                else if interesting {
                    // We cannot process `next_time` now, and must delay it.
                    //
                    // I think we are probably only here because of an uninteresting time declared interesting,
                    // as initial interesting times are filtered to be in interval, and synthetic times are also
                    // filtered before introducing them to `self.synth_times`.
                    new_interesting.push(next_time.clone());
                    debug_assert!(outputs.iter().any(|(t,_)| t.less_equal(&next_time)))
                }


                // Update `meet` to track the meet of each source of times.
                meet = None;//T::maximum();
                update_meet(&mut meet, batch_replay.meet());
                update_meet(&mut meet, input_replay.meet());
                update_meet(&mut meet, output_replay.meet());
                for time in self.synth_times.iter() { update_meet(&mut meet, Some(time)); }
                // if let Some(time) = batch_replay.meet() { meet = meet.meet(time); }
                // if let Some(time) = input_replay.meet() { meet = meet.meet(time); }
                // if let Some(time) = output_replay.meet() { meet = meet.meet(time); }
                // for time in self.synth_times.iter() { meet = meet.meet(time); }
                update_meet(&mut meet, meets_slice.first());
                // if let Some(time) = meets_slice.first() { meet = meet.meet(time); }

                // Update `times_current` by the frontier.
                if let Some(meet) = meet.as_ref() {
                    for time in self.times_current.iter_mut() {
                        *time = time.join(meet);
                    }
                }

                sort_dedup(&mut self.times_current);
            }

            // Normalize the representation of `new_interesting`, deduplicating and ordering.
            sort_dedup(new_interesting);

            (compute_counter, output_counter)
        }
    }

    /// Updates an optional meet by an optional time.
    fn update_meet<T: Lattice+Clone>(meet: &mut Option<T>, other: Option<&T>) {
        if let Some(time) = other {
            if let Some(meet) = meet.as_mut() {
                *meet = meet.meet(time);
            }
            if meet.is_none() {
                *meet = Some(time.clone());
            }
        }
    }
}
