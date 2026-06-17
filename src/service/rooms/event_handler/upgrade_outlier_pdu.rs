use std::{borrow::Borrow, collections::HashMap, iter::once, sync::Arc, time::Instant};

use futures::{FutureExt, StreamExt};
use ruma::{
	CanonicalJsonObject, EventId, OwnedEventId, RoomId, RoomVersionId, ServerName,
	events::StateEventType, room_version_rules::RoomVersionRules,
};
use tuwunel_core::{
	Err, Result, debug, debug_info, err, implement, is_equal_to,
	matrix::{Event, EventTypeExt, PduEvent, StateKey, pdu::check_rules, room_version},
	trace,
	utils::stream::{BroadbandExt, ReadyExt},
	warn,
};

use super::policy_server::PolicyCheck;
use crate::rooms::{
	state::RoomMutexGuard,
	state_compressor::{CompressedState, HashSetCompressStateEvent},
	state_res,
	timeline::RawPduId,
};

#[implement(super::Service)]
#[tracing::instrument(
	name = "upgrade",
	level = "debug",
	ret(level = "debug"),
	skip_all,
	fields(lev = %recursion_level)
)]
#[expect(clippy::too_many_arguments)]
pub(super) async fn upgrade_outlier_to_timeline_pdu(
	&self,
	origin: &ServerName,
	room_id: &RoomId,
	incoming_pdu: PduEvent,
	mut pdu_json: CanonicalJsonObject,
	room_version: &RoomVersionId,
	recursion_level: usize,
	create_event_id: &EventId,
) -> Result<Option<(RawPduId, bool)>> {
	// Skip the PDU if we already have it as a timeline event
	if let Ok(pdu_id) = self
		.services
		.timeline
		.get_pdu_id(incoming_pdu.event_id())
		.await
	{
		debug!(?pdu_id, "Exists.");
		return Ok(Some((pdu_id, false)));
	}

	if self
		.services
		.pdu_metadata
		.is_event_soft_failed(incoming_pdu.event_id())
		.await
	{
		return Err!(Request(InvalidParam("Event has been soft failed")));
	}

	trace!("Upgrading to timeline pdu");

	let timer = Instant::now();
	let room_rules = room_version::rules(room_version)?;

	trace!(format = ?room_rules.event_format, "Checking format");
	check_rules(&pdu_json, &room_rules.event_format)?;

	let (state_at_incoming_event, via_fetch) = self
		.resolve_state_at_incoming_event(
			origin,
			room_id,
			&incoming_pdu,
			room_version,
			recursion_level,
			create_event_id,
		)
		.await?;

	self.auth_check_outlier_pdu(room_id, &incoming_pdu, &room_rules, &state_at_incoming_event)
		.await?;

	let soft_fail = self
		.compute_soft_fail(&incoming_pdu, &room_rules, &mut pdu_json)
		.await?;

	// 13. Use state resolution to find new room state
	// We start looking at current room state now, so lets lock the room
	trace!("Locking the room");
	let state_lock = self.services.state.mutex.lock(room_id).await;

	let extremities = self
		.compute_remaining_extremities(room_id, &incoming_pdu)
		.await;

	trace!("Compressing state...");
	let state_ids_compressed: Arc<CompressedState> = self
		.services
		.state_compressor
		.compress_state_events(
			state_at_incoming_event
				.iter()
				.map(|(ssk, eid)| (ssk, eid.borrow())),
		)
		.collect()
		.map(Arc::new)
		.await;

	if via_fetch && !soft_fail {
		self.cache_resolved_state(room_id, incoming_pdu.event_id(), state_ids_compressed.clone())
			.await;
	}

	if incoming_pdu.state_key().is_some() {
		self.resolve_and_force_state_after(
			room_id,
			room_version,
			&incoming_pdu,
			&state_at_incoming_event,
			&state_lock,
		)
		.boxed()
		.await?;
	}

	// 14. Check if the event passes auth based on the "current state" of the room,
	//     if not soft fail it
	//
	// Now that the event has passed all auth it is added into the timeline.
	// We use the `state_at_event` instead of `state_after` so we accurately
	// represent the state for this event.
	trace!("Appending pdu to timeline");

	// Incoming event will be referenced in prev_events unless soft-failed.
	let incoming_extremity = once(incoming_pdu.event_id()).filter(|_| !soft_fail);

	let extremities = extremities
		.iter()
		.map(Borrow::borrow)
		.chain(incoming_extremity);

	let pdu_id = self
		.services
		.timeline
		.append_incoming_pdu(
			&incoming_pdu,
			pdu_json,
			extremities,
			state_ids_compressed,
			soft_fail,
			&state_lock,
		)
		.await?;

	debug_assert!(
		pdu_id.is_some() || soft_fail,
		"Ok(None) returned by timeline for soft-failed PDU's"
	);

	if soft_fail {
		self.services
			.pdu_metadata
			.mark_event_soft_failed(incoming_pdu.event_id());

		drop(state_lock);
		warn!(
			elapsed = ?timer.elapsed(),
			"Event was soft failed: {:?}",
			incoming_pdu.event_id()
		);

		return Err!(Request(InvalidParam("Event has been soft failed")));
	}

	drop(state_lock);
	debug_info!(
		elapsed = ?timer.elapsed(),
		"Accepted",
	);

	Ok(pdu_id.zip(Some(true)))
}

