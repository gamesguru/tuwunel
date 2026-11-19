use std::fmt::Write as _;

use ruma::{OwnedRoomId, OwnedServerName};
use tuwunel_core::Result;
use tuwunel_service::rooms::reconcile::{CompareOutcome, Comparison};

use crate::admin_command;

const LIST_LIMIT: usize = 50;

#[admin_command]
pub(super) async fn compare(&self, room_id: OwnedRoomId, server: OwnedServerName) -> Result {
	let federation = &self.services.server.config.federation;
	if !federation.enable {
		return self.write_str("Federation is disabled.").await;
	}

	let outcome = self
		.services
		.reconcile
		.compare(&room_id, &server)
		.await?;
	let out = match outcome {
		| CompareOutcome::Compared(comparison) => render(&room_id, &server, &comparison),
		| CompareOutcome::NotEligible => {
			format!("Cannot compare {room_id} with {server}: not eligible.")
		},
		| CompareOutcome::SenderChosenEventIds => {
			format!("Cannot compare {room_id}: the room version has sender-chosen event IDs.")
		},
		| CompareOutcome::Unsupported => {
			format!("{server} does not support MSC4521 reconciliation.")
		},
	};

	self.write_str(&out).await
}

fn render(room_id: &OwnedRoomId, server: &OwnedServerName, comparison: &Comparison) -> String {
	let mut out = String::new();

	writeln!(out, "MSC4521 comparison of {room_id} with {server}").expect("String write");
	writeln!(out).expect("String write");
	writeln!(out, "Local events:  {}", comparison.local_known_event_count).expect("String write");
	writeln!(out, "Remote events: {}", comparison.remote_known_event_count)
		.expect("String write");
	writeln!(out, "Only local:    {}", comparison.missing_remotely.len()).expect("String write");
	writeln!(out, "Only remote:   {}", comparison.missing_locally.len()).expect("String write");
	writeln!(out, "Rounds:        {}", comparison.rounds).expect("String write");
	writeln!(out, "Status:        {}", status(comparison)).expect("String write");

	render_ids(
		&mut out,
		"Only remote (we lack)",
		comparison
			.missing_locally
			.iter()
			.map(ToString::to_string),
	);
	render_ids(
		&mut out,
		"Only local (remote lacks)",
		comparison
			.missing_remotely
			.iter()
			.map(ToString::to_string),
	);
	if !comparison.unresolved_event_digests.is_empty() {
		render_ids(
			&mut out,
			"Unresolved event digests",
			comparison
				.unresolved_event_digests
				.iter()
				.map(|digest| {
					digest
						.iter()
						.map(|byte| format!("{byte:02x}"))
						.collect()
				}),
		);
	}

	out
}

fn status(comparison: &Comparison) -> &'static str {
	if comparison.synchronized {
		"synchronized"
	} else if comparison.frame_mismatch {
		"frame mismatch"
	} else if comparison.capacity_exceeded {
		"incomplete (capacity exceeded)"
	} else if comparison.exhausted {
		"incomplete (round limit)"
	} else {
		"complete"
	}
}

fn render_ids(out: &mut String, title: &str, ids: impl Iterator<Item = String>) {
	let mut ids = ids.peekable();
	if ids.peek().is_none() {
		return;
	}

	writeln!(out, "\n{title}:").expect("String write");
	for id in ids.by_ref().take(LIST_LIMIT) {
		writeln!(out, "  {id}").expect("String write");
	}
	let remaining = ids.count();
	if remaining > 0 {
		writeln!(out, "  ... {remaining} more").expect("String write");
	}
}
