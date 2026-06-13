use std::fmt::Write;

use futures::StreamExt;
use ruma::{
	OwnedEventId,
	events::{TimelineEventType, room::member::RoomMemberEventContent},
	signatures::{PublicKeyMap, PublicKeySet, Verified, required_keys, verify_json},
};
use tuwunel_core::{
	Result,
	matrix::{Event, event::TypeExt, room_version},
	utils::stream::IterStream,
};
use tuwunel_service::rooms::state_res;

use crate::admin_command;

#[admin_command]
pub(super) async fn verify_pdu(&self, event_id: OwnedEventId) -> Result {
	let pdu = self.services.timeline.get_pdu(&event_id).await?;
	let pdu_json = self
		.services
		.timeline
		.get_pdu_json(&event_id)
		.await?;

	let mut out = String::new();

	let room_id = pdu.room_id();
	let room_version_id = match self
		.services
		.state
		.get_room_version(room_id)
		.await
	{
		| Ok(v) => v,
		| Err(_) => {
			writeln!(out, "Warning: No m.room.create found, defaulting to Room Version 1")?;
			ruma::RoomVersionId::V1
		},
	};

	let room_rules = room_version::rules(&room_version_id)?;

	writeln!(out, "Event: {event_id}")?;
	writeln!(out, "Room: {room_id}")?;
	writeln!(out, "Type: {}", pdu.kind())?;

	if *pdu.kind() == TimelineEventType::RoomMember {
		if let Ok(content) = pdu.get_content::<RoomMemberEventContent>() {
			writeln!(out, "Membership: {}", content.membership)?;
		}
	}

	if let Some(state_key) = pdu.state_key() {
		writeln!(out, "State key: {state_key}")?;
	}

	writeln!(out, "Sender: {}", pdu.sender())?;
	writeln!(out, "Room Version: {room_version_id}")?;

	// Verify (Signatures)
	let mut verify_err = String::new();
	let required = required_keys(&pdu_json, &room_rules.signatures)?;
	for (server, key_ids) in required {
		for key_id in key_ids {
			let pubkey = self
				.services
				.server_keys
				.get_verify_key(&server, &key_id)
				.await;

			match pubkey {
				| Ok(key) => {
					let mut map = PublicKeyMap::new();
					let mut set = PublicKeySet::new();
					set.insert(key_id.as_str().to_owned(), key.key);
					map.insert(server.as_str().to_owned(), set);

					if let Err(e) = verify_json(&map, &pdu_json) {
						if !verify_err.is_empty() {
							verify_err.push_str(", ");
						}
						write!(verify_err, "{server} ({key_id}): {e}")?;
					}
				},
				| Err(_) => {
					if !verify_err.is_empty() {
						verify_err.push_str(", ");
					}
					write!(verify_err, "{server} ({key_id}): MISSING KEY")?;
				},
			}
		}
	}

	if verify_err.is_empty() {
		let verification = self
			.services
			.server_keys
			.verify_event(&pdu_json, Some(&room_version_id))
			.await;

		match verification {
			| Ok(Verified::All) => writeln!(out, "Verify: OK")?,
			| Ok(Verified::Signatures) => writeln!(out, "Verify: REDACTED / HASH FAILED")?,
			| Err(e) => writeln!(out, "Verify: FAILED ({e})")?,
		}
	} else {
		writeln!(out, "Verify: SIGNATURE FAILED: {verify_err}")?;
	}

	// Auth check
	let auth_events: Vec<_> = pdu
		.auth_events()
		.map(ToOwned::to_owned)
		.collect::<Vec<_>>()
		.stream()
		.filter_map(|auth_event_id| async move {
			self.services
				.timeline
				.get_pdu(&auth_event_id)
				.await
				.ok()
		})
		.map(|auth_event| {
			let event_type = auth_event.kind().clone();
			let state_key = auth_event
				.state_key()
				.map(ToOwned::to_owned)
				.unwrap_or_default();
			(event_type.with_state_key(state_key), auth_event)
		})
		.collect()
		.await;

	let auth_check_res = state_res::auth_check(
		&room_rules,
		&pdu,
		&async |event_id| self.services.timeline.get_pdu(&event_id).await,
		&async |event_type, state_key| {
			let target = event_type.with_state_key(state_key);
			auth_events
				.iter()
				.find(|(type_state_key, _)| *type_state_key == target)
				.map(|(_, pdu)| pdu.clone())
				.ok_or_else(|| tuwunel_core::err!(Request(NotFound("state not found"))))
		},
	)
	.await;

	match auth_check_res {
		| Ok(()) => writeln!(out, "Auth check: OK")?,
		| Err(e) => writeln!(out, "Auth check: FAIL ({e})")?,
	}

	// Status
	let in_timeline = self
		.services
		.timeline
		.get_pdu_id(&event_id)
		.await
		.is_ok();

	let is_outlier = self
		.services
		.timeline
		.get_outlier_pdu(&event_id)
		.await
		.is_ok();

	let is_soft_failed = self
		.services
		.pdu_metadata
		.is_event_soft_failed(&event_id)
		.await;

	writeln!(
		out,
		"Status: timeline={in_timeline} outlier={is_outlier} rejected=false \
		 soft_failed={is_soft_failed}"
	)?;

	self.write_str(&out).await
}
