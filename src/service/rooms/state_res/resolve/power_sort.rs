use ruma::{
	OwnedEventId,
	events::{TimelineEventType, room::power_levels::UserPowerLevel},
	room_version_rules::RoomVersionRules,
};
use tuwunel_core::{Result, matrix::Event};

use super::super::events::{
	RoomCreateEvent, RoomPowerLevelsEvent, RoomPowerLevelsIntField,
	power_levels::RoomPowerLevelsEventOptionExt,
};

/// Find the power level for the sender of a pre-fetched PDU, avoiding the
/// initial database fetch.
///
/// We find the most recent `m.room.power_levels` by walking backwards in the
/// auth chain of the event.
///
/// This function is designed for use during state resolution (such as
/// topological sorting or initial state-resolution preprocessing of conflicted
/// events), where a fully resolved room state is not yet available.
///
/// Do NOT use this outside of the state resolution context. In other contexts
/// (such as standard event processing, permission checks, or client API
/// handlers), you should look up the sender's power level using the room's
/// current resolved state, as naively walking the auth chain is highly
/// inefficient and may not align with the resolved state of the room.
///
/// ## Arguments
///
/// * `event` - The pre-fetched PDU of the event.
///
/// * `rules` - The authorization rules for the current room version.
///
/// * `fetch` - Function to fetch an event in the room given its event ID.
///
/// ## Returns
///
/// Returns the power level of the sender of the event or an `Err(_)` if one of
/// the auth events is malformed.
#[tracing::instrument(
	name = "pdu_sender_power",
	level = "trace",
	skip_all,
	fields(
		event_id = ?event.event_id(),
	)
)]
pub(super) async fn power_level_for_pdu_sender<Fetch, Fut, Pdu>(
	event: &Pdu,
	rules: &RoomVersionRules,
	fetch: &Fetch,
) -> Result<UserPowerLevel>
where
	Fetch: Fn(OwnedEventId) -> Fut + Sync,
	Fut: Future<Output = Result<Pdu>> + Send,
	Pdu: Event,
{
	let hydra_room_id = rules
		.authorization
		.room_create_event_id_as_room_id;

	let mut create_event = None;
	let mut power_levels_event = None;
	if hydra_room_id {
		let create_id = event.room_id().as_event_id()?;
		let fetched = fetch(create_id).await?;

		_ = create_event.insert(RoomCreateEvent::new(fetched));
	}

	for auth_event_id in event.auth_events() {
		use TimelineEventType::{RoomCreate, RoomPowerLevels};

		let Ok(auth_event) = fetch(auth_event_id.to_owned()).await else {
			continue;
		};

		if !hydra_room_id && auth_event.is_type_and_state_key(&RoomCreate, "") {
			_ = create_event.get_or_insert_with(|| RoomCreateEvent::new(auth_event));
		} else if auth_event.is_type_and_state_key(&RoomPowerLevels, "") {
			_ = power_levels_event.get_or_insert_with(|| RoomPowerLevelsEvent::new(auth_event));
		}

		if power_levels_event.is_some() && create_event.is_some() {
			break;
		}
	}

	let creators = create_event
		.as_ref()
		.and_then(|event| event.creators(&rules.authorization).ok());

	if let Some(creators) = creators {
		power_levels_event.user_power_level(event.sender(), creators, &rules.authorization)
	} else {
		power_levels_event
			.get_as_int_or_default(RoomPowerLevelsIntField::UsersDefault, &rules.authorization)
			.map(Into::into)
	}
}
