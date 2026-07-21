//! Minimal native libpolkit-agent-1 availability bridge.
//!
//! The full dashboard-mediated listener requires a custom `PolkitAgentListener`
//! GObject subclass so `initiate_authentication` can be forwarded to an
//! authenticated WebSocket prompt. This module only proves that the supported
//! native library is linked and exposes its version-independent registration
//! primitives; it intentionally does not instantiate the stock text listener.

use std::ffi::{c_int, c_uint, c_void};

#[allow(non_camel_case_types)]
type gboolean = c_int;
#[allow(non_camel_case_types)]
type guint = c_uint;

#[link(name = "polkit-agent-1")]
unsafe extern "C" {
    fn polkit_agent_listener_get_type() -> usize;
    fn polkit_agent_session_get_type() -> usize;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativePolkitTypes {
    pub listener_type: usize,
    pub session_type: usize,
}

/// Verify that native GObject types are available. This must succeed before a
/// Tetra-owned polkit listener is attempted in a no-desktop-agent session.
pub fn native_types() -> Option<NativePolkitTypes> {
    let listener_type = unsafe { polkit_agent_listener_get_type() };
    let session_type = unsafe { polkit_agent_session_get_type() };
    (listener_type != 0 && session_type != 0).then_some(NativePolkitTypes {
        listener_type,
        session_type,
    })
}

// Keep the C ABI primitive aliases local to this module. The future listener
// subclass uses them in a small C shim rather than exposing GObject internals
// across the Rust agent surface.
#[allow(dead_code)]
type GError = c_void;
#[allow(dead_code)]
type GVariant = c_void;

#[allow(dead_code)]
type PolkitAgentRegisterFlags = guint;
#[allow(dead_code)]
type PolkitBool = gboolean;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_agent_types_are_available() {
        assert!(native_types().is_some());
    }
}
