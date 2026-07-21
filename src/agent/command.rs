use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A command envelope dispatched to the agent.
///
/// This is the single shape every transport (HTTP, vsock, WSS) and the local
/// `agent-dispatch` CLI all accept. The [`Dispatcher`](super::Dispatcher) routes
/// on `module` + `action` and hands `payload` to the matching module's
/// `handle` method.
///
/// `id` is opaque to the agent — it's echoed back in the [`AgentResponse`] so
/// the controller can correlate requests and responses over async transports.
///
/// `signature` is reserved for future command signing. It is currently only
/// checked for emptiness (see [`Dispatcher::dispatch`](super::Dispatcher)); a
/// non-empty value is accepted but not yet verified against a key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCommand {
	pub id: String,
	pub module: String,
	pub action: String,
	#[serde(default)]
	pub payload: Value,
	#[serde(default)]
	pub signature: Option<String>,
}

/// The response to an [`AgentCommand`].
///
/// Exactly one of `payload` (success) or `error` (failure) is set; the
/// `skip_serializing_if` attributes keep the wire format compact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentResponse {
	pub id: String,
	pub ok: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub payload: Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub error: Option<String>,
}

impl AgentResponse {
	/// Build a successful response carrying `payload`, echoing the command `id`.
	pub fn ok(id: impl Into<String>, payload: Value) -> Self {
		Self {
			id: id.into(),
			ok: true,
			payload: Some(payload),
			error: None,
		}
	}

	/// Build an error response carrying a human-readable `error` message,
	/// echoing the command `id`.
	pub fn error(id: impl Into<String>, error: impl Into<String>) -> Self {
		Self {
			id: id.into(),
			ok: false,
			payload: None,
			error: Some(error.into()),
		}
	}
}
