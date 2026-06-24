use std::sync::Arc;

use futures::{Stream, StreamExt, TryFutureExt, future::Either};
use ruma::{
	EventId, RoomId, UserId,
	api::Direction,
	events::{reaction::ReactionEventContent, relation::RelationType},
};
use tuwunel_core::{
	PduId, Result,
	arrayvec::ArrayVec,
	implement, is_equal_to,
	matrix::{Event, Pdu, PduCount, RawPduId, event::RelationTypeEqual},
	result::LogErr,
	trace,
	utils::{
		stream::{ReadyExt, TryIgnore, WidebandExt},
		u64_from_u8,
	},
};
use tuwunel_database::{Interfix, Map};

use crate::rooms::short::ShortRoomId;

pub struct Service {
	services: Arc<crate::services::OnceServices>,
	db: Data,
}

struct Data {
	tofrom_relation: Arc<Map>,
	referencedevents: Arc<Map>,
	softfailedeventids: Arc<Map>,
}

impl crate::Service for Service {
	fn build(args: &crate::Args<'_>) -> Result<Arc<Self>> {
		Ok(Arc::new(Self {
			services: args.services.clone(),
			db: Data {
				tofrom_relation: args.db["tofrom_relation"].clone(),
				referencedevents: args.db["referencedevents"].clone(),
				softfailedeventids: args.db["softfailedeventids"].clone(),
			},
		}))
	}

	fn name(&self) -> &str { crate::service::make_name(std::module_path!()) }
}

#[implement(Service)]
#[tracing::instrument(skip(self, from, to), level = "debug")]
pub fn add_relation(&self, from: PduCount, to: PduCount) {
	const BUFSIZE: usize = size_of::<u64>() * 2;

	match (from, to) {
		| (PduCount::Normal(from), PduCount::Normal(to)) => {
			let key: &[u64] = &[to, from];
			self.db
				.tofrom_relation
				.aput_raw::<BUFSIZE, _, _>(key, []);
		},
		| _ => {}, // TODO: Relations with backfilled pdus
	}
}

/// Query relations of an event to determine if matching any of the trailing
/// arguments. When all criteria are None the mere presence of a relation causes
/// this function to return true.
#[implement(Service)]
pub async fn event_has_relation(
	&self,
	event_id: &EventId,
	user_id: Option<&UserId>,
	rel_type: Option<&RelationType>,
	key: Option<&str>,
) -> bool {
	let Ok(pdu_id) = self.services.timeline.get_pdu_id(event_id).await else {
		return false;
	};

	self.has_relation(pdu_id.into(), user_id, rel_type, key)
		.await
}

/// Query relations of an event by PduId to determine if matching any of the
/// trailing arguments. When all criteria are None the mere presence of a
/// relation causes this function to return true.
#[implement(Service)]
pub async fn has_relation(
	&self,
	target: PduId,
	user_id: Option<&UserId>,
	rel_type: Option<&RelationType>,
	key: Option<&str>,
) -> bool {
	self.get_relations(target.shortroomid, target.count, None, Direction::Forward, None)
		.ready_filter(|(_, pdu)| user_id.is_none_or(is_equal_to!(pdu.sender())))
		.ready_filter(|(_, pdu)| {
			debug_assert!(
				key.is_none() || rel_type.is_none_or(is_equal_to!(&RelationType::Annotation)),
				"key argument only applies to Annotation type relations."
			);

			// When key is supplied we don't need to double-parse the content here and below.
			key.is_some() || rel_type
				.is_none_or(|rel_type| rel_type.relation_type_equal(&pdu))
		})
		.ready_filter(|(_, pdu)| {
			key.is_none_or(|key| {
				pdu.get_content::<ReactionEventContent>()
					.map(|content| content.relates_to.key == key)
					.unwrap_or(false)
			})
		})
		.ready_any(|_| true) // first match or false
		.await
}

