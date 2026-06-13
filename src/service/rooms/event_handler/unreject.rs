use futures::{StreamExt, TryFutureExt, future::BoxFuture};
use ruma::{
	CanonicalJsonObject, CanonicalJsonValue, OwnedEventId, RoomId, RoomVersionId, ServerName,
};
use tuwunel_core::{
	Event, EventTypeExt, Result, debug, err, implement,
	matrix::{PduEvent, room_version},
	ref_at,
	utils::{
		future::TryExtExt,
		stream::{IterStream, ReadyExt},
	},
	warn,
};

use crate::rooms::state_res;

#[implement(super::Service)]
#[must_use]
pub fn unreject_rejected_events<'a>(
	&'a self,
	origin: &'a ServerName,
	room_id: &'a RoomId,
	room_version: &'a RoomVersionId,
) -> BoxFuture<'a, Result> {
	Box::pin(async move {
		debug!("Starting unreject scanning for room {room_id}");

		// Get all outliers in eventid_outlierpdu
		let outliers: Vec<_> = self
			.services
			.timeline
			.db
			.eventid_outlierpdu
			.stream::<OwnedEventId, CanonicalJsonObject>()
			.ready_filter_map(Result::ok)
			.collect()
			.await;

		// Filter for outliers that are in this room and have "rejected" == true
		let mut rejected_outliers = Vec::new();
		for item in outliers {
			let pdu_room_id = item
				.1
				.get("room_id")
				.and_then(CanonicalJsonValue::as_str);
			if pdu_room_id == Some(room_id.as_str()) {
				let is_rejected = item
					.1
					.get("rejected")
					.and_then(CanonicalJsonValue::as_bool)
					.unwrap_or(false);
				if is_rejected {
					rejected_outliers.push(item);
				}
			}
		}

		debug!(count = rejected_outliers.len(), "Found rejected outliers in room");

		// For each rejected outlier, re-run auth check
		let room_rules = room_version::rules(room_version)?;
		let mut promoted_any = false;

		for item in rejected_outliers {
			let event_id = &item.0;
			let mut pdu_json = item.1;

			// Convert to PduEvent
			let Ok((event, _)) = PduEvent::from_object_federation(
				room_id,
				event_id,
				pdu_json.clone(),
				&room_rules,
			) else {
				continue;
			};

			// Fetch its auth events (some of which might have just arrived!)
			let auth_events: Vec<_> = event
				.auth_events()
				.stream()
				.filter_map(|auth_event_id| {
					self.event_fetch(auth_event_id)
						.inspect_err(move |e| warn!("Missing auth_event {auth_event_id}: {e}"))
						.ok()
				})
				.map(|auth_event| {
					let event_type = auth_event.event_type();
					let state_key = auth_event
						.state_key()
						.expect("all auth events have state_key");

					(event_type.with_state_key(state_key), auth_event)
				})
				.collect()
				.await;

			// Re-run state_res::auth_check
			let auth_pass = state_res::auth_check(
				&room_rules,
				&event,
				&async |event_id| self.event_fetch(&event_id).await,
				&async |event_type, state_key| {
					let target = event_type.with_state_key(state_key);
					auth_events
						.iter()
						.find(|(type_state_key, _)| *type_state_key == target)
						.map(ref_at!(1))
						.cloned()
						.ok_or_else(|| err!(Request(NotFound("state not found"))))
				},
			)
			.await
			.is_ok();

			if auth_pass {
				debug!("Successfully unrejected event {event_id}!");
				// Remove the "rejected" flag
				pdu_json.remove("rejected");

				// Clear the rejected status by adding it as a normal outlier
				self.services
					.timeline
					.add_pdu_outlier(event_id, &pdu_json);

				// Promote to timeline or handle incoming PDU!
				let _: Result<_> = Box::pin(self.handle_incoming_pdu(
					origin, room_id, event_id, pdu_json, true, // is_timeline_event
				))
				.await;

				promoted_any = true;
			}
		}

		// Cascade: if we promoted any event, we might have unrejected some auth
		// event that other events depend on, so recursively check them
		if promoted_any {
			Box::pin(self.unreject_rejected_events(origin, room_id, room_version)).await?;
		}

		Ok(())
	})
}
