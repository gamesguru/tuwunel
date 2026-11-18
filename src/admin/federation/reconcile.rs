use ruma::{OwnedRoomId, OwnedServerName};
use tuwunel_core::Result;

use crate::admin_command;

#[admin_command]
pub(super) async fn reconcile(&self, room_id: OwnedRoomId, server: OwnedServerName) -> Result {
	let federation = &self.services.server.config.federation;
	if !federation.enable || !federation.msc4521_enabled {
		return self
			.write_str("MSC4521 reconciliation is disabled.")
			.await;
	}

	match self
		.services
		.reconcile
		.reconcile_now(&room_id, &server)
		.await
	{
		| Ok(true) =>
			self.write_str(&format!("Reconciliation of {room_id} with {server} finished."))
				.await,
		| Ok(false) =>
			self.write_str(&format!("{server} does not support MSC4521 reconciliation."))
				.await,
		| Err(error) =>
			self.write_str(&format!("Reconciliation failed: {error}"))
				.await,
	}
}