#[implement(Service)]
pub fn get_relations<'a>(
	&'a self,
	shortroomid: ShortRoomId,
	target: PduCount,
	from: Option<PduCount>,
	dir: Direction,
	user_id: Option<&'a UserId>,
) -> impl Stream<Item = (PduCount, Pdu)> + Send + '_ {
	let target = target.to_be_bytes();
	let from = from
		.map(|from| from.saturating_inc(dir))
		.unwrap_or_else(|| match dir {
			| Direction::Backward => PduCount::max(),
			| Direction::Forward => PduCount::default(),
		})
		.to_be_bytes();

	let mut buf = ArrayVec::<u8, 16>::new();
	let start = {
		buf.extend(target);
		buf.extend(from);
		buf.as_slice()
	};

	match dir {
		| Direction::Backward => Either::Left(self.db.tofrom_relation.rev_raw_keys_from(start)),
		| Direction::Forward => Either::Right(self.db.tofrom_relation.raw_keys_from(start)),
	}
	.ignore_err()
	.ready_take_while(move |key| key.starts_with(&target))
	.map(|to_from| u64_from_u8(&to_from[8..16]))
	.map(PduCount::from_unsigned)
	.map(move |count| (user_id, shortroomid, count))
	.wide_filter_map(async |(user_id, shortroomid, count)| {
		let pdu_id: RawPduId = PduId { shortroomid, count }.into();
		self.services
			.timeline
			.get_pdu_from_id(&pdu_id)
			.map_ok(move |mut pdu| {
				if user_id.is_none_or(|user_id| pdu.sender() != user_id) {
					pdu.as_mut_pdu()
						.remove_transaction_id()
						.log_err()
						.ok();
				}

				(count, pdu)
			})
			.await
			.ok()
	})
}

/// Fold read-time bundled aggregations into a served event's `unsigned`,
/// per-requester. Currently the MSC3816 thread-participation correction: the
/// stored `m.thread` bundle carries a shared `current_user_participated`, so
/// the flag is recomputed for `sender_user` on the way out. The presence gate
/// keeps the common no-bundle case to a substring scan; the authoritative check
/// happens on mutate.
#[implement(Service)]
pub async fn bundle_aggregations(&self, sender_user: &UserId, mut pdu: Pdu) -> Pdu {
	let has_thread = pdu
		.unsigned()
		.is_some_and(|unsigned| unsigned.get().contains("m.thread"));

	if !has_thread {
		return pdu;
	}

	let participated = self
		.services
		.threads
		.user_participated(pdu.event_id(), sender_user)
		.await;

	pdu.set_thread_participated(participated)
		.log_err()
		.ok();

	pdu
}

#[implement(Service)]
#[tracing::instrument(skip_all, level = "debug")]
pub fn mark_as_referenced<'a, I>(&self, room_id: &RoomId, event_ids: I)
where
	I: Iterator<Item = &'a EventId>,
{
	for prev in event_ids {
		let key = (room_id, prev);
		self.db.referencedevents.put_raw(key, []);
	}
}

#[implement(Service)]
#[tracing::instrument(skip(self), level = "debug", ret)]
pub async fn is_event_referenced(&self, room_id: &RoomId, event_id: &EventId) -> bool {
	let key = (room_id, event_id);
	self.db.referencedevents.qry(&key).await.is_ok()
}

#[implement(Service)]
#[tracing::instrument(skip(self), level = "debug")]
pub fn mark_event_soft_failed(&self, event_id: &EventId) {
	self.db.softfailedeventids.insert(event_id, []);
}

#[implement(Service)]
#[tracing::instrument(skip(self), level = "debug", ret)]
pub async fn is_event_soft_failed(&self, event_id: &EventId) -> bool {
	self.db
		.softfailedeventids
		.get(event_id)
		.await
		.is_ok()
}

#[implement(Service)]
#[tracing::instrument(skip(self), level = "debug")]
pub async fn delete_all_referenced_for_room(&self, room_id: &RoomId) -> Result {
	let prefix = (room_id, Interfix);

	self.db
		.referencedevents
		.keys_prefix_raw(&prefix)
		.ignore_err()
		.ready_for_each(|key| {
			trace!(?key, "Removing key");
			self.db.referencedevents.remove(key);
		})
		.await;

	Ok(())
}
