#include "tetra-polkit-agent.h"

#include <glib.h>
#include <polkit/polkit.h>
#include <polkitagent/polkitagent.h>

/* Each request is keyed by Polkit's opaque cookie. The cookie is never exposed
 * as a credential; it only correlates one WSS prompt with one PAM session. */
typedef struct {
  PolkitAgentSession *session;
  gchar *prompt_id;
  GAsyncReadyCallback completion_callback;
  gpointer completion_user_data;
} TetraPendingAuthentication;

typedef struct {
  PolkitAgentListener parent_instance;
  TetraPolkitPromptCallback prompt_callback;
  TetraPolkitCompletionCallback completion_callback;
  gpointer callback_user_data;
  GHashTable *pending;
} TetraPolkitListener;

typedef struct {
  PolkitAgentListenerClass parent_class;
} TetraPolkitListenerClass;

G_DEFINE_TYPE(TetraPolkitListener, tetra_polkit_listener, POLKIT_AGENT_TYPE_LISTENER)

static void tetra_pending_free(TetraPendingAuthentication *pending) {
  if (pending == NULL) return;
  if (pending->session != NULL) g_object_unref(pending->session);
  g_free(pending->prompt_id);
  g_free(pending);
}

static void on_session_request(
    PolkitAgentSession *session,
    const gchar *request,
    gboolean echo_on,
    gpointer user_data) {
  TetraPendingAuthentication *pending = user_data;
  TetraPolkitListener *listener = g_object_get_data(G_OBJECT(session), "tetra-listener");
  /* PAM may emit informational requests. Only secret/no-echo requests are
   * forwarded as credentials; informational text is included in message text. */
  if (listener != NULL && !echo_on && listener->prompt_callback != NULL) {
    listener->prompt_callback(
        pending->prompt_id,
        "io.tetra.agent.elevate",
        request != NULL ? request : "Authentication is required.",
        listener->callback_user_data);
  }
}

static void on_session_completed(
    PolkitAgentSession *session,
    gboolean authorized,
    gpointer user_data) {
  TetraPendingAuthentication *pending = user_data;
  TetraPolkitListener *listener = g_object_get_data(G_OBJECT(session), "tetra-listener");
  if (listener != NULL && listener->completion_callback != NULL) {
    listener->completion_callback(
        pending->prompt_id,
        authorized,
        listener->callback_user_data);
  }
  if (pending->completion_callback != NULL) {
    pending->completion_callback(
        G_OBJECT(listener),
        NULL,
        pending->completion_user_data);
  }
}

static void tetra_listener_initiate_authentication(
    PolkitAgentListener *agent_listener,
    const gchar *action_id,
    const gchar *message,
    const gchar *icon_name,
    PolkitDetails *details,
    const gchar *cookie,
    GList *identities,
    GCancellable *cancellable,
    GAsyncReadyCallback callback,
    gpointer user_data) {
  /* Future prompt frames may include icon/details; the first bridge forwards
   * the action/message/cookie lifecycle only. */
  (void)icon_name;
  (void)details;
  (void)cancellable;
  TetraPolkitListener *listener = (TetraPolkitListener *)agent_listener;
  PolkitIdentity *identity = identities != NULL ? POLKIT_IDENTITY(identities->data) : NULL;
  if (identity == NULL || cookie == NULL) {
    if (callback != NULL) callback(G_OBJECT(agent_listener), NULL, user_data);
    return;
  }

  TetraPendingAuthentication *pending = g_new0(TetraPendingAuthentication, 1);
  pending->prompt_id = g_strdup(cookie);
  pending->completion_callback = callback;
  pending->completion_user_data = user_data;
  pending->session = polkit_agent_session_new(identity, cookie);
  g_object_set_data(G_OBJECT(pending->session), "tetra-listener", listener);
  g_signal_connect(pending->session, "request", G_CALLBACK(on_session_request), pending);
  g_signal_connect(pending->session, "completed", G_CALLBACK(on_session_completed), pending);
  g_hash_table_insert(listener->pending, g_strdup(cookie), pending);

  if (listener->prompt_callback != NULL) {
    listener->prompt_callback(
        cookie,
        action_id != NULL ? action_id : "io.tetra.agent.elevate",
        message != NULL ? message : "Authentication is required.",
        listener->callback_user_data);
  }
  polkit_agent_session_initiate(pending->session);
}

