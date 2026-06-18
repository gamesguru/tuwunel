//! Single-flight bookkeeping for one in-flight fetch.
//!
//! [`Key`] is the dedup key the worker's in-flight map is keyed on;
//! [`Inflight`] is the worker-owned entry every coalesced caller subscribes to;
//! [`SharedResult`] is the broadcast outcome and [`Subscription`] the caller's
//! handle, whose liveness token cancels the fetch on drop.

use std::{
	collections::hash_map::DefaultHasher,
	hash::{Hash, Hasher},
	sync::{Arc, Weak},
};

use ruma::{MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedRoomId, api::Direction};
use tokio::sync::watch::{Receiver, Sender};
use tuwunel_core::smallvec::SmallVec;

use super::{Failure, Op, Opts, Outcome};

/// Borrowed window ids gathered for sorting before hashing; inline-sized for
/// the common single-prev case.
type WindowRefs<'a> = SmallVec<[&'a OwnedEventId; 1]>;

/// Single-flight dedup key. `MissingEvents` keys on a content hash of its
/// request window instead of a single event_id, so two callers asking for the
/// same window coalesce regardless of event order; see [`window_hash`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Key {
	/// Endpoint class; two ops over the same event do not coalesce.
	pub(super) op: Op,

	/// Room the event belongs to, or `None` for an unscoped fetch.
	pub(super) room_id: Option<OwnedRoomId>,

	/// Sought event, or `None` for ops that do not key on one.
	pub(super) event_id: Option<OwnedEventId>,

	/// Content hash of the [`Op::MissingEvents`] window; `None` for every other
	/// op, so their coalescing is byte-identical to before.
	pub(super) window_hash: Option<u64>,

	/// [`Op::TimestampToEvent`] search timestamp; `None` for every other op, so
	/// distinct-timestamp queries for one room do not coalesce.
	pub(super) ts: Option<MilliSecondsSinceUnixEpoch>,

	/// [`Op::TimestampToEvent`] search direction; `None` for every other op, so
	/// opposite-direction queries for one room do not coalesce.
	pub(super) dir: Option<Direction>,
}

impl Hash for Key {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.op.hash(state);
		self.room_id.hash(state);
		self.event_id.hash(state);
		self.window_hash.hash(state);
		self.ts.hash(state);
		self.dir
			.map(|dir| matches!(dir, Direction::Forward))
			.hash(state);
	}
}

/// Outcome shared by every caller coalesced onto one fetch. Cheap to clone so
/// the worker can broadcast it down each subscriber's channel.
pub(super) type SharedResult = Result<Arc<Outcome>, Failure>;

/// Reply handed to a caller: the channel it awaits the outcome on, plus the
/// sole strong liveness token whose drop cancels the in-flight fetch.
pub(super) type Subscription = (Receiver<Option<SharedResult>>, Arc<()>);

/// One in-flight fetch, owned by the worker. The worker is the sole mutator, so
/// no lock guards it; coalesced callers reach it only through their channels.
pub(super) struct Inflight {
	/// Result channel. Coalesced callers subscribe to await the outcome.
	pub(super) tx: Sender<Option<SharedResult>>,

	/// Liveness signal. The strong token rides to the callers; the worker holds
	/// this weak ref and the fetch bails once it can no longer upgrade it.
	pub(super) interest: Weak<()>,

	/// Retained (shared) so a re-armed key re-dispatches without re-cloning it.
	pub(super) opts: Arc<Opts>,
}

impl Key {
	/// Derive the single-flight key from a request's [`Opts`].
	pub(super) fn new(opts: &Opts) -> Self {
		Self {
			op: opts.op,
			room_id: opts.room_id.clone(),
			event_id: opts.event_id.clone(),
			window_hash: matches!(opts.op, Op::MissingEvents).then(|| window_hash(opts)),
			ts: opts.ts,
			dir: opts.dir,
		}
	}
}

/// Order-independent content hash of an [`Op::MissingEvents`] window: the
/// sorted `latest_events` and `earliest_events` sets plus the batch limit.
/// Sorting before hashing folds request-order permutations onto one key; the
/// two sets hash as distinct sequences so swapping them does not collide.
fn window_hash(opts: &Opts) -> u64 {
	let mut latest: WindowRefs<'_> = opts.latest_events.iter().collect();
	let mut earliest: WindowRefs<'_> = opts.earliest_events.iter().collect();
	latest.sort_unstable();
	earliest.sort_unstable();

	let mut hasher = DefaultHasher::new();
	latest.hash(&mut hasher);
	earliest.hash(&mut hasher);
	opts.backfill_limit.hash(&mut hasher);

	hasher.finish()
}