#[implement(super::Service)]
async fn resolve_state_at_incoming_event(
	&self,
	origin: &ServerName,
	room_id: &RoomId,
	incoming_pdu: &PduEvent,
	room_version: &RoomVersionId,
	recursion_level: usize,
	create_event_id: &EventId,
) -> Result<(HashMap<u64, OwnedEventId>, bool)> {
	// 10. Fetch missing state and auth chain events by calling /state_ids at
	//     backwards extremities doing all the checks in this list starting at 1.
	//     These are not timeline events.
	trace!("Resolving state at event");

	let mut state_at_incoming_event = if incoming_pdu.prev_events().count() == 1 {
		self.state_at_incoming_degree_one(incoming_pdu)
			.await?
	} else {
		self.state_at_incoming_resolved(incoming_pdu, room_id, room_version)
			.boxed()
			.await?
	};

	if state_at_incoming_event.is_none() {
		state_at_incoming_event = self
			.cached_resolved_state(incoming_pdu.event_id())
			.await;
	}

	let (state_at_incoming_event, via_fetch) = match state_at_incoming_event {
		| Some(state) => (state, false),
		| None => {
			let state = self
				.fetch_state(
					origin,
					room_id,
					incoming_pdu.event_id(),
					room_version,
					recursion_level,
					create_event_id,
				)
				.boxed()
				.await?
				.expect("fetch_state always resolves state to some");

			(state, true)
		},
	};

	Ok((state_at_incoming_event, via_fetch))
}

#[implement(super::Service)]
async fn auth_check_outlier_pdu(
	&self,
	room_id: &RoomId,
	incoming_pdu: &PduEvent,
	room_rules: &RoomVersionRules,
	state_at_incoming_event: &HashMap<u64, OwnedEventId>,
) -> Result {
	// 11. Check the auth of the event passes based on the state of the event

	let state_fetch = async |k: StateEventType, s: StateKey| {
		let shortstatekey = self
			.services
			.short
			.get_shortstatekey(&k, s.as_str())
			.await?;

		let event_id = state_at_incoming_event
			.get(&shortstatekey)
			.ok_or_else(|| {
				err!(Request(NotFound(
					"shortstatekey {shortstatekey:?} not found for ({k:?},{s:?})"
				)))
			})?;

		self.services.timeline.get_pdu(event_id).await
	};

	let event_fetch = async |event_id: OwnedEventId| self.event_fetch(&event_id).await;

	trace!("Performing auth check");
	state_res::auth_check(room_rules, incoming_pdu, &event_fetch, &state_fetch).await?;

	trace!("Gathering auth events");
	let auth_events = self
		.services
		.state
		.get_auth_events(
			room_id,
			incoming_pdu.kind(),
			incoming_pdu.sender(),
			incoming_pdu.state_key(),
			incoming_pdu.content(),
			&room_rules.authorization,
			true,
		)
		.await?;

	let state_fetch = async |k: StateEventType, s: StateKey| {
		auth_events
			.get(&k.with_state_key(s.as_str()))
			.map(ToOwned::to_owned)
			.ok_or_else(|| err!(Request(NotFound("state event not found"))))
	};

	trace!("Performing auth check");
	state_res::auth_check(room_rules, incoming_pdu, &event_fetch, &state_fetch).await?;

	Ok(())
}

