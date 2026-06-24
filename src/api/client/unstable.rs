use axum::extract::State;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as b64};
use futures::StreamExt;
use ruma::{
	OwnedRoomId, UInt,
	api::{
		client::{
			membership::mutual_rooms::v1::{Request, Response},
			profile::{delete_profile_field, get_profile_field, set_profile_field},
		},
		federation,
	},
	presence::PresenceState,
	profile::{ProfileFieldName, ProfileFieldValue},
};
use tuwunel_core::{Err, Result, err};
use tuwunel_service::users::propagation_default;

use super::profile::{
	enforce_profile_size, profile_mxc, profile_size_error, profile_str, resolve_propagation,
};
use crate::{ClientIp, Ruma};

/// MSC4133 maximum profile field-name length, in bytes.
const MAX_KEY_LENGTH: usize = 255;

/// Maximum number of rooms returned in a single `mutual_rooms` page.
const PAGE_SIZE: usize = 1000;

/// # `GET /_matrix/client/v1/mutual_rooms`
/// # `GET /_matrix/client/unstable/uk.half-shot.msc2666/user/mutual_rooms`
///
/// Gets all the rooms the sender shares with the specified user.
///
/// An implementation of [MSC2666](https://github.com/matrix-org/matrix-spec-proposals/pull/2666)
#[tracing::instrument(skip_all, fields(%client), name = "mutual_rooms")]
pub(crate) async fn get_mutual_rooms_route(
	State(services): State<crate::State>,
	ClientIp(client): ClientIp,
	body: Ruma<Request>,
) -> Result<Response> {
	let sender_user = body.sender_user();

	if sender_user == body.user_id {
		return Err!(Request(InvalidParam("You cannot request rooms in common with yourself.")));
	}

	if body.user_id.validate_historical().is_err() {
		return Err!(Request(InvalidParam("The user_id is not a compliant user identifier.")));
	}

	let all: Vec<OwnedRoomId> = services
		.state_cache
		.get_shared_rooms(sender_user, &body.user_id)
		.map(ToOwned::to_owned)
		.collect()
		.await;

	let count = UInt::try_from(all.len()).unwrap_or(UInt::MAX);

	let start = match body.from.as_deref() {
		| None => 0,
		| Some(token) => {
			let cursor = decode_cursor(token)
				.ok_or_else(|| err!(Request(InvalidParam("Invalid `from` token."))))?;

			all.partition_point(|room_id| room_id.as_str() <= cursor.as_str())
		},
	};

	let end = start.saturating_add(PAGE_SIZE).min(all.len());
	let next_batch = (end < all.len()).then(|| b64.encode(all[end.saturating_sub(1)].as_str()));

	let joined = if start == 0 && end == all.len() {
		all
	} else {
		all[start..end].to_vec()
	};

	Ok(Response { joined, count, next_batch })
}

/// Decodes a base64url pagination cursor to its room id.
fn decode_cursor(token: &str) -> Option<OwnedRoomId> {
	let bytes = b64.decode(token).ok()?;
	let room_id = str::from_utf8(&bytes).ok()?;

	OwnedRoomId::parse(room_id).ok()
}

