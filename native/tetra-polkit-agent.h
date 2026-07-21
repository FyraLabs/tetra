#pragma once

#include <polkitagent/polkitagent.h>

/*
 * Native GObject bridge for the fallback Tetra authentication agent.
 *
 * Rust does not implement GObject subclasses directly in this project. This
 * bridge owns the PolkitAgentListener vtable and forwards lifecycle events to
 * a Rust callback. The Rust side must return user responses only through the
 * PolkitAgentSession API; credentials never enter command argv or storage.
 */

typedef void (*TetraPolkitPromptCallback)(
    const char *prompt_id,
    const char *action_id,
    const char *message,
    void *user_data);

typedef void (*TetraPolkitCompletionCallback)(
    const char *prompt_id,
    int authorized,
    void *user_data);

/* Creates a listener object. Registration is deliberately separate so Rust can
 * defer to an existing desktop agent when registration fails. */
PolkitAgentListener *tetra_polkit_listener_new(
    TetraPolkitPromptCallback prompt_callback,
    TetraPolkitCompletionCallback completion_callback,
    void *user_data);

/* Registers the listener for a logind Unix session. Returns NULL on failure and
 * writes a newly allocated GError message to error_message when available. */
gpointer tetra_polkit_listener_register_for_session(
    PolkitAgentListener *listener,
    const char *session_id,
    char **error_message);

void tetra_polkit_listener_unregister(gpointer registration_handle);

/* Delivers a credential response to the active PolkitAgentSession. This buffer
 * is consumed synchronously by libpolkit-agent and must be zeroed by Rust after
 * returning from this function. */
int tetra_polkit_listener_respond(
    PolkitAgentListener *listener,
    const char *prompt_id,
    const char *response,
    char **error_message);

void tetra_polkit_listener_cancel(PolkitAgentListener *listener, const char *prompt_id);
void tetra_polkit_listener_free(PolkitAgentListener *listener);
