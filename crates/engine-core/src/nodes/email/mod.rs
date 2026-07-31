//! `email` (`EN.6.B`) — the Resend-backed email channel adapter.
//!
//! `transport` (task 2) is [`transport::EmailChannelTransport`], the
//! `ChannelTransport` impl that sends outbound mail through the Resend HTTP
//! API over the injectable `HttpPost` seam. Later tasks in this block add
//! `inbound` (task 4, `parse_inbound_email`) and `webhook_events` (task 5,
//! `map_delivery_event`) alongside it.

pub mod transport;

pub use transport::{
    EmailChannelTransport, DEFAULT_EMAIL_FROM, EMAIL_FROM_ENV, RESEND_API_KEY_ENV,
};
