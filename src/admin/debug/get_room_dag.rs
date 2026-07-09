use std::{collections::HashSet, fmt::Write};

use ruma::{CanonicalJsonObject, OwnedEventId, OwnedRoomOrAliasId, events::TimelineEventType};
use serde_json::Value as JsonValue;
use tokio::io::AsyncWriteExt;
use tuwunel_core::{Result, err, matrix::PduEvent, warn};

use crate::admin_command;

pub(super) struct DagExportStats {
	pub count: u64,
	pub total_prev_events: u64,
	pub state_events: u64,
	pub missing_hash: u64,
	pub unique_hashes: HashSet<u64>,
	pub last_ssh: Option<u64>,
	pub last_is_state_event: bool,
	pub last_event_id: Option<OwnedEventId>,
	pub last_event_type: Option<TimelineEventType>,
	pub last_state_key: Option<String>,
	pub max_depth: u64,
	pub min_depth: u64,
	pub all_event_ids: HashSet<OwnedEventId>,
	pub referenced_as_prev: HashSet<OwnedEventId>,
}

impl Default for DagExportStats {
	fn default() -> Self {
		Self {
			count: 0,
			total_prev_events: 0,
			state_events: 0,
			missing_hash: 0,
			unique_hashes: HashSet::new(),
			last_ssh: None,
			last_is_state_event: false,
			last_event_id: None,
			last_event_type: None,
			last_state_key: None,
			max_depth: 0,
			min_depth: u64::MAX,
			all_event_ids: HashSet::new(),
			referenced_as_prev: HashSet::new(),
		}
	}
}

pub(super) async fn decorate_pdu_for_export(
	ctx: &crate::Context<'_>,
	pdu_json: &CanonicalJsonObject,
	pdu_opt: Option<&PduEvent>,
	is_outlier: bool,
) -> Result<(serde_json::Map<String, JsonValue>, bool, Option<u64>)> {
	let mut obj: serde_json::Map<String, JsonValue> =
		serde_json::from_value(serde_json::to_value(pdu_json)?)?;

	if is_outlier {
		obj.insert("__outlier".to_owned(), JsonValue::Bool(true));
	}

	let mut is_separated = is_outlier;
	let mut shortstatehash = None;

	if let Some(pdu) = pdu_opt {
		obj.insert("event_id".to_owned(), JsonValue::String(pdu.event_id.to_string()));
		let is_soft_failed = ctx
			.services
			.pdu_metadata
			.is_event_soft_failed(&pdu.event_id)
			.await;
		if is_soft_failed {
			obj.insert("__soft_failed".to_owned(), JsonValue::Bool(true));
			is_separated = true;
		}

		if !is_separated
			&& let Ok(ssh) = ctx
				.services
				.state
				.pdu_shortstatehash(&pdu.event_id)
				.await
		{
			obj.insert("__shortstatehash".to_owned(), JsonValue::from(ssh));
			shortstatehash = Some(ssh);
		}
	} else {
		is_separated = true;
	}

	Ok((obj, is_separated, shortstatehash))
}

impl DagExportStats {
	#[allow(clippy::too_many_arguments)]
	pub(super) async fn process_and_write_pdu(
		&mut self,
		ctx: &crate::Context<'_>,
		file: &mut tokio::fs::File,
		outliers_file: &mut tokio::fs::File,
		pdu_json: CanonicalJsonObject,
		pdu_result: Result<PduEvent>,
		is_outlier: bool,
		print: bool,
	) -> Result<()> {
		let pdu_opt = pdu_result.as_ref().ok();
		let (obj, is_separated, shortstatehash) =
			decorate_pdu_for_export(ctx, &pdu_json, pdu_opt, is_outlier).await?;

		if let Ok(pdu) = &pdu_result
			&& !is_separated
		{
			if let Some(ssh) = shortstatehash {
				self.unique_hashes.insert(ssh);
				self.last_ssh = Some(ssh);
			} else {
				self.missing_hash = self.missing_hash.saturating_add(1);
			}

			if pdu.state_key.is_some() {
				self.state_events = self.state_events.saturating_add(1);
				self.last_is_state_event = true;
				self.last_event_type = Some(pdu.kind.clone());
				self.last_state_key = pdu.state_key.as_ref().map(ToString::to_string);
			} else {
				self.last_is_state_event = false;
			}

			self.last_event_id = Some(pdu.event_id.clone());
			let eid = pdu.event_id.clone();
			self.all_event_ids.insert(eid);
			for prev in &pdu.prev_events {
				self.referenced_as_prev.insert(prev.clone());
			}
			let d: u64 = pdu.depth.into();
			self.max_depth = self.max_depth.max(d);
			self.min_depth = self.min_depth.min(d);
		}

		let json = serde_json::to_string(&obj)?;

		if is_separated {
			outliers_file.write_all(json.as_bytes()).await?;
			outliers_file.write_all(b"\n").await?;
		} else {
			file.write_all(json.as_bytes()).await?;
			file.write_all(b"\n").await?;
			self.count = self.count.saturating_add(1);
			if let Ok(pdu) = &pdu_result {
				self.total_prev_events = self
					.total_prev_events
					.saturating_add(u64::try_from(pdu.prev_events.len()).unwrap_or(0));
			}
		}

		if print {
			ctx.write_str(&format!("{json}\n")).await?;
		}

		Ok(())
	}
}

