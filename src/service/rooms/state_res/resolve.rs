#[cfg(test)]
mod tests;

mod auth_difference;
mod conflicted_subgraph;
mod power_sort;
mod split_conflicted;

use std::{
	collections::{BTreeMap, HashMap, HashSet},
	ops::Deref,
	vec::IntoIter,
};

use futures::{FutureExt, Stream, StreamExt};
use ruma::{
	OwnedEventId, events::room::power_levels::UserPowerLevel,
	room_version_rules::RoomVersionRules,
};
use tuwunel_core::{
	Result, debug,
	itertools::Itertools,
	matrix::{Event, TypeStateKey, event_id::RandomState},
	smallvec::SmallVec,
	trace,
	utils::{
		BoolExt,
		stream::{BroadbandExt, IterStream},
	},
};

use self::{
	auth_difference::auth_difference, conflicted_subgraph::conflicted_subgraph_dfs,
	power_sort::power_level_for_pdu_sender, split_conflicted::split_conflicted_state,
};
#[cfg(test)]
use super::test_utils;

/// A mapping of event type and state_key to some value `T`, usually an
/// `EventId`.
pub type StateMap<Id> = BTreeMap<TypeStateKey, Id>;

/// Full recursive auth chain for one candidate [`StateMap`].
///
/// Values are distinct and immutable after construction. Their order is
/// arbitrary, and consumers must not depend on it.
#[derive(Clone)]
pub struct AuthSet<Id>(Vec<Id>);

/// Conflicting event ids for each contested state key.
pub type ConflictMap<Id> = StateMap<ConflictVec<Id>>;

/// Event ids contesting one state key.
///
/// Two forks disputing a key is the modal conflict, so two ids stay inline.
type ConflictVec<Id> = SmallVec<[Id; 2]>;

/// The full conflicted set (arbitrary order).
type ConflictedSet = HashSet<OwnedEventId, RandomState>;

impl<Id> AuthSet<Id> {
	/// Creates an auth set from distinct identifiers.
	///
	/// The caller must ensure `ids` contains no duplicates. Duplicates are
	/// not checked, so hot paths avoid redundant work.
	#[inline]
	#[must_use]
	pub(crate) fn from_distinct(ids: Vec<Id>) -> Self { Self(ids) }
}

impl<Id> Default for AuthSet<Id> {
	fn default() -> Self { Self(Vec::new()) }
}

impl<Id: Ord> FromIterator<Id> for AuthSet<Id> {
	fn from_iter<I: IntoIterator<Item = Id>>(iter: I) -> Self {
		Self::from_distinct(
			iter.into_iter()
				.sorted_unstable()
				.dedup()
				.collect(),
		)
	}
}

impl<Id> IntoIterator for AuthSet<Id> {
	type IntoIter = IntoIter<Id>;
	type Item = Id;

	fn into_iter(self) -> Self::IntoIter { self.0.into_iter() }
}

