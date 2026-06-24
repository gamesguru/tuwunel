use std::collections::BTreeMap;

use futures::{
	FutureExt, Stream, StreamExt, TryFutureExt, TryStreamExt,
	future::{join3, join4},
};
use ruma::{
	MxcUri, OwnedMxcUri, OwnedRoomId, RoomId, UserId,
	events::room::member::{MembershipState, RoomMemberEventContent},
	profile::ProfileFieldValue,
	serde::Raw,
};
use serde_json::Value as JsonValue;
use tuwunel_core::{
	Result, implement,
	matrix::PduBuilder,
	utils::{
		future::TryExtExt,
		stream::{IterStream, ReadyExt, TryIgnore},
	},
};
use tuwunel_database::{Deserialized, Ignore, Interfix, Json};

/// Per-update policy for fanning a global profile change out to each of
/// the user's joined rooms as a fresh `m.room.member` event. Mirrors the
/// MSC4466 `propagate_to` axis.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Propagation {
	/// Send a member event to every joined room.
	All,

	/// Send a member event only to rooms whose current per-room value
	/// matches the user's prior global value; rooms with a per-room
	/// override (e.g. set via `/myroomnick`) are skipped.
	Unchanged,

	/// Send no member events; update the global profile only.
	None,
}

/// Server-default propagation when a request does not carry an explicit
/// MSC4466 `propagate_to`. `Unchanged` when overrides should be
/// preserved, `All` for legacy clobber-every-room behavior.
#[inline]
#[must_use]
pub fn propagation_default(preserve_room_profile_overrides: bool) -> Propagation {
	if preserve_room_profile_overrides {
		Propagation::Unchanged
	} else {
		Propagation::All
	}
}

#[implement(super::Service)]
pub async fn update_displayname(
	&self,
	user_id: &UserId,
	displayname: Option<&str>,
	rooms: &[OwnedRoomId],
	propagation: Propagation,
) {
	let (current_avatar_url, current_blurhash, current_displayname) = join3(
		self.services.users.avatar_url(user_id).ok(),
		self.services.users.blurhash(user_id).ok(),
		self.services.users.displayname(user_id).ok(),
	)
	.await;

	if displayname == current_displayname.as_deref() {
		return;
	}

	self.services
		.users
		.set_displayname(user_id, displayname);

	if matches!(propagation, Propagation::None) {
		return;
	}

	let make_pdu = || {
		PduBuilder::state(user_id.to_string(), &RoomMemberEventContent {
			displayname: displayname.map(ToOwned::to_owned),
			membership: MembershipState::Join,
			avatar_url: current_avatar_url.clone(),
			blurhash: current_blurhash.clone(),
			join_authorized_via_users_server: None,
			reason: None,
			is_direct: None,
			third_party_invite: None,
		})
	};

	let keep = async |room_id: &RoomId| match propagation {
		| Propagation::All => true,
		| Propagation::None => false,
		| Propagation::Unchanged =>
			self.member_displayname(room_id, user_id)
				.await
				.as_deref() == current_displayname.as_deref(),
	};

	let rooms = rooms
		.iter()
		.try_stream()
		.try_filter(|room_id: &&OwnedRoomId| keep(room_id))
		.and_then(async |room_id: &OwnedRoomId| Ok((make_pdu(), room_id)))
		.ignore_err();

	self.update_all_rooms(user_id, rooms)
		.boxed()
		.await;
}

/// Sets a new displayname or removes it if displayname is None. You still
/// need to notify all rooms of this change.
#[implement(super::Service)]
pub fn set_displayname(&self, user_id: &UserId, displayname: Option<&str>) {
	if let Some(displayname) = displayname {
		self.db
			.userid_displayname
			.insert(user_id, displayname);
	} else {
		self.db.userid_displayname.remove(user_id);
	}
}

/// Returns the displayname of a user on this homeserver.
#[implement(super::Service)]
pub async fn displayname(&self, user_id: &UserId) -> Result<String> {
	self.db
		.userid_displayname
		.get(user_id)
		.await
		.deserialized()
}

