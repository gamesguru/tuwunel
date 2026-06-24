use tuwunel_core::{Err, Result, err};

/// Upper bound on an email address length, applied before canonicalization so
/// a pathological input cannot drive unbounded work.
const MAX_EMAIL_LEN: usize = 500;

/// Canonicalize an email address for storage and matching: case-fold the
/// whole address and lower-case the domain. This is stronger than
/// `str::to_lowercase`, which leaves `ß` intact; case-folding maps it to `ss`
/// so `Strauß@Example.com` and `strauss@example.com` collide on one key.
///
/// Errors when the address exceeds [`MAX_EMAIL_LEN`] or has no `@` separating
/// a non-empty local part from a non-empty domain.
pub fn canonicalize_email(address: &str) -> Result<String> {
	if address.len() > MAX_EMAIL_LEN {
		return Err!(Request(InvalidParam("Email address is too long")));
	}

	let (local, domain) = address
		.rsplit_once('@')
		.ok_or_else(|| err!(Request(InvalidParam("Email address must contain a domain"))))?;

	if local.is_empty() || domain.is_empty() {
		return Err!(Request(InvalidParam("Email address is malformed")));
	}

	let local = case_fold(local);
	let domain = case_fold(domain);

	Ok(format!("{local}@{domain}"))
}

/// Per-character Unicode case fold. `char::to_lowercase` covers the common
/// path; the full-fold expansions it omits (the German sharp s being the one
/// that matters for email) are mapped explicitly.
fn case_fold(input: &str) -> String {
	input.chars().fold(String::new(), |mut out, c| {
		match c {
			| 'ß' => out.push_str("ss"),
			| other => out.extend(other.to_lowercase()),
		}

		out
	})
}
