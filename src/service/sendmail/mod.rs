use std::sync::Arc;

use lettre::{
	Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
	message::{Mailbox, header::ContentType},
};
use tuwunel_core::{Err, Result, err, implement};

/// Outbound email transport. Holds a pooled SMTP connection and the configured
/// sender mailbox when `[global.smtp]` is present; disabled otherwise.
pub struct Service {
	transport: Option<Transport>,
}

struct Transport {
	smtp: AsyncSmtpTransport<Tokio1Executor>,
	sender: Mailbox,
}

impl crate::Service for Service {
	fn build(args: &crate::Args<'_>) -> Result<Arc<Self>> {
		let smtp = &args.server.config.smtp;
		let transport = smtp
			.connection_uri
			.is_some()
			.then(|| build_transport(smtp))
			.transpose()?;

		Ok(Arc::new(Self { transport }))
	}

	fn name(&self) -> &str { crate::service::make_name(std::module_path!()) }
}

/// Whether the email subsystem is configured and able to send.
#[implement(Service)]
#[inline]
#[must_use]
pub fn is_enabled(&self) -> bool { self.transport.is_some() }

/// Send a single HTML message to one recipient from the configured sender.
/// Returns an error when the subsystem is disabled or delivery fails.
#[implement(Service)]
#[tracing::instrument(
	level = "debug",
	skip(self, subject, body_html),
	fields(
		%to,
	),
)]
pub async fn send(&self, to: &Address, subject: &str, body_html: String) -> Result<()> {
	let Some(transport) = self.transport.as_ref() else {
		return Err!(Config("smtp", "The email subsystem is not configured"));
	};

	let message = Message::builder()
		.from(transport.sender.clone())
		.to(Mailbox::new(None, to.clone()))
		.subject(subject)
		.header(ContentType::TEXT_HTML)
		.body(body_html)
		.map_err(|e| err!(Request(Unknown("Failed to build email message: {e}"))))?;

	transport
		.smtp
		.send(message)
		.await
		.map_err(|e| err!(Request(Unknown("Failed to send email: {e}"))))?;

	Ok(())
}

/// A malformed address maps to `M_INVALID_PARAM`.
#[implement(Service)]
pub async fn send_to(&self, to: &str, subject: &str, body_html: String) -> Result<()> {
	let to: Address = to
		.parse()
		.map_err(|_| err!(Request(InvalidParam("Email address is malformed"))))?;

	self.send(&to, subject, body_html).await
}

/// Confirms a string address parses as a deliverable mailbox. A malformed
/// address maps to `M_INVALID_PARAM`.
#[implement(Service)]
pub fn check_address(&self, to: &str) -> Result<()> {
	to.parse::<Address>()
		.map(|_| ())
		.map_err(|_| err!(Request(InvalidParam("Email address is malformed"))))
}

fn build_transport(config: &tuwunel_core::config::SmtpConfig) -> Result<Transport> {
	let uri = config.connection_uri.as_deref().ok_or_else(|| {
		err!(Config(
			"smtp.connection_uri",
			"An SMTP connection_uri is required to send email"
		))
	})?;

	let sender = config
		.sender
		.as_deref()
		.ok_or_else(|| err!(Config("smtp.sender", "An SMTP sender mailbox is required")))?
		.parse()
		.map_err(|e| err!(Config("smtp.sender", "Invalid sender mailbox: {e}")))?;

	let smtp = AsyncSmtpTransport::<Tokio1Executor>::from_url(uri)
		.map_err(|e| err!(Config("smtp.connection_uri", "Invalid SMTP connection_uri: {e}")))?
		.build();

	Ok(Transport { smtp, sender })
}