static gboolean tetra_listener_initiate_authentication_finish(
    PolkitAgentListener *listener,
    GAsyncResult *res,
    GError **error) {
  (void)listener;
  (void)res;
  (void)error;
  /* The completion callback above is the agent's lifecycle signal. Returning
   * TRUE acknowledges the listener request to polkit. */
  return TRUE;
}

static void tetra_polkit_listener_dispose(GObject *object) {
  TetraPolkitListener *listener = (TetraPolkitListener *)object;
  if (listener->pending != NULL) {
    g_hash_table_destroy(listener->pending);
    listener->pending = NULL;
  }
  G_OBJECT_CLASS(tetra_polkit_listener_parent_class)->dispose(object);
}

static void tetra_polkit_listener_class_init(TetraPolkitListenerClass *klass) {
  GObjectClass *object_class = G_OBJECT_CLASS(klass);
  PolkitAgentListenerClass *listener_class = POLKIT_AGENT_LISTENER_CLASS(klass);
  object_class->dispose = tetra_polkit_listener_dispose;
  listener_class->initiate_authentication = tetra_listener_initiate_authentication;
  listener_class->initiate_authentication_finish = tetra_listener_initiate_authentication_finish;
}

static void tetra_polkit_listener_init(TetraPolkitListener *listener) {
  listener->pending = g_hash_table_new_full(g_str_hash, g_str_equal, g_free, (GDestroyNotify)tetra_pending_free);
}

PolkitAgentListener *tetra_polkit_listener_new(
    TetraPolkitPromptCallback prompt_callback,
    TetraPolkitCompletionCallback completion_callback,
    void *user_data) {
  TetraPolkitListener *listener = g_object_new(tetra_polkit_listener_get_type(), NULL);
  listener->prompt_callback = prompt_callback;
  listener->completion_callback = completion_callback;
  listener->callback_user_data = user_data;
  return POLKIT_AGENT_LISTENER(listener);
}

gpointer tetra_polkit_listener_register_for_session(
    PolkitAgentListener *listener,
    const char *session_id,
    char **error_message) {
  GError *error = NULL;
  PolkitSubject *subject = polkit_unix_session_new(session_id);
  gpointer handle = polkit_agent_listener_register(
      listener,
      POLKIT_AGENT_REGISTER_FLAGS_RUN_IN_THREAD,
      subject,
      "/io/tetra/Agent",
      NULL,
      &error);
  g_object_unref(subject);
  if (handle == NULL && error != NULL && error_message != NULL) {
    *error_message = g_strdup(error->message);
  }
  if (error != NULL) g_error_free(error);
  return handle;
}

void tetra_polkit_listener_unregister(gpointer registration_handle) {
  if (registration_handle != NULL) polkit_agent_listener_unregister(registration_handle);
}

int tetra_polkit_listener_respond(
    PolkitAgentListener *agent_listener,
    const char *prompt_id,
    const char *response,
    char **error_message) {
  TetraPolkitListener *listener = (TetraPolkitListener *)agent_listener;
  TetraPendingAuthentication *pending = g_hash_table_lookup(listener->pending, prompt_id);
  if (pending == NULL) {
    if (error_message != NULL) *error_message = g_strdup("Unknown or expired polkit prompt.");
    return 0;
  }
  polkit_agent_session_response(pending->session, response);
  return 1;
}

void tetra_polkit_listener_cancel(PolkitAgentListener *agent_listener, const char *prompt_id) {
  TetraPolkitListener *listener = (TetraPolkitListener *)agent_listener;
  TetraPendingAuthentication *pending = g_hash_table_lookup(listener->pending, prompt_id);
  if (pending != NULL) polkit_agent_session_cancel(pending->session);
}

void tetra_polkit_listener_free(PolkitAgentListener *listener) {
  if (listener != NULL) g_object_unref(listener);
}