#[implement(super::Service)]
async fn compute_soft_fail(
	&self,
	incoming_pdu: &PduEvent,
	room_rules: &RoomVersionRules,
	pdu_json: &mut CanonicalJsonObject,
) -> Result<bool> {
	// Soft fail check before doing state res
	trace!("Performing soft-fail check");
	let soft_fail_redact = match incoming_pdu.redacts_id(room_rules) {
		| None => false,
		| Some(redact_id) =>
			!self
				.services
				.state_accessor
				.user_can_redact(&redact_id, incoming_pdu.sender(), incoming_pdu.room_id(), true)
				.await?,
	};

	// MSC4284: soft-fail when the policy server rejects the event.
	Ok(soft_fail_redact
		|| matches!(
			self.verify_or_fetch_inbound_policy_signature(pdu_json, incoming_pdu)
				.await,
			PolicyCheck::Invalid,
		))
}

#[implement(super::Service)]
async fn compute_remaining_extremities(
	&self,
	room_id: &RoomId,
	incoming_pdu: &PduEvent,
) -> Vec<OwnedEventId> {
	// Now we calculate the set of extremities this room has after the incoming
	// event has been applied. We start with the previous extremities (aka leaves)
	trace!("Calculating extremities");
	let extremities: Vec<_> = self
		.services
		.state
		.get_forward_extremities(room_id)
		.map(ToOwned::to_owned)
		.ready_filter(|event_id| {
			// Remove any that are referenced by this incoming event's prev_events
			!incoming_pdu
				.prev_events()
				.any(is_equal_to!(event_id))
		})
		.broad_filter_map(async |event_id| {
			// Only keep those extremities were not referenced yet
			self.services
				.pdu_metadata
				.is_event_referenced(room_id, &event_id)
				.await
				.eq(&false)
				.then_some(event_id)
		})
		.collect()
		.await;

	debug!(
		retained = extremities.len(),
		prev_events = incoming_pdu.prev_events().count(),
		"Retained extremities checked against prev_events.",
	);

	extremities
}

#[implement(super::Service)]
async fn resolve_and_force_state_after(
	&self,
	room_id: &RoomId,
	room_version: &RoomVersionId,
	incoming_pdu: &PduEvent,
	state_at_incoming_event: &HashMap<u64, OwnedEventId>,
	state_lock: &RoomMutexGuard,
) -> Result {
	// We also add state after incoming event to the fork states
	let mut state_after = state_at_incoming_event.clone();
	if let Some(state_key) = incoming_pdu.state_key() {
		let event_id = incoming_pdu.event_id();
		let event_type = incoming_pdu.kind();
		let shortstatekey = self
			.services
			.short
			.get_or_create_shortstatekey(&event_type.to_string().into(), state_key)
			.await;

		state_after.insert(shortstatekey, event_id.to_owned());
		// Now it's the state after the event.
		debug!(
			?event_id,
			?event_type,
			?state_key,
			?shortstatekey,
			state_after = state_after.len(),
			"Adding event to state."
		);
	}

	trace!("Resolving new room state.");
	let new_room_state = self
		.resolve_state(room_id, room_version, state_after)
		.boxed()
		.await?;

	// Set the new room state to the resolved state
	trace!("Saving resolved state.");
	let HashSetCompressStateEvent { shortstatehash, added, removed } = self
		.services
		.state_compressor
		.save_state(room_id, new_room_state)
		.await?;

	debug!(
		?shortstatehash,
		added = added.len(),
		removed = removed.len(),
		"Forcing new room state."
	);
	self.services
		.state
		.force_state(room_id, shortstatehash, added, removed, state_lock)
		.await?;

	Ok(())
}