/// Apply the [state resolution] algorithm introduced in room version 2 to
/// resolve the state of a room.
///
/// ## Arguments
///
/// * `rules` - The rules to apply for the version of the current room.
///
/// * `state_maps` - The incoming states to resolve. Each `StateMap` represents
///   a possible fork in the state of a room.
///
/// * `auth_sets` - The list of full recursive sets of `auth_events` for each
///   event in the `state_maps`. Inputs must not contain duplicates.
///
/// * `fetch_event` - Function to fetch an event in the room given its event ID.
///
/// ## Invariants
///
/// The caller of `resolve` must ensure that all the events are from the same
/// room.
///
/// ## Returns
///
/// The resolved room state.
///
/// [state resolution]: https://spec.matrix.org/latest/rooms/v2/#state-resolution
#[tracing::instrument(level = "debug", skip_all)]
pub async fn resolve<States, AuthSets, FetchExists, ExistsFut, FetchEvent, EventFut, Pdu>(
	rules: &RoomVersionRules,
	state_maps: States,
	auth_sets: AuthSets,
	fetch: &FetchEvent,
	exists: &FetchExists,
	hydra_backports: bool,
) -> Result<StateMap<OwnedEventId>>
where
	States: Stream<Item = StateMap<OwnedEventId>> + Send,
	AuthSets: Stream<Item = AuthSet<OwnedEventId>> + Send,
	FetchExists: Fn(OwnedEventId) -> ExistsFut + Sync,
	ExistsFut: Future<Output = bool> + Send,
	FetchEvent: Fn(OwnedEventId) -> EventFut + Sync,
	EventFut: Future<Output = Result<Pdu>> + Send,
	Pdu: Event + Clone,
{
	// Split the unconflicted state map and the conflicted state set.
	let (unconflicted_state, conflicted_states) = split_conflicted_state(state_maps).await;

	debug!(
		unconflicted = unconflicted_state.len(),
		conflicted_states = conflicted_states.len(),
		conflicted_events = conflicted_states
			.values()
			.fold(0_usize, |a, s| a.saturating_add(s.len())),
		"unresolved states"
	);

	if conflicted_states.is_empty() {
		return Ok(unconflicted_state.into_iter().collect());
	}

	// 0. The full conflicted set is the union of the conflicted state set and the
	//    auth difference. Don't honor events that don't exist.
	let full_conflicted_set = full_conflicted_set::<_, _, _, _, _, Pdu>(
		rules,
		conflicted_states.clone(),
		auth_sets,
		fetch,
		exists,
		hydra_backports,
	)
	.await;

	// Use FuturesUnordered to fetch all required PDUs and their sender's power
	// level in parallel.
	let mut conflicted_events = HashMap::new();
	let mut auth_context = HashMap::new();

	let mut all_ids_to_fetch = full_conflicted_set.clone();
	for id in unconflicted_state.values() {
		all_ids_to_fetch.insert(id.clone());
	}

	let mut fetch_futures = futures::stream::FuturesUnordered::new();
	for id in all_ids_to_fetch {
		let id_clone = id.clone();
		let is_conflicted = full_conflicted_set.contains(&id_clone);
		fetch_futures.push(async move {
			let pdu_res = fetch(id_clone.clone()).await;
			match pdu_res {
				| Ok(pdu) =>
					if is_conflicted {
						let pl_res =
							power_level_for_pdu_sender::<_, _, Pdu>(&pdu, rules, fetch).await;
						(id_clone, Ok(pdu), Some(pl_res))
					} else {
						(id_clone, Ok(pdu), None)
					},
				| Err(e) => (id_clone, Err(e), None),
			}
		});
	}

	while let Some((id, pdu_res, pl_res)) = fetch_futures.next().await {
		let pdu = pdu_res?;

		let sender_power = match pl_res {
			| Some(Ok(UserPowerLevel::Infinite)) => i64::MAX,
			| Some(Ok(UserPowerLevel::Int(x))) => i64::from(x),
			| Some(Err(e)) => return Err(e),
			| _ => 0,
		};

		let lean = rezzy::LeanEvent {
			event_id: pdu.event_id().to_owned(),
			event_type: pdu.kind().to_string(),
			state_key: pdu.state_key().map(ToOwned::to_owned),
			power_level: sender_power,
			origin_server_ts: pdu.origin_server_ts().get().into(),
			sender: pdu.sender().to_string(),
			content: pdu.get_content_as_value(),
			prev_events: pdu.prev_events().map(ToOwned::to_owned).collect(),
			auth_events: pdu.auth_events().map(ToOwned::to_owned).collect(),
			depth: pdu.as_pdu().depth.into(),
			rejected: pdu.rejected(),
			soft_fail: false,
		};

		if full_conflicted_set.contains(&id) {
			conflicted_events.insert(id, lean);
		} else {
			auth_context.insert(id, lean);
		}
	}

	// Map RoomVersionRules / hydra_backports to rezzy::StateResVersion
	let version = if rules
		.state_res
		.v2_rules()
		.is_some_and(|r| r.begin_iterative_auth_checks_with_empty_state_map)
		|| hydra_backports
	{
		rezzy::StateResVersion::V2_1
	} else if rules.state_res.v2_rules().is_none() {
		rezzy::StateResVersion::V1
	} else {
		rezzy::StateResVersion::V2
	};

	// Convert unconflicted_state BTreeMap into the imbl::OrdMap format expected by
	// rezzy
	let mut unconflicted_shared = rezzy::SharedState::new();
	for (key, id) in &unconflicted_state {
		unconflicted_shared.insert((key.0.to_string().into(), key.1.to_string()), id.clone());
	}

	// Perform state resolution using rezzy
	let resolved = rezzy::resolve_iterative_sort(
		unconflicted_shared,
		conflicted_events,
		&auth_context,
		version,
		&mut HashMap::new(),
	);

	// Convert back into tuwunel's StateMap format
	let mut final_state = BTreeMap::new();
	for (key, id) in resolved {
		final_state
			.insert((ruma::events::StateEventType::from(key.0.to_string()), key.1.into()), id);
	}

	final_state.extend(unconflicted_state);

	debug!(resolved_state = final_state.len(), "resolved state");
	trace!(?final_state, "resolved state");

	Ok(final_state)
}

#[tracing::instrument(
	name = "conflicted",
	level = "debug",
	skip_all,
	fields(
		states = conflicted_states.len(),
		events = conflicted_states.values().flatten().count()
	),
)]
async fn full_conflicted_set<AuthSets, FetchExists, ExistsFut, FetchEvent, EventFut, Pdu>(
	rules: &RoomVersionRules,
	conflicted_states: ConflictMap<OwnedEventId>,
	auth_sets: AuthSets,
	fetch: &FetchEvent,
	exists: &FetchExists,
	hydra_backports: bool,
) -> ConflictedSet
where
	AuthSets: Stream<Item = AuthSet<OwnedEventId>> + Send,
	FetchExists: Fn(OwnedEventId) -> ExistsFut + Sync,
	ExistsFut: Future<Output = bool> + Send,
	FetchEvent: Fn(OwnedEventId) -> EventFut + Sync,
	EventFut: Future<Output = Result<Pdu>> + Send,
	Pdu: Event,
{
	let consider_conflicted_subgraph = rules
		.state_res
		.v2_rules()
		.is_some_and(|rules| rules.consider_conflicted_state_subgraph)
		|| hydra_backports;

	let conflicted_state_set: Vec<_> = conflicted_states
		.values()
		.flatten()
		.sorted_unstable()
		.dedup()
		.collect();

	// Since `org.matrix.hydra.11`, fetch the conflicted state subgraph.
	let conflicted_subgraph = consider_conflicted_subgraph
		.then_async(async || conflicted_subgraph_dfs(&conflicted_state_set, fetch))
		.map(Option::into_iter)
		.map(IterStream::stream)
		.flatten_stream()
		.flatten()
		.boxed();

	let conflicted_state_ids = conflicted_state_set
		.iter()
		.map(Deref::deref)
		.cloned()
		.stream();

	auth_difference(auth_sets)
		.chain(conflicted_state_ids)
		.broad_filter_map(async |id| exists(id.clone()).await.then_some(id))
		.chain(conflicted_subgraph)
		.collect::<ConflictedSet>()
		.inspect(|set| debug!(count = set.len(), "full conflicted set"))
		.inspect(|set| trace!(?set, "full conflicted set"))
		.await
}
