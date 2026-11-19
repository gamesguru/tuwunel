//! On-demand MSC4521 view of a room's accepted event set.
//!
//! `h64` and `q3` are derived from reference-hash event IDs, so no database
//! migration or change to the timeline write path is needed. The snapshot
//! keeps the accumulator, strata, sorted sketch population, and event lookup
//! index together so callers cannot combine different scans.

use std::collections::{BTreeMap, HashSet};

use futures::{StreamExt, TryStreamExt};
use rezzy_recon::{
	AlgebraicError, EventIdFormat, FixedWindowElementHash, FixedWindowPopulation, ResidentKernel,
};
use ruma::{OwnedEventId, RoomId, RoomVersionId};
use tuwunel_core::{Result, err};

use super::Service;

/// A point-in-time reconciliation view built from accepted timeline events.
#[derive(Debug)]
pub struct RoomReconciliationState {
	/// Accumulator and strata for this event set.
	pub resident: ResidentKernel,
	/// Sorted full digests used to build GF(2^256) bucket sketches.
	pub population: FixedWindowPopulation,
	/// Full event IDs corresponding to each digest.
	pub digest_to_events: BTreeMap<[u8; 32], Vec<(OwnedEventId, FixedWindowElementHash)>>,
	/// Event ID encoding selected by the room version.
	pub event_id_format: EventIdFormat,
	/// Forward extremities observed around the scan.
	pub frame_event_ids: Vec<OwnedEventId>,
	/// Inclusive minimum and maximum event depth.
	pub depth_range: [u64; 2],
	/// Inclusive minimum and maximum origin timestamp.
	pub origin_server_ts_range: [u64; 2],
}

/// Maps supported Matrix room versions to their event ID encoding.
#[must_use]
pub fn event_id_format(room_version: &RoomVersionId) -> Option<EventIdFormat> {
	match room_version {
		| RoomVersionId::V1 | RoomVersionId::V2 => None,
		| RoomVersionId::V3 => Some(EventIdFormat::V3),
		| _ => Some(EventIdFormat::V4Plus),
	}
}

#[derive(Default)]
struct Builder {
	resident: ResidentKernel,
	elements: Vec<FixedWindowElementHash>,
	digest_to_events: BTreeMap<[u8; 32], Vec<(OwnedEventId, FixedWindowElementHash)>>,
	seen: HashSet<OwnedEventId>,
	depth_range: Option<[u64; 2]>,
	ts_range: Option<[u64; 2]>,
}

impl Builder {
	fn push(
		&mut self,
		event_id: OwnedEventId,
		depth: u64,
		timestamp: u64,
		format: EventIdFormat,
	) -> std::result::Result<(), AlgebraicError> {
		if !self.seen.insert(event_id.clone()) {
			return Ok(());
		}

		let hash = FixedWindowElementHash::from_matrix_event_id(event_id.as_str(), format)?;
		self.resident.insert(hash)?;
		self.elements.push(hash);
		let digest = hash.digest();
		self.digest_to_events
			.entry(digest)
			.or_default()
			.push((event_id, hash));

		let [min, max] = self.depth_range.get_or_insert([depth, depth]);
		*min = (*min).min(depth);
		*max = (*max).max(depth);
		let [min, max] = self
			.ts_range
			.get_or_insert([timestamp, timestamp]);
		*min = (*min).min(timestamp);
		*max = (*max).max(timestamp);
		Ok(())
	}

	fn finish(
		self,
		frame_event_ids: Vec<OwnedEventId>,
		event_id_format: EventIdFormat,
	) -> std::result::Result<RoomReconciliationState, AlgebraicError> {
		Ok(RoomReconciliationState {
			resident: self.resident,
			population: FixedWindowPopulation::new(self.elements)?,
			digest_to_events: self.digest_to_events,
			event_id_format,
			frame_event_ids,
			depth_range: self.depth_range.unwrap_or([0, 0]),
			origin_server_ts_range: self.ts_range.unwrap_or([0, 0]),
		})
	}
}

impl Service {
	/// Scans accepted events into a reconciliation snapshot.
	///
	/// Rooms with sender-chosen event IDs (versions 1 and 2) cannot use this
	/// profile. The scan retries up to three times when forward extremities move.
	pub async fn reconciliation_state(
		&self,
		room_id: &RoomId,
	) -> Result<RoomReconciliationState> {
		let room_version = self
			.services
			.state
			.get_room_version(room_id)
			.await?;
		let format = event_id_format(&room_version).ok_or_else(|| {
			err!(Request(UnsupportedRoomVersion("Room version has sender-chosen event IDs.")))
		})?;

		for attempt in 0..3 {
			let frame_before = self.forward_extremities_sorted(room_id).await;
			let mut builder = Builder::default();
			let pdus = self.pdus(None, room_id, None);
			futures::pin_mut!(pdus);
			while let Some((_, pdu)) = pdus.try_next().await? {
				builder
					.push(
						pdu.event_id,
						u64::from(pdu.depth),
						u64::from(pdu.origin_server_ts),
						format,
					)
					.map_err(|e| err!(Database("building reconciliation snapshot: {e:?}")))?;
			}

			let frame_after = self.forward_extremities_sorted(room_id).await;
			if frame_before == frame_after || attempt == 2 {
				return builder
					.finish(frame_after, format)
					.map_err(|e| err!(Database("building reconciliation population: {e:?}")));
			}
		}

		unreachable!("the bounded reconciliation scan always returns")
	}

	async fn forward_extremities_sorted(&self, room_id: &RoomId) -> Vec<OwnedEventId> {
		let mut ids: Vec<OwnedEventId> = self
			.services
			.state
			.get_forward_extremities(room_id)
			.map(OwnedEventId::from)
			.collect()
			.await;
		ids.sort_unstable();
		ids.dedup();
		ids
	}
}
