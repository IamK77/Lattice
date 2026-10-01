//! Flow-scoped operation authorization. Rules match complete argv elements,
//! never command substrings; shell decomposition is all-or-nothing.

mod gate;
mod rules;
mod shell;
mod store;

pub use gate::{manifest, OperationPolicy};
pub use rules::{CommandRule, Decision, GrantMatcher, Invocation};
pub use store::{read_grants, AuthorizationSources, FlowGrant, GrantState};

pub const NAME: &str = "operation-policy";
pub const INSTANCE: &str = "operations";
pub const AUTH_REQUESTED: &str = "operation.authorization_requested";
pub const DECISION: &str = "operation.authorization_decided";
pub const STATE: &str = "operation.authorization.state";
pub const CHANNEL: &str = "operation.authorization";
