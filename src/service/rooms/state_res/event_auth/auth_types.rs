use ruma::{
	UserId,
	events::{StateEventType, TimelineEventType},
	room_version_rules::AuthorizationRules,
};
use serde_json::value::RawValue as RawJsonValue;
use tuwunel_core::{Result, arrayvec::ArrayVec, matrix::pdu::MAX_AUTH_EVENTS};

use super::super::TypeStateKey;

pub type AuthTypes = ArrayVec<TypeStateKey, MAX_AUTH_EVENTS>;

/// Get the list of [relevant auth events] required to authorize the event of
/// the given type.
///
/// Returns a list of `(event_type, state_key)` tuples.
///
/// # Errors
///
/// Returns an `Err(_)` if a field could not be deserialized because `content`
/// does not respect the expected format for the `event_type`.
///
/// [relevant auth events]: https://spec.matrix.org/latest/server-server-api/#auth-events-selection
pub fn auth_types_for_event(
	event_type: &TimelineEventType,
	sender: &UserId,
	state_key: Option<&str>,
	content: &RawJsonValue,
	rules: &AuthorizationRules,
	always_create: bool,
) -> Result<AuthTypes> {
	let val: serde_json::Value = serde_json::from_str(content.get()).unwrap_or_default();
	let version = if rules.room_create_event_id_as_room_id {
		rezzy::StateResVersion::V2_1
	} else {
		rezzy::StateResVersion::V2
	};

	let rezzy_types = rezzy::auth::auth_types_for_event(
		&event_type.to_string(),
		sender.as_str(),
		state_key,
		&val,
		version,
	);

	let mut auth_types = AuthTypes::new();
	for (k, v) in rezzy_types {
		auth_types.push((k.into(), v.into()));
	}

	if always_create {
		let key = (StateEventType::RoomCreate, "".into());
		if !auth_types.contains(&key) {
			auth_types.push(key);
		}
	}

	Ok(auth_types)
}
