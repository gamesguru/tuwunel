#[cfg(test)]
mod tests;

mod auth_difference;
mod conflicted_subgraph;
mod iterative_auth_check;
mod mainline_sort;
mod power_sort;
mod split_conflicted;

use std::{
	collections::{BTreeMap, BTreeSet, HashMap, HashSet},
	ops::Deref,
};

use futures::{FutureExt, Stream, StreamExt};
use ruma::{
	OwnedEventId, events::room::power_levels::UserPowerLevel,
	room_version_rules::RoomVersionRules,
};
use tuwunel_core::{
	Result, debug,
	itertools::Itertools,
	matrix::{Event, TypeStateKey},
	smallvec::SmallVec,
	trace,
	utils::{
		BoolExt,
		stream::{BroadbandExt, IterStream},
	},
};

use self::{
	auth_difference::auth_difference, conflicted_subgraph::conflicted_subgraph_dfs,
	power_sort::power_level_for_sender, split_conflicted::split_conflicted_state,
};
#[cfg(test)]
use self::{
	iterative_auth_check::iterative_auth_check, mainline_sort::mainline_sort,
	power_sort::power_sort,
};
#[cfg(test)]
use super::test_utils;

/// A mapping of event type and state_key to some value `T`, usually an
/// `EventId`.
pub type StateMap<Id> = BTreeMap<TypeStateKey, Id>;

/// Full recursive set of `auth_events` for each event in a StateMap.
pub type AuthSet<Id> = BTreeSet<Id>;

/// ConflictMap of OwnedEventId specifically.
pub type ConflictMap<Id> = StateMap<ConflictVec<Id>>;

/// List of conflicting event_ids
type ConflictVec<Id> = SmallVec<[Id; 2]>;

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
/// * `auth_chains` - The list of full recursive sets of `auth_events` for each
///   event in the `state_maps`.
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

	let conflicted_ids: HashSet<OwnedEventId> = conflicted_states
		.values()
		.flatten()
		.cloned()
		.collect();

	let mut all_ids_to_fetch = full_conflicted_set.clone();
	for id in unconflicted_state.values() {
		all_ids_to_fetch.insert(id.clone());
	}

	let mut fetch_futures = futures::stream::FuturesUnordered::new();
	for id in all_ids_to_fetch {
		let id_clone = id.clone();
		fetch_futures.push(async move {
			let pdu_res = fetch(id_clone.clone()).await;
			let pl_res = power_level_for_sender::<_, _, Pdu>(&id_clone, rules, fetch).await;
			(id_clone, pdu_res, pl_res)
		});
	}

	while let Some((id, pdu_res, pl_res)) = fetch_futures.next().await {
		if let Ok(pdu) = pdu_res {
			let sender_power = match pl_res {
				| Ok(UserPowerLevel::Infinite) => i64::MAX,
				| Ok(UserPowerLevel::Int(x)) => i64::from(x),
				| _ => 0,
			};

			let lean = rezzy::LeanEvent {
				event_id: pdu.event_id().to_owned(),
				event_type: pdu.kind().to_string(),
				state_key: pdu.state_key().map(|sk| sk.to_string()),
				power_level: sender_power,
				origin_server_ts: pdu.origin_server_ts().get().into(),
				sender: pdu.sender().to_string(),
				content: pdu.get_content_as_value(),
				prev_events: pdu.prev_events().map(|e| e.to_owned()).collect(),
				auth_events: pdu.auth_events().map(|e| e.to_owned()).collect(),
				depth: pdu.as_pdu().depth.into(),
			};

			if conflicted_ids.contains(&id) {
				conflicted_events.insert(id, lean);
			} else {
				auth_context.insert(id, lean);
			}
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
	let mut unconflicted_shared = imbl::OrdMap::new();
	for (key, id) in &unconflicted_state {
		unconflicted_shared.insert((key.0.to_string(), key.1.to_string()), id.clone());
	}

	// Perform state resolution using rezzy
	let resolved = rezzy::resolve_iterative_sort(
		unconflicted_shared,
		conflicted_events,
		&auth_context,
		version,
	);

	// Convert back into tuwunel's StateMap format
	let mut final_state = BTreeMap::new();
	for (key, id) in resolved {
		final_state.insert((key.0.into(), key.1.into()), id);
	}

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
) -> HashSet<OwnedEventId>
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
		.collect::<HashSet<_>>()
		.inspect(|set| debug!(count = set.len(), "full conflicted set"))
		.inspect(|set| trace!(?set, "full conflicted set"))
		.await
}
