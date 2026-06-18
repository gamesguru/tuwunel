mod get_nonce;
mod is_user_locked;
mod is_user_suspended;
mod lock_user;
pub(crate) mod mas;
mod register;
mod suspend_user;

use futures::future::join3;
use ruma::UserId;
use tuwunel_core::{Err, Result};

pub(crate) use self::{
	get_nonce::admin_register_nonce_route, is_user_locked::is_user_locked_route,
	is_user_suspended::is_user_suspended_route, lock_user::lock_user_route,
	register::admin_register_route, suspend_user::suspend_user_route,
};

/// MSC4323: authorization is checked before account lookups
/// (anti-enumeration) per spec.
async fn authorize(services: &crate::State, caller: &UserId, target: &UserId) -> Result {
	if caller == target {
		return Err!(Request(Forbidden("You cannot suspend or lock your own account")));
	}

	if !services.globals.user_is_local(target) {
		return Err!(Request(InvalidParam("User is not local to this server")));
	}

	let (caller_admin, target_active, target_admin) = join3(
		services.admin.user_is_admin(caller),
		services.users.is_active(target),
		services.admin.user_is_admin(target),
	)
	.await;

	if !caller_admin {
		return Err!(Request(Forbidden("Only server administrators can use this endpoint")));
	}

	if !target_active {
		return Err!(Request(NotFound("Unknown user")));
	}

	if target_admin {
		return Err!(Request(Forbidden(
			"You cannot suspend or lock another server administrator"
		)));
	}

	Ok(())
}
