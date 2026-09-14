use std::fmt::Write;

use futures::StreamExt;
use ruma::{
	CanonicalJsonObject, OwnedEventId,
	events::{TimelineEventType, room::member::RoomMemberEventContent},
	signatures::{PublicKeyMap, PublicKeySet, Verified, required_keys, verify_json},
};
use tuwunel_core::{
	Result,
	matrix::{Event, event::TypeExt, room_version},
	utils::stream::IterStream,
};
use tuwunel_service::rooms::state_res;

use crate::{admin_command, context::Context};

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
	let room_version_id = get_room_version_id(self, room_id, &mut out).await?;
	let room_rules = room_version::rules(&room_version_id)?;

	writeln!(out, "Event: {event_id}")?;
	writeln!(out, "Room: {room_id}")?;
	writeln!(out, "Type: {}", pdu.kind())?;

	if *pdu.kind() == TimelineEventType::RoomMember
		&& let Ok(content) = pdu.get_content::<RoomMemberEventContent>()
	{
		writeln!(out, "Membership: {}", content.membership)?;
	}

	if let Some(state_key) = pdu.state_key() {
		writeln!(out, "State key: {state_key}")?;
	}

	writeln!(out, "Sender: {}", pdu.sender())?;
	writeln!(out, "Room Version: {room_version_id}")?;

	// Verify (Signatures)
	verify_signatures(self, &pdu_json, &room_version_id, &room_rules, &mut out).await?;

	// Auth check
	verify_auth_events(self, &pdu, &room_rules, &mut out).await?;

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

	let is_rejected = {
		#[cfg(test)]
		{
			pdu.rejected().to_string()
		}
		#[cfg(not(test))]
		{
			"unknown".to_owned()
		}
	};

	writeln!(
		out,
		"Status: timeline={in_timeline} outlier={is_outlier} rejected={is_rejected} \
		 soft_failed={is_soft_failed}"
	)?;

	self.write_str(&out).await
}

async fn get_room_version_id(
	context: &Context<'_>,
	room_id: &ruma::RoomId,
	out: &mut String,
) -> Result<ruma::RoomVersionId> {
	match context
		.services
		.state
		.get_room_version(room_id)
		.await
	{
		| Ok(v) => Ok(v),
		| Err(tuwunel_core::Error::Request(ruma::api::error::ErrorKind::NotFound, ..)) => {
			writeln!(out, "Warning: No m.room.create found, defaulting to Room Version 1")?;
			Ok(ruma::RoomVersionId::V1)
		},
		| Err(e) => Err(e),
	}
}

async fn verify_signatures(
	context: &Context<'_>,
	pdu_json: &CanonicalJsonObject,
	room_version_id: &ruma::RoomVersionId,
	room_rules: &room_version::RoomVersionRules,
	out: &mut String,
) -> Result {
	let mut verify_err = String::new();
	let mut has_failure = false;
	let required = required_keys(pdu_json, &room_rules.signatures)?;
	let mut verify_json_pdu = pdu_json.clone();
	if !room_rules.event_format.require_event_id {
		verify_json_pdu.remove("event_id");
	}

	for (server, key_ids) in required {
		for key_id in key_ids {
			let pubkey = context
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

					match verify_json(&map, &verify_json_pdu) {
						| Ok(()) => {
							if !verify_err.is_empty() {
								verify_err.push_str(", ");
							}
							write!(verify_err, "{server} ({key_id}): OK")?;
						},
						| Err(e) => {
							has_failure = true;
							if !verify_err.is_empty() {
								verify_err.push_str(", ");
							}
							write!(verify_err, "{server} ({key_id}): {e}")?;
						},
					}
				},
				| Err(_) => {
					has_failure = true;
					if !verify_err.is_empty() {
						verify_err.push_str(", ");
					}
					write!(verify_err, "{server} ({key_id}): MISSING KEY")?;
				},
			}
		}
	}

	if !has_failure {
		let verification = context
			.services
			.server_keys
			.verify_event(pdu_json, Some(room_version_id))
			.await;

		match verification {
			| Ok(Verified::All) => writeln!(out, "Verify: OK")?,
			| Ok(Verified::Signatures) => writeln!(out, "Verify: REDACTED / HASH FAILED")?,
			| Err(e) => writeln!(out, "Verify: FAILED ({e})")?,
		}
	} else {
		writeln!(out, "Verify: SIGNATURE FAILED: {verify_err}")?;
	}

	Ok(())
}

async fn verify_auth_events(
	context: &Context<'_>,
	pdu: &tuwunel_core::PduEvent,
	room_rules: &room_version::RoomVersionRules,
	out: &mut String,
) -> Result {
	let mut auth_errors = Vec::new();
	let auth_events: Vec<_> = pdu
		.auth_events()
		.map(ToOwned::to_owned)
		.collect::<Vec<_>>()
		.stream()
		.then(|auth_event_id| async move {
			let res = context
				.services
				.timeline
				.get_pdu(&auth_event_id)
				.await;
			(auth_event_id, res)
		})
		.collect::<Vec<_>>()
		.await
		.into_iter()
		.filter_map(|(auth_event_id, res)| match res {
			| Ok(auth_event) => {
				let event_type = auth_event.kind().clone();
				let state_key = auth_event
					.state_key()
					.map(ToOwned::to_owned)
					.unwrap_or_default();
				Some((event_type.with_state_key(state_key), auth_event))
			},
			| Err(e) => {
				auth_errors.push((auth_event_id, e));
				None
			},
		})
		.collect();

	for (auth_event_id, err) in &auth_errors {
		writeln!(out, "Warning: Failed to fetch auth event {auth_event_id}: {err}")?;
	}

	let auth_check_res = state_res::auth_check(
		room_rules,
		pdu,
		&async |event_id| context.services.timeline.get_pdu(&event_id).await,
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

	Ok(())
}
