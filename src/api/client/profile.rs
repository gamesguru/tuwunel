use std::collections::BTreeMap;

use axum::extract::State;
use futures::{
	StreamExt,
	future::{join, join4},
};
use ruma::{
	MxcUri, OwnedRoomId,
	api::{
		client::profile::{
			PropagateTo, get_avatar_url, get_display_name, get_profile, set_avatar_url,
			set_display_name,
		},
		federation::query::get_profile_information,
	},
	presence::PresenceState,
};
use serde_json::Value as JsonValue;
use tuwunel_core::{Err, Result, utils::future::TryExtExt};
use tuwunel_service::users::{Propagation, propagation_default};

use crate::{ClientIp, Ruma};

pub(super) type ProfileResponse = get_profile_information::v1::Response;

/// Pull a string field out of a federation profile-info response. The body
/// shape switched from explicit fields to a flat `BTreeMap<String, JsonValue>`
/// once extended profile fields stabilised.
pub(super) fn profile_str<'a>(resp: &'a ProfileResponse, field: &str) -> Option<&'a str> {
	resp.get(field).and_then(JsonValue::as_str)
}

pub(super) fn profile_mxc<'a>(resp: &'a ProfileResponse, field: &str) -> Option<&'a MxcUri> {
	profile_str(resp, field).map(<&MxcUri>::from)
}

/// Resolve a `PropagateTo` request value against the server default.
///
/// MSC4466's `_Custom` variant is treated as the server default so
/// unknown values do not silently change behavior.
pub(super) fn resolve_propagation(
	propagate_to: &PropagateTo,
	server_default: Propagation,
) -> Propagation {
	match propagate_to {
		| PropagateTo::All => Propagation::All,
		| PropagateTo::Unchanged => Propagation::Unchanged,
		| PropagateTo::None => Propagation::None,
		| _ => server_default,
	}
}

/// # `PUT /_matrix/client/r0/profile/{userId}/displayname`
///
/// Updates the displayname.
///
/// - Also makes sure other users receive the update using presence EDUs
pub(crate) async fn set_displayname_route(
	State(services): State<crate::State>,
	ClientIp(client): ClientIp,
	body: Ruma<set_display_name::v3::Request>,
) -> Result<set_display_name::v3::Response> {
	let sender_user = body.sender_user();

	if *sender_user != body.user_id && body.appservice_info.is_none() {
		return Err!(Request(Forbidden("You cannot update the profile of another user")));
	}

	let all_joined_rooms: Vec<OwnedRoomId> = services
		.state_cache
		.rooms_joined(&body.user_id)
		.map(ToOwned::to_owned)
		.collect()
		.await;

	let propagation = resolve_propagation(
		&body.propagate_to,
		propagation_default(
			services
				.server
				.config
				.preserve_room_profile_overrides,
		),
	);

	services
		.users
		.update_displayname(
			&body.user_id,
			body.displayname.as_deref(),
			&all_joined_rooms,
			propagation,
		)
		.await;

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

	Ok(set_display_name::v3::Response {})
}

/// # `GET /_matrix/client/v3/profile/{userId}/displayname`
///
/// Returns the displayname of the user.
///
/// - If user is on another server and we do not have a local copy already fetch
///   displayname over federation
pub(crate) async fn get_displayname_route(
	State(services): State<crate::State>,
	body: Ruma<get_display_name::v3::Request>,
) -> Result<get_display_name::v3::Response> {
	if !services.globals.user_is_local(&body.user_id) {
		// Create and update our local copy of the user
		if let Ok(response) = services
			.federation
			.execute(body.user_id.server_name(), get_profile_information::v1::Request {
				user_id: body.user_id.clone(),
				field: None, // we want the full user's profile to update locally too
			})
			.await
		{
			if !services.users.exists(&body.user_id).await {
				services
					.users
					.create(&body.user_id, None, None)
					.await?;
			}

			let displayname = profile_str(&response, "displayname");
			services
				.users
				.set_displayname(&body.user_id, displayname);
			services
				.users
				.set_avatar_url(&body.user_id, profile_mxc(&response, "avatar_url"));
			services
				.users
				.set_blurhash(&body.user_id, profile_str(&response, "blurhash"));

			return Ok(get_display_name::v3::Response {
				displayname: displayname.map(str::to_owned),
			});
		}
	}

	if !services.users.exists(&body.user_id).await {
		// Return 404 if this user doesn't exist and we couldn't fetch it over
		// federation
		return Err!(Request(NotFound("Profile was not found.")));
	}

	Ok(get_display_name::v3::Response {
		displayname: services
			.users
			.displayname(&body.user_id)
			.await
			.ok(),
	})
}

/// # `PUT /_matrix/client/v3/profile/{userId}/avatar_url`
///
/// Updates the `avatar_url` and `blurhash`.
///
/// - Also makes sure other users receive the update using presence EDUs
pub(crate) async fn set_avatar_url_route(
	State(services): State<crate::State>,
	ClientIp(client): ClientIp,
	body: Ruma<set_avatar_url::v3::Request>,
) -> Result<set_avatar_url::v3::Response> {
	let sender_user = body.sender_user();

	if *sender_user != body.user_id && body.appservice_info.is_none() {
		return Err!(Request(Forbidden("You cannot update the profile of another user")));
	}

	let all_joined_rooms: Vec<OwnedRoomId> = services
		.state_cache
		.rooms_joined(&body.user_id)
		.map(ToOwned::to_owned)
		.collect()
		.await;

	let propagation = resolve_propagation(
		&body.propagate_to,
		propagation_default(
			services
				.server
				.config
				.preserve_room_profile_overrides,
		),
	);

	services
		.users
		.update_avatar_url(
			&body.user_id,
			body.avatar_url.as_deref(),
			body.blurhash.as_deref(),
			&all_joined_rooms,
			propagation,
		)
		.await;

	// Presence update
	services
		.presence
		.maybe_ping_presence(
			&body.user_id,
			body.sender_device.as_deref(),
			Some(client),
			&PresenceState::Online,
		)
		.await
		.ok();

	Ok(set_avatar_url::v3::Response {})
}