#[implement(super::Service)]
pub async fn update_avatar_url(
	&self,
	user_id: &UserId,
	avatar_url: Option<&MxcUri>,
	blurhash: Option<&str>,
	rooms: &[OwnedRoomId],
	propagation: Propagation,
) {
	let (current_avatar_url, current_blurhash, current_displayname) = join3(
		self.services.users.avatar_url(user_id).ok(),
		self.services.users.blurhash(user_id).ok(),
		self.services.users.displayname(user_id).ok(),
	)
	.await;

	if current_avatar_url.as_deref() == avatar_url && current_blurhash.as_deref() == blurhash {
		return;
	}

	self.services
		.users
		.set_avatar_url(user_id, avatar_url);
	self.services
		.users
		.set_blurhash(user_id, blurhash);

	if matches!(propagation, Propagation::None) {
		return;
	}

	let make_pdu = || {
		PduBuilder::state(user_id.to_string(), &RoomMemberEventContent {
			avatar_url: avatar_url.map(ToOwned::to_owned),
			blurhash: blurhash.map(ToOwned::to_owned),
			membership: MembershipState::Join,
			displayname: current_displayname.clone(),
			join_authorized_via_users_server: None,
			reason: None,
			is_direct: None,
			third_party_invite: None,
		})
	};

	let keep = async |room_id: &RoomId| match propagation {
		| Propagation::All => true,
		| Propagation::None => false,
		| Propagation::Unchanged =>
			self.member_avatar_url(room_id, user_id)
				.await
				.as_deref() == current_avatar_url.as_deref(),
	};

	let rooms = rooms
		.iter()
		.try_stream()
		.try_filter(|room_id: &&OwnedRoomId| keep(room_id))
		.and_then(async |room_id: &OwnedRoomId| Ok((make_pdu(), room_id)))
		.ignore_err();

	self.update_all_rooms(user_id, rooms)
		.boxed()
		.await;
}

/// Sets a new avatar_url or removes it if avatar_url is None.
#[implement(super::Service)]
pub fn set_avatar_url(&self, user_id: &UserId, avatar_url: Option<&MxcUri>) {
	if let Some(avatar_url) = avatar_url {
		self.db
			.userid_avatarurl
			.insert(user_id, avatar_url);
	} else {
		self.db.userid_avatarurl.remove(user_id);
	}
}

/// Get the `avatar_url` of a user.
#[implement(super::Service)]
pub async fn avatar_url(&self, user_id: &UserId) -> Result<OwnedMxcUri> {
	self.db
		.userid_avatarurl
		.get(user_id)
		.await
		.deserialized()
}

/// Sets a new avatar_url or removes it if avatar_url is None.
#[implement(super::Service)]
pub fn set_blurhash(&self, user_id: &UserId, blurhash: Option<&str>) {
	if let Some(blurhash) = blurhash {
		self.db.userid_blurhash.insert(user_id, blurhash);
	} else {
		self.db.userid_blurhash.remove(user_id);
	}
}

/// Get the blurhash of a user.
#[implement(super::Service)]
pub async fn blurhash(&self, user_id: &UserId) -> Result<String> {
	self.db
		.userid_blurhash
		.get(user_id)
		.await
		.deserialized()
}

/// Sets a new timezone or removes it if timezone is None.
#[implement(super::Service)]
pub fn set_timezone(&self, user_id: &UserId, timezone: Option<&str>) {
	let key = (user_id, "m.tz");

	if let Some(timezone) = timezone {
		self.db
			.useridprofilekey_value
			.put_raw(key, timezone);
	} else {
		self.db.useridprofilekey_value.del(key);
	}
}

/// Get the timezone of a user.
#[implement(super::Service)]
pub async fn timezone(&self, user_id: &UserId) -> Result<String> {
	//TODO: remove unstable key eventually.
	let stable_key = (user_id, "m.tz");
	let unstable_key = (user_id, "us.cloke.msc4175.tz");
	self.db
		.useridprofilekey_value
		.qry(&stable_key)
		.or_else(|_| self.db.useridprofilekey_value.qry(&unstable_key))
		.await
		.deserialized()
}

