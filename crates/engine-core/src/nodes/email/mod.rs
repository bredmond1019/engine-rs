//! `email` (`EN.6.B`) — the Resend-backed email channel adapter.
//!
//! `transport` (task 2) is [`transport::EmailChannelTransport`], the
//! `ChannelTransport` impl that sends outbound mail through the Resend HTTP
//! API over the injectable `HttpPost` seam. `inbound` (task 4) is
//! [`inbound::parse_inbound_email`], a pure parser from a Resend inbound-mail
//! webhook payload to an `IngressEnvelope`. A later task in this block adds
//! `webhook_events` (task 5, `map_delivery_event`) alongside them.

pub mod inbound;
pub mod transport;

pub use inbound::parse_inbound_email;
pub use transport::{
    EmailChannelTransport, DEFAULT_EMAIL_FROM, EMAIL_FROM_ENV, RESEND_API_KEY_ENV,
};
