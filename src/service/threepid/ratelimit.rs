use std::{hash::Hash, net::IpAddr, time::Instant};

use http::StatusCode;
use ruma::api::error::{ErrorKind, LimitExceededErrorData};
use tuwunel_core::{Error, Result, implement};

use super::Ratelimiter;

/// Refills per second on each requestToken bucket; a generous burst absorbs a
/// real client's retries while bounding sustained spray.
const RC_PER_SECOND: f64 = 0.2;
const RC_BURST: f64 = 5.0;

/// Cap on each bucket table; fully refilled buckets are pruned past it so a
/// spray cannot grow the table without bound.
const RATELIMIT_MAP_CAP: usize = 1 << 16;

/// Per-caller-IP requestToken throttle, the axis bounding one source spraying
/// many addresses.
#[implement(super::Service)]
pub fn check_ip_rate_limit(&self, client: IpAddr) -> Result {
	check_bucket(&self.ip_ratelimiter, client, RC_PER_SECOND, RC_BURST)
}

/// Per-target-address requestToken throttle, the axis bounding many sources
/// spraying one address.
#[implement(super::Service)]
pub fn check_address_rate_limit(&self, address: &str) -> Result {
	check_bucket(&self.address_ratelimiter, address.into(), RC_PER_SECOND, RC_BURST)
}

fn check_bucket<K>(table: &Ratelimiter<K>, key: K, rate: f64, burst: f64) -> Result
where
	K: Eq + Hash,
{
	let now = Instant::now();
	let mut buckets = table.lock()?;

	if buckets.len() >= RATELIMIT_MAP_CAP {
		buckets.retain(|_, bucket| {
			let (last, toks) = *bucket;
			now.duration_since(last)
				.as_secs_f64()
				.mul_add(rate, toks)
				< burst
		});
	}

	let (last_time, tokens) = buckets.entry(key).or_insert_with(|| (now, burst));

	let new_tokens = now
		.duration_since(*last_time)
		.as_secs_f64()
		.mul_add(rate, *tokens)
		.min(burst);

	if new_tokens < 1.0 {
		return Err(Error::Request(
			ErrorKind::LimitExceeded(LimitExceededErrorData { retry_after: None }),
			"Too many verification requests.".into(),
			StatusCode::TOO_MANY_REQUESTS,
		));
	}

	*last_time = now;
	*tokens = new_tokens - 1.0;

	Ok(())
}
