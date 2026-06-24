use futures::stream::StreamExt;
use ruma::OwnedUserId;
use tokio::time::Instant;
use tuwunel_core::Result;

use crate::admin_command;

#[admin_command]
pub(super) async fn get_bindings(&self, user_id: OwnedUserId) -> Result {
	let timer = Instant::now();
	let result: Vec<_> = self
		.services
		.threepid
		.get_bindings(&user_id)
		.collect()
		.await;

	let query_time = timer.elapsed();

	write!(self, "Query completed in {query_time:?}:\n\n```rs\n{result:#?}\n```").await
}