/// # `PUT /_matrix/client/v3/profile/{user_id}/{field}`
///
/// Updates the profile key-value field of a user. Stabilized as part of
/// Matrix 1.16 (MSC4133); ruma's history block keeps the unstable
/// `uk.tcpip.msc4133` path mounted for older clients.
///
/// This also handles the avatar_url and displayname being updated.
pub(crate) async fn set_profile_field_route(
	State(services): State<crate::State>,
	ClientIp(client): ClientIp,
	body: Ruma<set_profile_field::v3::Request>,
) -> Result<set_profile_field::v3::Response> {
	let sender_user = body.sender_user();

	if *sender_user != body.user_id && body.appservice_info.is_none() {
		return Err!(Request(Forbidden("You cannot update the profile of another user")));
	}

	// MSC3823: displayname/avatar are forbidden during suspension; custom
	// MSC4133 fields fall through.
	if matches!(body.value, ProfileFieldValue::DisplayName(_) | ProfileFieldValue::AvatarUrl(_))
		&& services.users.is_suspended(sender_user).await
	{
		return Err!(Request(UserSuspended("Account is suspended.")));
	}

	let field_name = body.value.field_name();

	check_key_length(field_name.as_str())?;

	if !is_namespaced_key(field_name.as_str()) {
		return Err!(Request(BadJson(
			"Profile key names must follow the Common Namespaced Identifier Grammar."
		)));
	}

	enforce_profile_size(
		&services,
		&body.user_id,
		field_name.as_str(),
		body.value.value().into_owned(),
	)
	.await?;

	let propagation = resolve_propagation(
		&body.propagate_to,
		propagation_default(
			services
				.server
				.config
				.preserve_room_profile_overrides,
		),
	);

	match &body.value {
		| ProfileFieldValue::DisplayName(displayname) => {
			let all_joined_rooms: Vec<OwnedRoomId> = services
				.state_cache
				.rooms_joined(&body.user_id)
				.map(Into::into)
				.collect()
				.await;

			services
				.users
				.update_displayname(
					&body.user_id,
					Some(displayname),
					&all_joined_rooms,
					propagation,
				)
				.await;
		},
		| ProfileFieldValue::AvatarUrl(avatar_url) => {
			let all_joined_rooms: Vec<OwnedRoomId> = services
				.state_cache
				.rooms_joined(&body.user_id)
				.map(Into::into)
				.collect()
				.await;

			services
				.users
				.update_avatar_url(
					&body.user_id,
					Some(avatar_url),
					None,
					&all_joined_rooms,
					propagation,
				)
				.await;
		},
		| _ => {
			services.users.set_profile_key(
				&body.user_id,
				body.value.field_name().as_str(),
				Some(&body.value.value()),
			);
		},
	}

	// Presence update
	services
		.presence
		.maybe_ping_presence(
			&body.user_id,
			body.sender_device.as_deref(),
			Some(client),
			&PresenceState::Online,
		)
		.await?;

	Ok(set_profile_field::v3::Response {})
}

/// # `DELETE /_matrix/client/unstable/uk.tcpip.msc4133/profile/{user_id}/{field}`
///
/// Deletes the profile key-value field of a user, as per MSC4133.
///
/// This also handles the avatar_url and displayname being updated.
pub(crate) async fn delete_profile_field_route(
	State(services): State<crate::State>,
	ClientIp(client): ClientIp,
	body: Ruma<delete_profile_field::v3::Request>,
) -> Result<delete_profile_field::v3::Response> {
	let sender_user = body.sender_user();

	if *sender_user != body.user_id && body.appservice_info.is_none() {
		return Err!(Request(Forbidden("You cannot update the profile of another user")));
	}

	check_key_length(body.field.as_str())?;

	// MSC3823: displayname/avatar are forbidden during suspension; custom
	// MSC4133 fields fall through.
	if matches!(body.field, ProfileFieldName::DisplayName | ProfileFieldName::AvatarUrl)
		&& services.users.is_suspended(sender_user).await
	{
		return Err!(Request(UserSuspended("Account is suspended.")));
	}

	let propagation = resolve_propagation(
		&body.propagate_to,
		propagation_default(
			services
				.server
				.config
				.preserve_room_profile_overrides,
		),
	);

	match body.field {
		| ProfileFieldName::DisplayName => {
			let all_joined_rooms: Vec<OwnedRoomId> = services
				.state_cache
				.rooms_joined(&body.user_id)
				.map(Into::into)
				.collect()
				.await;

			services
				.users
				.update_displayname(&body.user_id, None, &all_joined_rooms, propagation)
				.await;
		},
		| ProfileFieldName::AvatarUrl => {
			let all_joined_rooms: Vec<OwnedRoomId> = services
				.state_cache
				.rooms_joined(&body.user_id)
				.map(Into::into)
				.collect()
				.await;

			services
				.users
				.update_avatar_url(&body.user_id, None, None, &all_joined_rooms, propagation)
				.await;
		},
		| _ => {
			services
				.users
				.set_profile_key(&body.user_id, body.field.as_str(), None);
		},
	}

	// Presence update
	services
		.presence
		.maybe_ping_presence(
			&body.user_id,
			body.sender_device.as_deref(),
			Some(client),
			&PresenceState::Online,
		)
		.await?;

	Ok(delete_profile_field::v3::Response {})
}

