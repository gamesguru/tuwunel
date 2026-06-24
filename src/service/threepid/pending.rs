use std::time::{Duration, SystemTime};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as b64encode};
use ruma::thirdparty::Medium;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tuwunel_core::{
	Err, Result, implement,
	utils::{self, hash::sha256},
};
use tuwunel_database::{Cbor, Deserialized};

use super::Association;

/// Characters minted for the single-use, server-private validation token.
const TOKEN_LENGTH: usize = 48;

/// Failed-validation ceiling: the session self-destructs once this many wrong
/// submissions have been counted, so the Nth burns and N-1 are tolerated. Caps
/// token brute-force (mirrors the device-grant ceiling).
const MAX_VERIFY_ATTEMPTS: u32 = 5;

/// CBOR value of a `threepidsid_pending` row. The whole row carries a TTL via
/// `expires_at` so a validated-but-unconsumed session self-reaps rather than
/// leaking.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Pending {
	client_secret: String,
	medium: Medium,
	address: String,
	token: String,
	send_attempt: u64,
	attempts: u32,
	validated_at: Option<SystemTime>,
	expires_at: Option<SystemTime>,
}

/// Result of [`create_or_reuse_pending`]: the session id to hand the client,
/// and the freshly minted token when a new message must be sent. A reused
/// session yields `None`, signalling no new mail.
#[derive(Clone, Debug)]
pub struct PendingOutcome {
	pub sid: String,
	pub freshly_minted_token: Option<String>,
}

/// Open a pending verification, or reuse an in-flight one for the same
/// request identity. The session id is derived from `(medium, address,
/// client_secret)`, so a resubmit collides on the same row: a non-validated
/// session whose `send_attempt` did not advance returns the same `sid` with no
/// new token (and thus no new mail), per the send-attempt dedup rule.
#[implement(super::Service)]
#[tracing::instrument(level = "debug", skip(self, client_secret))]
pub async fn create_or_reuse_pending(
	&self,
	client_secret: &str,
	medium: Medium,
	address: &str,
	send_attempt: u64,
	ttl: Duration,
) -> Result<PendingOutcome> {
	let sid = derive_sid(&medium, address, client_secret);

	if let Ok(existing) = self.get_pending(&sid).await
		&& existing.validated_at.is_none()
		&& send_attempt <= existing.send_attempt
	{
		return Ok(PendingOutcome { sid, freshly_minted_token: None });
	}

	let token = utils::random_string(TOKEN_LENGTH);
	let expires_at = SystemTime::now().checked_add(ttl);
	let pending = Pending {
		client_secret: client_secret.to_owned(),
		medium,
		address: address.to_owned(),
		token: token.clone(),
		send_attempt,
		attempts: 0,
		validated_at: None,
		expires_at,
	};

	self.persist_pending(&sid, &pending);

	Ok(PendingOutcome { sid, freshly_minted_token: Some(token) })
}

/// Validate a submitted token against a pending session. A wrong
/// `client_secret` or `token` counts toward the attempt ceiling and burns the
/// session once exceeded; the caller learns nothing about session or token
/// liveness beyond pass or fail.
#[implement(super::Service)]
#[tracing::instrument(level = "debug", skip(self, client_secret, token))]
pub async fn validate_pending_token(
	&self,
	sid: &str,
	client_secret: &str,
	token: &str,
) -> Result<()> {
	let pending = self.get_pending(sid).await?;

	if expired(&pending) {
		self.delete_pending(sid);

		return Err!(Request(NotFound("The verification session has expired")));
	}

	let secret_ok = ct_eq(&pending.client_secret, client_secret);
	let token_ok = ct_eq(&pending.token, token);

	if !secret_ok || !token_ok {
		let attempts = pending.attempts.saturating_add(1);
		match attempts >= MAX_VERIFY_ATTEMPTS {
			| true => self.delete_pending(sid),
			| false => self.persist_pending(sid, &Pending { attempts, ..pending }),
		}

		return Err!(Request(ThreepidAuthFailed("Invalid verification token")));
	}

	let validated_at = Some(SystemTime::now());
	self.persist_pending(sid, &Pending { validated_at, ..pending });

	Ok(())
}

/// Consume a validated pending session for the add flow, returning the
/// validated `(medium, address)` and deleting the row. Errors if the session
/// is unknown, unvalidated, expired, or the `client_secret` does not match.
#[implement(super::Service)]
#[tracing::instrument(level = "debug", skip(self, client_secret))]
pub async fn consume_validated(&self, sid: &str, client_secret: &str) -> Result<Association> {
	let pending = self.get_pending(sid).await?;

	if expired(&pending) {
		self.delete_pending(sid);

		return Err!(Request(NotFound("The verification session has expired")));
	}

	if !ct_eq(&pending.client_secret, client_secret) {
		return Err!(Request(ThreepidAuthFailed("Client secret does not match")));
	}

	if pending.validated_at.is_none() {
		return Err!(Request(ThreepidAuthFailed("The address has not been validated")));
	}

	self.delete_pending(sid);

	Ok(Association {
		medium: pending.medium,
		address: pending.address,
	})
}

/// Whether a pending session exists, is unexpired, matches `client_secret`, and
/// has been validated; a non-consuming gate for the registration UIA. Wrong
/// secret, expired, unknown, or unvalidated all read as `false`, so the caller
/// learns nothing about session liveness beyond the gate result.
#[implement(super::Service)]
#[tracing::instrument(level = "debug", skip(self, client_secret))]
pub async fn session_validated(&self, sid: &str, client_secret: &str) -> bool {
	let Ok(pending) = self.get_pending(sid).await else {
		return false;
	};

	!expired(&pending)
		&& ct_eq(&pending.client_secret, client_secret)
		&& pending.validated_at.is_some()
}

#[implement(super::Service)]
fn persist_pending(&self, sid: &str, pending: &Pending) {
	self.db
		.threepidsid_pending
		.raw_put(sid, Cbor(pending));
}

/// Blind-delete a pending session row.
#[implement(super::Service)]
#[tracing::instrument(level = "debug", skip(self))]
pub fn delete_pending(&self, sid: &str) { self.db.threepidsid_pending.remove(sid); }

#[implement(super::Service)]
async fn get_pending(&self, sid: &str) -> Result<Pending> {
	self.db
		.threepidsid_pending
		.get(sid)
		.await
		.deserialized::<Cbor<_>>()
		.map(|Cbor(pending)| pending)
}

/// Deterministic session id binding the request identity to one storage key.
fn derive_sid(medium: &Medium, address: &str, client_secret: &str) -> String {
	let parts = [medium.as_str().as_bytes(), address.as_bytes(), client_secret.as_bytes()];
	let digest = sha256::delimited(parts.into_iter());

	b64encode.encode(digest)
}

fn expired(pending: &Pending) -> bool {
	pending
		.expires_at
		.is_some_and(|expires_at| SystemTime::now() > expires_at)
}

fn ct_eq(a: &str, b: &str) -> bool { a.as_bytes().ct_eq(b.as_bytes()).into() }