async fn collect_pdu_ids(
	services: &tuwunel_service::Services,
	room_id: &ruma::RoomId,
) -> Vec<OwnedEventId> {
	use futures::StreamExt;

	let pdu_stream = services.timeline.pdus(None, room_id, None);
	futures::pin_mut!(pdu_stream);
	let mut pdu_ids = Vec::new();
	while let Some(res) = pdu_stream.next().await {
		if let Ok((_, pdu)) = res {
			pdu_ids.push(pdu.event_id.clone());
		}
	}
	pdu_ids
}

async fn collect_outlier_ids(
	services: &tuwunel_service::Services,
	room_id: &ruma::RoomId,
) -> Vec<OwnedEventId> {
	use futures::StreamExt;

	let mut outlier_ids = Vec::new();
	let outlier_stream = services.timeline.outlier_pdus_raw();
	futures::pin_mut!(outlier_stream);
	while let Some(res) = outlier_stream.next().await {
		if let Ok(data) = res
			&& let Ok(pdu) = serde_json::from_slice::<PduEvent>(data)
			&& pdu.room_id == *room_id
		{
			outlier_ids.push(pdu.event_id.clone());
		}
	}
	outlier_ids
}

async fn determine_tip_match(
	services: &tuwunel_service::Services,
	stats: &DagExportStats,
	room_ssh: Option<u64>,
) -> String {
	match (stats.last_ssh, room_ssh) {
		| (Some(tip), Some(room)) if tip == room => "✓ tip matches room state".to_owned(),
		| (Some(tip), Some(room)) if stats.last_is_state_event => {
			if let (Some(last_eid), Some(last_type), Some(last_sk)) =
				(&stats.last_event_id, &stats.last_event_type, &stats.last_state_key)
			{
				let room_has_tip = services
					.state_accessor
					.state_get_id(room, &last_type.to_string().into(), last_sk)
					.await
					.is_ok_and(|eid| eid == *last_eid);

				if room_has_tip {
					format!(
						"✓ tip is state event — room state includes tip (pre={tip} post={room})"
					)
				} else {
					format!(
						"✗ tip DIVERGES — room state at ({last_type}, {last_sk}) does not point \
						 to tip event {last_eid}"
					)
				}
			} else {
				"✗ tip DIVERGES from room state (state event but missing metadata)".to_owned()
			}
		},
		| (Some(_tip), Some(_room)) => "✗ tip DIVERGES from room state".to_owned(),
		| _ => "? unknown".to_owned(),
	}
}

fn format_dag_stats_output(
	stats: &DagExportStats,
	room_ssh: Option<u64>,
	tip_match: &str,
	display_path: &str,
) -> String {
	let heads_count = stats
		.all_event_ids
		.difference(&stats.referenced_as_prev)
		.count();

	let (bf_whole, bf_frac) = if stats.count > 0 {
		let scaled = stats
			.total_prev_events
			.saturating_mul(1000)
			.checked_div(stats.count)
			.unwrap_or(0);
		(scaled.checked_div(1000).unwrap_or(0), scaled % 1000)
	} else {
		(0, 0)
	};

	let mut out = format!("Wrote {count} PDUs to {display_path}\n", count = stats.count);
	writeln!(out, "```").expect("fmt");
	writeln!(out, "PDUs:           {count}", count = stats.count).expect("fmt");
	writeln!(out, "State events:   {state_events}", state_events = stats.state_events)
		.expect("fmt");
	writeln!(out, "Branching:      {bf_whole}.{bf_frac:03} avg prev_events/PDU").expect("fmt");

	let (frag_whole, frag_frac) = if stats.max_depth > 0 {
		let scaled = stats
			.count
			.saturating_mul(1000)
			.checked_div(stats.max_depth)
			.unwrap_or(0);
		(scaled.checked_div(1000).unwrap_or(0), scaled % 1000)
	} else {
		(0, 0)
	};

	writeln!(
		out,
		"Frag factor:    {frag_whole}.{frag_frac:03} ({count} events / {max_depth} depth, \
		 {heads_count} heads)",
		count = stats.count,
		max_depth = stats.max_depth
	)
	.expect("fmt");

	writeln!(out, "Unique states:  {}", stats.unique_hashes.len()).expect("fmt");
	writeln!(out, "Missing hash:   {missing_hash}", missing_hash = stats.missing_hash)
		.expect("fmt");

	if let Some(tip) = stats.last_ssh {
		writeln!(out, "Tip SSH:        {tip}").expect("fmt");
	}
	if let Some(room) = room_ssh {
		writeln!(out, "Room SSH:       {room}").expect("fmt");
	}
	writeln!(out, "Status:         {tip_match}").expect("fmt");
	writeln!(out, "```").expect("fmt");

	out
}