/// # `GET /_matrix/client/v3/profile/{userId}/{field}`
///
/// Gets the profile key-value field of a user, as per MSC4133.
///
/// - If user is on another server and we do not have a local copy already fetch
///   `timezone` over federation
pub(crate) async fn get_profile_field_route(
	State(services): State<crate::State>,
	body: Ruma<get_profile_field::v3::Request>,
) -> Result<get_profile_field::v3::Response> {
	check_key_length(body.field.as_str())?;

	if !services.globals.user_is_local(&body.user_id) {
		// Create and update our local copy of the user
		if let Ok(response) = services
			.federation
			.execute(
				body.user_id.server_name(),
				federation::query::get_profile_information::v1::Request {
					user_id: body.user_id.clone(),
					field: None, // we want the full user's profile to update locally as well
				},
			)
			.await
		{
			if !services.users.exists(&body.user_id).await {
				services
					.users
					.create(&body.user_id, None, None)
					.await?;
			}

			services
				.users
				.set_displayname(&body.user_id, profile_str(&response, "displayname"));

			services
				.users
				.set_avatar_url(&body.user_id, profile_mxc(&response, "avatar_url"));

			services
				.users
				.set_blurhash(&body.user_id, profile_str(&response, "blurhash"));

			services
				.users
				.set_timezone(&body.user_id, profile_str(&response, "m.tz"));

			let value = response.get(body.field.as_str()).ok_or_else(|| {
				err!(Request(NotFound("The requested profile key does not exist.")))
			})?;

			services
				.users
				.set_profile_key(&body.user_id, body.field.as_str(), Some(value));

			let profile_key_value = ProfileFieldValue::new(body.field.as_str(), value.clone())?;

			return Ok(get_profile_field::v3::Response { value: Some(profile_key_value) });
		}
	}

	if !services.users.exists(&body.user_id).await {
		// Return 404 if this user doesn't exist and we couldn't fetch it over
		// federation
		return Err!(Request(NotFound("Profile was not found.")));
	}

	let value = services
		.users
		.profile_key(&body.user_id, body.field.as_str())
		.await
		.and_then(|val| serde_json::to_value(val.json()).map_err(Into::into))
		.map_err(|_| err!(Request(NotFound("The requested profile key does not exist."))))?;

	let profile_key_value = ProfileFieldValue::new(body.field.as_str(), value)?;

	Ok(get_profile_field::v3::Response { value: Some(profile_key_value) })
}

/// Validate a profile field name against the Common Namespaced Identifier
/// Grammar: a lowercase-leading identifier over `[a-z0-9_.-]`, matching the
/// reference homeserver. Length is bounded separately by `MAX_KEY_LENGTH`.
fn is_namespaced_key(name: &str) -> bool {
	name.bytes()
		.next()
		.is_some_and(|b| b.is_ascii_lowercase())
		&& name.bytes().all(|b| {
			b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
		})
}

/// Reject an over-long MSC4133 profile field name with `M_KEY_TOO_LARGE`.
fn check_key_length(name: &str) -> Result<()> {
	(name.len() <= MAX_KEY_LENGTH)
		.then_some(())
		.ok_or_else(|| {
			profile_size_error(
				"M_KEY_TOO_LARGE",
				"Profile key names cannot be longer than 255 bytes.",
			)
		})
}