/// Gets all the user's profile keys and values in an iterator
#[implement(super::Service)]
pub fn all_profile_keys<'a>(
	&'a self,
	user_id: &'a UserId,
) -> impl Stream<Item = (String, Raw<ProfileFieldValue>)> + 'a + Send {
	type KeyVal = ((Ignore, String), Raw<ProfileFieldValue>);

	let prefix = (user_id, Interfix);
	self.db
		.useridprofilekey_value
		.stream_prefix(&prefix)
		.ignore_err()
		.map(|((_, key), val): KeyVal| (key, val))
}

/// MSC4133: the user's full local profile as `GET /profile/{user}` serves
/// it, the canonical fields (avatar_url, blurhash, displayname, m.tz) merged
/// over the custom profile keys. Backs the profile response and the 64 KiB
/// `M_PROFILE_TOO_LARGE` size cap.
#[implement(super::Service)]
pub async fn full_profile(&self, user_id: &UserId) -> BTreeMap<String, JsonValue> {
	let mut fields: BTreeMap<String, JsonValue> = self
		.all_profile_keys(user_id)
		.ready_filter_map(|(key, val)| {
			serde_json::to_value(val.json())
				.ok()
				.map(|val| (key, val))
		})
		.collect()
		.await;

	// timezone() resolves m.tz (and the legacy MSC4175 key); drop the raw
	// stored copies so the canonical m.tz below is served once.
	fields.remove("us.cloke.msc4175.tz");
	fields.remove("m.tz");

	let (avatar_url, blurhash, displayname, tz) = join4(
		self.avatar_url(user_id).ok(),
		self.blurhash(user_id).ok(),
		self.displayname(user_id).ok(),
		self.timezone(user_id).ok(),
	)
	.await;

	let canonical = [
		("avatar_url", avatar_url.map(Into::into)),
		("blurhash", blurhash),
		("displayname", displayname),
		("m.tz", tz),
	];

	fields.extend(
		canonical
			.into_iter()
			.filter_map(|(key, val)| val.map(|val| (key.to_owned(), JsonValue::String(val)))),
	);

	fields
}

/// Sets a new profile key value, removes the key if value is None
#[implement(super::Service)]
pub fn set_profile_key(
	&self,
	user_id: &UserId,
	profile_key: &str,
	profile_key_value: Option<&serde_json::Value>,
) {
	let key = (user_id, profile_key);

	if let Some(value) = profile_key_value {
		self.db
			.useridprofilekey_value
			.put(key, Json(value));
	} else {
		self.db.useridprofilekey_value.del(key);
	}
}

/// Gets a specific user profile key
#[implement(super::Service)]
pub async fn profile_key(
	&self,
	user_id: &UserId,
	profile_key: &str,
) -> Result<Raw<ProfileFieldValue>> {
	let key = (user_id, profile_key);
	self.db
		.useridprofilekey_value
		.qry(&key)
		.await
		.deserialized()
}

/// Current per-room displayname for the user, or `None` if the room has
/// no member event for them.
#[implement(super::Service)]
async fn member_displayname(&self, room_id: &RoomId, user_id: &UserId) -> Option<String> {
	self.services
		.state_accessor
		.get_member(room_id, user_id)
		.await
		.ok()
		.and_then(|m: RoomMemberEventContent| m.displayname)
}

/// Current per-room avatar_url for the user, or `None` if the room has
/// no member event for them.
#[implement(super::Service)]
async fn member_avatar_url(&self, room_id: &RoomId, user_id: &UserId) -> Option<OwnedMxcUri> {
	self.services
		.state_accessor
		.get_member(room_id, user_id)
		.await
		.ok()
		.and_then(|m: RoomMemberEventContent| m.avatar_url)
}