#[admin_command]
pub(super) async fn get_room_dag(
	&self,
	room_id: OwnedRoomOrAliasId,
	start: i64,
	end: i64,
	print: bool,
	outliers: bool,
) -> Result {
	let room_id = self
		.services
		.alias
		.maybe_resolve(&room_id)
		.await?;

	let pdu_ids = collect_pdu_ids(self.services, &room_id).await;

	let actual_start = if start < 0 {
		let offset = usize::try_from(start.unsigned_abs()).unwrap_or(usize::MAX);
		u64::try_from(pdu_ids.len().saturating_sub(offset)).unwrap_or(u64::MAX)
	} else {
		start.unsigned_abs()
	};

	let mut i = 0_u64;
	let mut stats = DagExportStats::default();
	let server = self.services.globals.server_name();
	let room_version_str = self
		.services
		.state
		.get_room_version(&room_id)
		.await
		.map_or_else(|_| "unknown".to_owned(), |v| v.to_string());
	let safe_room_id = room_id
		.to_string()
		.replace('!', "")
		.replace(':', "_");

	let tmp_dir = std::env::temp_dir();
	let path =
		tmp_dir.join(format!("local-dag-{safe_room_id}-v{room_version_str}-{server}.jsonl"));
	let mut file = tokio::fs::File::create(&path)
		.await
		.map_err(|e| err!(Database("Failed to create file {path:?}: {e:?}")))?;

	let outliers_path = tmp_dir
		.join(format!("local-dag-{safe_room_id}-v{room_version_str}-{server}-outliers.jsonl"));
	let mut outliers_file = tokio::fs::File::create(&outliers_path)
		.await
		.map_err(|e| err!(Database("Failed to create outliers file {outliers_path:?}: {e:?}")))?;

	for event_id in pdu_ids {
		if let Ok(end_val) = u64::try_from(end)
			&& i > end_val
		{
			break;
		}
		if i >= actual_start
			&& let Ok(pdu_json) = self
				.services
				.timeline
				.get_pdu_json(&event_id)
				.await
		{
			let pdu_result = self.services.timeline.get_pdu(&event_id).await;
			if let Err(e) = stats
				.process_and_write_pdu(
					self,
					&mut file,
					&mut outliers_file,
					pdu_json,
					pdu_result,
					false,
					print,
				)
				.await
			{
				warn!("Failed to process PDU {event_id}: {e}");
			}
		}
		i = i.saturating_add(1);
	}

	if outliers {
		let outlier_ids = collect_outlier_ids(self.services, &room_id).await;

		for event_id in outlier_ids {
			if let Ok(pdu_json) = self
				.services
				.timeline
				.get_outlier_pdu_json(&event_id)
				.await
			{
				let pdu_result = self
					.services
					.timeline
					.get_outlier_pdu(&event_id)
					.await;
				if let Err(e) = stats
					.process_and_write_pdu(
						self,
						&mut file,
						&mut outliers_file,
						pdu_json,
						pdu_result,
						true,
						print,
					)
					.await
				{
					warn!("Failed to process outlier PDU {event_id}: {e}");
				}
			}
		}
	}

	let room_ssh = self
		.services
		.state
		.get_room_shortstatehash(&room_id)
		.await
		.ok();

	let tip_match = determine_tip_match(self.services, &stats, room_ssh).await;

	let min_d = if stats.min_depth == u64::MAX {
		0
	} else {
		stats.min_depth
	};
	let final_path = tmp_dir.join(format!(
		"local-dag-{safe_room_id}-v{room_version_str}-{server}-d{min_d}-{max_depth}.jsonl",
		max_depth = stats.max_depth
	));
	if let Err(e) = tokio::fs::rename(&path, &final_path).await {
		warn!("Failed to rename {path:?} -> {final_path:?}: {e}");
	}
	let display_path = if tokio::fs::metadata(&final_path).await.is_ok() {
		final_path.to_string_lossy().into_owned()
	} else {
		path.to_string_lossy().into_owned()
	};

	let out = format_dag_stats_output(&stats, room_ssh, &tip_match, &display_path);
	self.write_str(&out).await
}
