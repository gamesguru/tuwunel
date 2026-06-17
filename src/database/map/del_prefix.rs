use std::{fmt::Debug, sync::Arc};

use serde::Serialize;
use tuwunel_core::{
	implement,
	utils::stream::{ReadyExt, TryIgnore},
};

/// Delete every key under `prefix`. !!! USE WITH CAUTION !!!
///
/// Operates on a snapshot taken when invoked; data written during or after the
/// call may be missed. Mirrors the borrowed-cursor delete of `for_clear`.
#[implement(super::Map)]
#[tracing::instrument(level = "trace", skip(self))]
pub async fn del_prefix<P>(self: &Arc<Self>, prefix: &P)
where
	P: Serialize + ?Sized + Debug + Sync,
{
	self.keys_prefix_raw(prefix)
		.ignore_err()
		.ready_for_each(|key| self.remove(&key))
		.await;
}
