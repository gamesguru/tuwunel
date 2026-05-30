use std::fmt::Write;

use ruma::{
	OwnedEventId,
	signatures::{PublicKeyMap, PublicKeySet, Verified, required_keys, verify_json},
};
use tuwunel_core::{Result, matrix::room_version};

use crate::admin_command;

#[admin_command]
pub(super) async fn verify_pdu(&self, event_id: OwnedEventId) -> Result {
	let mut event = self
		.services
		.timeline
		.get_pdu_json(&event_id)
		.await?;

	let room_id = event
		.get("room_id")
		.and_then(|v| v.as_str())
		.and_then(|s| <&ruma::RoomId>::try_from(s).ok());

	let room_version_id = if let Some(room_id) = room_id {
		self.services
			.state
			.get_room_version(room_id)
			.await
			.ok()
	} else {
		None
	};

	let room_version_id = room_version_id.unwrap_or(ruma::RoomVersionId::V11);
	let room_version_rules = room_version::rules(&room_version_id)?;

	event.remove("event_id");

	let mut out = String::new();
	writeln!(out, "Event ID: {event_id}")?;
	if let Some(room_id) = room_id {
		writeln!(out, "Room ID: {room_id}")?;
	}
	writeln!(out, "Room Version: {room_version_id}")?;

	let required = required_keys(&event, &room_version_rules.signatures)?;

	writeln!(out, "\nSignatures:")?;

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
					set.insert(key_id.to_owned(), key.key);
					map.insert(server.to_owned(), set);

					match verify_json(&map, &event) {
						| Ok(_) => {
							writeln!(out, "  - {server} ({key_id}): OK")?;
						},
						| Err(e) => {
							writeln!(out, "  - {server} ({key_id}): FAILED ({e})")?;
						},
					}
				},
				| Err(_) => {
					writeln!(out, "  - {server} ({key_id}): MISSING KEY")?;
				},
			}
		}
	}

	let verification = self
		.services
		.server_keys
		.verify_event(&event, Some(&room_version_id))
		.await;

	writeln!(out, "\nSummary:")?;
	match verification {
		| Ok(Verified::All) => {
			writeln!(out, "  - Overall: OK")?;
			writeln!(out, "  - Hashes: OK")?;
		},
		| Ok(Verified::Signatures) => {
			writeln!(out, "  - Overall: REDACTED / HASH FAILED")?;
			writeln!(out, "  - Signatures: OK")?;
			writeln!(out, "  - Hashes: FAILED (expected if redacted)")?;
		},
		| Err(e) => {
			writeln!(out, "  - Overall: FAILED")?;
			writeln!(out, "  - Error: {e}")?;
		},
	}

	self.write_str(&out).await
}
