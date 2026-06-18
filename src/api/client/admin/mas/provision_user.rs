use axum::extract::State;
use ruma::MxcUri;
use synapse_admin_api::mas::provision_user::{Request, Response};
use tuwunel_core::Result;
use tuwunel_service::users::{PASSWORD_SENTINEL, propagation_default};

use super::{Mas, joined_rooms, local_user};
use crate::Ruma;

/// # `POST /_synapse/mas/provision_user`
pub(crate) async fn provision_user_route(
	_mas: Mas,
	State(services): State<crate::State>,
	body: Ruma<Request>,
) -> Result<Response> {
	let user_id = local_user(services, &body.localpart)?;
	let created = !services.users.exists(&user_id).await;

	if created {
		services
			.users
			.create(&user_id, Some(PASSWORD_SENTINEL), Some("oidc"))
			.await?;
	}

	let touch_displayname = body.set_displayname.is_some() || body.unset_displayname;
	let touch_avatar = body.set_avatar_url.is_some() || body.unset_avatar_url;
	if touch_displayname || touch_avatar {
		let rooms = joined_rooms(services, &user_id).await;
		let propagation = propagation_default(
			services
				.server
				.config
				.preserve_room_profile_overrides,
		);

		if touch_displayname {
			services
				.users
				.update_displayname(
					&user_id,
					body.set_displayname.as_deref(),
					&rooms,
					propagation,
				)
				.await;
		}

		if touch_avatar {
			let avatar = body
				.set_avatar_url
				.as_deref()
				.map(<&MxcUri>::from);

			services
				.users
				.update_avatar_url(&user_id, avatar, None, &rooms, propagation)
				.await;
		}
	}

	// No 3PID store; MAS treats email as advisory, so set/unset_emails are a no-op.
	match body.locked {
		| Some(true) => services
			.users
			.set_locked(&user_id, &services.globals.server_user),
		| Some(false) => services.users.clear_locked(&user_id),
		| None => {},
	}

	Ok(Response::new(created))
}
