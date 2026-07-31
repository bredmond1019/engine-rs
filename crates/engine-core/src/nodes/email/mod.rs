//! `email` (`EN.6.B`) — the Resend-backed email channel adapter.
//!
//! `transport` (task 2) is [`transport::EmailChannelTransport`], the
//! `ChannelTransport` impl that sends outbound mail through the Resend HTTP
//! API over the injectable `HttpPost` seam. `inbound` (task 4) is
//! [`inbound::parse_inbound_email`], a pure parser from a Resend inbound-mail
//! webhook payload to an `IngressEnvelope`. `webhook_events` (task 5) is
//! [`webhook_events::map_delivery_event`], a pure mapper from a Resend
//! delivery/bounce webhook payload to `EN.7.B`'s `AddOpportunityActionEvent`,
//! correlated solely via the `opportunity_slug` tag echoed back on send.

pub mod inbound;
pub mod transport;
pub mod webhook_events;

pub use inbound::parse_inbound_email;
pub use transport::{
    EmailChannelTransport, DEFAULT_EMAIL_FROM, EMAIL_FROM_ENV, RESEND_API_KEY_ENV,
};
pub use webhook_events::{map_delivery_event, DeliveryMapping, SkipReason};
