use http::StatusCode;
use ruma::{OwnedServerName, api::error::ErrorBody};
use serde_json::Value;
use tuwunel_core::{Error, err};

use super::peer::{Classification, classify_error};

fn federation_error(status: StatusCode) -> Error {
	let server = OwnedServerName::try_from("remote.example").expect("valid server name");
	let body = ErrorBody::Json(Value::Null);

	Error::Federation(server, body.into_error(status))
}

#[test]
fn content_4xx_is_not_a_peer_failure() {
	for status in [
		StatusCode::BAD_REQUEST,
		StatusCode::UNAUTHORIZED,
		StatusCode::FORBIDDEN,
		StatusCode::NOT_FOUND,
	] {
		assert!(classify_error(&federation_error(status)).is_none(), "{status} recorded");
	}
}

#[test]
fn gone_is_permanent() {
	let verdict = classify_error(&federation_error(StatusCode::GONE));

	assert!(matches!(verdict, Some(Classification::Permanent)));
}

#[test]
fn server_error_and_rate_limit_are_transient() {
	for status in [
		StatusCode::TOO_MANY_REQUESTS,
		StatusCode::INTERNAL_SERVER_ERROR,
		StatusCode::SERVICE_UNAVAILABLE,
	] {
		assert!(
			matches!(classify_error(&federation_error(status)), Some(Classification::Transient)),
			"{status} not transient"
		);
	}
}

#[test]
fn non_federation_error_is_transient() {
	let error = err!(BadServerResponse("transport failure"));

	assert!(matches!(classify_error(&error), Some(Classification::Transient)));
}
