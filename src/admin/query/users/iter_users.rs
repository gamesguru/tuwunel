use futures::stream::StreamExt;
use ruma::OwnedUserId;
use tuwunel_core::Result;

use crate::admin_command;

#[admin_command]
pub(super) async fn iter_users(&self) -> Result {
	let query = self
		.services
		.users
		.stream()
		.map(Into::into)
		.collect::<Vec<OwnedUserId>>();

	self.write_timed_query(query).await
}
