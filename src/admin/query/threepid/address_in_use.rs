use tokio::time::Instant;
use tuwunel_core::Result;
use tuwunel_service::threepid::canonicalize_email;

use crate::admin_command;

#[admin_command]
pub(super) async fn address_in_use(&self, address: String) -> Result {
	let email_canon = canonicalize_email(&address)?;

	let timer = Instant::now();
	let result = self
		.services
		.threepid
		.address_in_use(&email_canon)
		.await;

	let query_time = timer.elapsed();

	write!(self, "Query completed in {query_time:?}:\n\n```rs\n{result:#?}\n```").await
}
