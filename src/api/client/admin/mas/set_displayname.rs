use axum::extract::State;
use synapse_admin_api::mas::set_displayname::{Request, Response};
use tuwunel_core::Result;
use tuwunel_service::users::propagation_default;

use super::{Mas, existing_user, joined_rooms};
use crate::Ruma;

/// # `POST /_synapse/mas/set_displayname`
pub(crate) async fn set_displayname_route(
	_mas: Mas,
	State(services): State<crate::State>,
	body: Ruma<Request>,
) -> Result<Response> {
	let user_id = existing_user(services, &body.localpart).await?;
	let rooms = joined_rooms(services, &user_id).await;
	let propagation = propagation_default(
		services
			.server
			.config
			.preserve_room_profile_overrides,
	);

	services
		.users
		.update_displayname(&user_id, Some(&body.displayname), &rooms, propagation)
		.await;

	Ok(Response::new())
}