/// # `GET /_matrix/client/v3/profile/{userId}/avatar_url`
///
/// Returns the `avatar_url` and `blurhash` of the user.
///
/// - If user is on another server and we do not have a local copy already fetch
///   `avatar_url` and blurhash over federation
pub(crate) async fn get_avatar_url_route(
	State(services): State<crate::State>,
	body: Ruma<get_avatar_url::v3::Request>,
) -> Result<get_avatar_url::v3::Response> {
	if !services.globals.user_is_local(&body.user_id) {
		// Create and update our local copy of the user
		if let Ok(response) = services
			.federation
			.execute(body.user_id.server_name(), get_profile_information::v1::Request {
				user_id: body.user_id.clone(),
				field: None, // we want the full user's profile to update locally as well
			})
			.await
		{
			if !services.users.exists(&body.user_id).await {
				services
					.users
					.create(&body.user_id, None, None)
					.await?;
			}

			let avatar_url = profile_mxc(&response, "avatar_url");
			let blurhash = profile_str(&response, "blurhash");
			services
				.users
				.set_displayname(&body.user_id, profile_str(&response, "displayname"));
			services
				.users
				.set_avatar_url(&body.user_id, avatar_url);
			services
				.users
				.set_blurhash(&body.user_id, blurhash);

			return Ok(get_avatar_url::v3::Response {
				avatar_url: avatar_url.map(ToOwned::to_owned),
				blurhash: blurhash.map(str::to_owned),
			});
		}
	}

	if !services.users.exists(&body.user_id).await {
		// Return 404 if this user doesn't exist and we couldn't fetch it over
		// federation
		return Err!(Request(NotFound("Profile was not found.")));
	}

	let (avatar_url, blurhash) = join(
		services.users.avatar_url(&body.user_id).ok(),
		services.users.blurhash(&body.user_id).ok(),
	)
	.await;

	Ok(get_avatar_url::v3::Response { avatar_url, blurhash })
}

/// # `GET /_matrix/client/v3/profile/{userId}`
///
/// Returns the displayname, avatar_url, blurhash, and tz of the user.
///
/// - If user is on another server and we do not have a local copy already,
///   fetch profile over federation.
pub(crate) async fn get_profile_route(
	State(services): State<crate::State>,
	body: Ruma<get_profile::v3::Request>,
) -> Result<get_profile::v3::Response> {
	const CANONICAL_FIELDS: &[&str] = &["avatar_url", "blurhash", "displayname", "m.tz"];

	let is_local = services.globals.user_is_local(&body.user_id);
	let allow_outbound = services
		.server
		.config
		.allow_outbound_profile_lookup_federation_requests;

	if !is_local && allow_outbound {
		// Create and update our local copy of the user
		if let Ok(response) = services
			.federation
			.execute(body.user_id.server_name(), get_profile_information::v1::Request {
				user_id: body.user_id.clone(),
				field: None,
			})
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

			for (key, value) in response.iter() {
				if CANONICAL_FIELDS.contains(&key.as_str()) {
					continue;
				}
				services
					.users
					.set_profile_key(&body.user_id, key, Some(value));
			}

			return Ok(response
				.iter()
				.map(|(key, val)| (key.clone(), val.clone()))
				.collect::<get_profile::v3::Response>());
		}
	}

	if !services.users.exists(&body.user_id).await {
		if !is_local && !allow_outbound {
			// MSC3550: signal a withheld profile, not a missing user.
			return Err!(Request(Forbidden(
				"Profile lookup over federation is not allowed on this homeserver."
			)));
		}

		// Return 404 if this user doesn't exist and we couldn't fetch it over
		// federation
		return Err!(Request(NotFound("Profile was not found.")));
	}

	let mut custom_profile_fields: BTreeMap<String, _> = services
		.users
		.all_profile_keys(&body.user_id)
		.collect()
		.await;

	// services.users.timezone will collect the MSC4175 timezone key if it exists
	custom_profile_fields.remove("us.cloke.msc4175.tz");
	custom_profile_fields.remove("m.tz");

	let (avatar_url, blurhash, displayname, tz) = join4(
		services.users.avatar_url(&body.user_id).ok(),
		services.users.blurhash(&body.user_id).ok(),
		services.users.displayname(&body.user_id).ok(),
		services.users.timezone(&body.user_id).ok(),
	)
	.await;

	let canonical_fields = [
		("avatar_url", avatar_url.map(Into::into)),
		("blurhash", blurhash),
		("displayname", displayname),
		("m.tz", tz),
	];

	Ok(canonical_fields
		.into_iter()
		.map(|(key, val)| (key.to_owned(), val))
		.filter_map(|(key, val)| {
			val.map(serde_json::to_value)
				.transpose()
				.ok()
				.flatten()
				.map(|val| (key, val))
		})
		.chain(
			custom_profile_fields
				.into_iter()
				.filter_map(|(key, val)| {
					serde_json::to_value(val.json())
						.map(|val| (key, val))
						.ok()
				}),
		)
		.collect())
}
