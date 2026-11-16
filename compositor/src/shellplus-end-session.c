#define G_LOG_DOMAIN "shellplus"

#include "shellplus-end-session.h"

#define DIALOG_PATH  "/org/gnome/SessionManager/EndSessionDialog"
#define DIALOG_IFACE "org.gnome.SessionManager.EndSessionDialog"
/* The panel's name for its implementation (panel/src/end_session.rs). */
#define PANEL_NAME   "org.gnome.ShellPlus.EndSessionDialog"

/* GNOME Shell's error when the dialog can't be shown. */
#define ERROR_GRAB_FAILED "org.gnome.Shell.ModalDialog.Error.GrabFailed"

static const char introspection_xml[] =
  "<node>"
  "  <interface name='" DIALOG_IFACE "'>"
  "    <method name='Open'>"
  "      <arg type='u' name='type' direction='in'/>"
  "      <arg type='u' name='timestamp' direction='in'/>"
  "      <arg type='u' name='seconds_to_stay_open' direction='in'/>"
  "      <arg type='ao' name='inhibitor_object_paths' direction='in'/>"
  "    </method>"
  "    <method name='Close'/>"
  "    <signal name='ConfirmedLogout'/>"
  "    <signal name='ConfirmedReboot'/>"
  "    <signal name='ConfirmedShutdown'/>"
  "    <signal name='Canceled'/>"
  "    <signal name='Closed'/>"
  "  </interface>"
  "</node>";

struct _ShellPlusEndSession
{
  ShellPlusEndSessionGetPanelPid get_panel_pid;
  gpointer user_data;

  GCancellable *cancellable;
  GDBusConnection *connection;
  GDBusNodeInfo *node_info;
  guint registration_id;
  guint signal_id;
  /* The panel's unique name, once checked to be the panel's process. */
  char *panel_unique_name;
};

typedef struct
{
  ShellPlusEndSession *self;
  GDBusMethodInvocation *invocation;
  char *unique_name;
} Request;

static void
request_free (Request *request)
{
  g_object_unref (request->invocation);
  g_free (request->unique_name);
  g_free (request);
}

static void
return_unavailable (GDBusMethodInvocation *invocation,
                    const char            *why)
{
  g_warning ("Can't show the end session dialog: %s", why);
  g_dbus_method_invocation_return_dbus_error (invocation, ERROR_GRAB_FAILED, why);
}

static void
on_forwarded (GObject      *source,
              GAsyncResult *result,
              gpointer      user_data)
{
  Request *request = user_data;
  g_autoptr (GVariant) reply = NULL;
  g_autoptr (GError) error = NULL;

  reply = g_dbus_connection_call_finish (G_DBUS_CONNECTION (source), result, &error);
  if (reply)
    {
      g_dbus_method_invocation_return_value (request->invocation, g_steal_pointer (&reply));
    }
  else if (g_dbus_error_is_remote_error (error))
    {
      /* Pass the panel's error on as is (e.g. GNOME's
       * org.gnome.Shell.ModalDialog.Error.UnknownType). */
      g_autofree char *name = g_dbus_error_get_remote_error (error);

      g_dbus_error_strip_remote_error (error);
      g_dbus_method_invocation_return_dbus_error (request->invocation, name, error->message);
    }
  else
    {
      g_dbus_method_invocation_return_gerror (request->invocation, error);
    }
  request_free (request);
}

static void
forward (ShellPlusEndSession   *self,
         GDBusMethodInvocation *invocation)
{
  Request *request = g_new0 (Request, 1);

  request->self = self;
  request->invocation = g_object_ref (invocation);
  g_dbus_connection_call (self->connection, self->panel_unique_name, DIALOG_PATH, DIALOG_IFACE,
                          g_dbus_method_invocation_get_method_name (invocation),
                          g_dbus_method_invocation_get_parameters (invocation),
                          NULL, G_DBUS_CALL_FLAGS_NONE, -1, self->cancellable,
                          on_forwarded, request);
}

static void
on_panel_pid (GObject      *source,
              GAsyncResult *result,
              gpointer      user_data)
{
  Request *request = user_data;
  g_autoptr (GVariant) reply = NULL;
  g_autoptr (GError) error = NULL;
  ShellPlusEndSession *self;
  guint32 pid = 0;
  pid_t panel_pid;

  reply = g_dbus_connection_call_finish (G_DBUS_CONNECTION (source), result, &error);
  if (g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
    {
      request_free (request);
      return;
    }

  self = request->self;
  if (reply)
    g_variant_get (reply, "(u)", &pid);
  /* Looked up now: the panel may have been restarted. */
  panel_pid = self->get_panel_pid (self->user_data);
  if (pid == 0 || panel_pid <= 0 || pid != (guint32) panel_pid)
    {
      return_unavailable (request->invocation, "the panel isn't running");
      request_free (request);
      return;
    }

  g_free (self->panel_unique_name);
  self->panel_unique_name = g_steal_pointer (&request->unique_name);
  forward (self, request->invocation);
  request_free (request);
}

static void
on_name_owner (GObject      *source,
               GAsyncResult *result,
               gpointer      user_data)
{
  Request *request = user_data;
  g_autoptr (GVariant) reply = NULL;
  g_autoptr (GError) error = NULL;

  reply = g_dbus_connection_call_finish (G_DBUS_CONNECTION (source), result, &error);
  if (g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
    {
      request_free (request);
      return;
    }
  if (!reply)
    {
      return_unavailable (request->invocation, "the panel isn't running");
      request_free (request);
      return;
    }

  g_variant_get (reply, "(s)", &request->unique_name);
  g_dbus_connection_call (request->self->connection, "org.freedesktop.DBus",
                          "/org/freedesktop/DBus", "org.freedesktop.DBus",
                          "GetConnectionUnixProcessID",
                          g_variant_new ("(s)", request->unique_name), G_VARIANT_TYPE ("(u)"),
                          G_DBUS_CALL_FLAGS_NONE, -1, request->self->cancellable,
                          on_panel_pid, request);
}

static void
handle_method_call (GDBusConnection       *connection,
                    const char            *sender,
                    const char            *object_path,
                    const char            *interface_name,
                    const char            *method_name,
                    GVariant              *parameters,
                    GDBusMethodInvocation *invocation,
                    gpointer               user_data)
{
  ShellPlusEndSession *self = user_data;
  Request *request;

  if (g_strcmp0 (method_name, "Close") == 0 && !self->panel_unique_name)
    {
      /* Nothing was ever opened. */
      g_dbus_method_invocation_return_value (invocation, NULL);
      return;
    }

  /* Find the panel and check it's ours, then forward. */
  request = g_new0 (Request, 1);
  request->self = self;
  request->invocation = g_object_ref (invocation);
  g_dbus_connection_call (connection, "org.freedesktop.DBus", "/org/freedesktop/DBus",
                          "org.freedesktop.DBus", "GetNameOwner",
                          g_variant_new ("(s)", PANEL_NAME), G_VARIANT_TYPE ("(s)"),
                          G_DBUS_CALL_FLAGS_NONE, -1, self->cancellable,
                          on_name_owner, request);
}

static const GDBusInterfaceVTable vtable = {
  .method_call = handle_method_call,
};

/* Re-emits the panel's signals as ours, where gnome-session listens. */
static void
on_panel_signal (GDBusConnection *connection,
                 const char      *sender_name,
                 const char      *object_path,
                 const char      *interface_name,
                 const char      *signal_name,
                 GVariant        *parameters,
                 gpointer         user_data)
{
  ShellPlusEndSession *self = user_data;
  g_autoptr (GError) error = NULL;

  if (!self->panel_unique_name || g_strcmp0 (sender_name, self->panel_unique_name) != 0)
    return;
  if (!g_dbus_interface_info_lookup_signal (self->node_info->interfaces[0], signal_name))
    return;

  g_debug ("End session dialog: %s", signal_name);
  if (!g_dbus_connection_emit_signal (connection, NULL, DIALOG_PATH, DIALOG_IFACE,
                                      signal_name, NULL, &error))
    g_warning ("Could not emit %s: %s", signal_name, error->message);
}

static void
on_bus (GObject      *source,
        GAsyncResult *result,
        gpointer      user_data)
{
  ShellPlusEndSession *self = user_data;
  g_autoptr (GDBusConnection) connection = NULL;
  g_autoptr (GError) error = NULL;

  connection = g_bus_get_finish (result, &error);
  if (!connection)
    {
      if (!g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
        g_warning ("No session bus for the end session dialog: %s", error->message);
      return;
    }

  /* The shared connection, the one that owns org.gnome.Shell. */
  self->connection = g_steal_pointer (&connection);
  self->registration_id =
    g_dbus_connection_register_object (self->connection, DIALOG_PATH,
                                       self->node_info->interfaces[0], &vtable,
                                       self, NULL, &error);
  if (!self->registration_id)
    {
      g_warning ("Could not export %s: %s", DIALOG_PATH, error->message);
      return;
    }
  self->signal_id =
    g_dbus_connection_signal_subscribe (self->connection, PANEL_NAME, DIALOG_IFACE, NULL,
                                        DIALOG_PATH, NULL, G_DBUS_SIGNAL_FLAGS_NONE,
                                        on_panel_signal, self, NULL);
}

ShellPlusEndSession *
shellplus_end_session_new (ShellPlusEndSessionGetPanelPid get_panel_pid,
                           gpointer                       user_data)
{
  ShellPlusEndSession *self = g_new0 (ShellPlusEndSession, 1);

  self->get_panel_pid = get_panel_pid;
  self->user_data = user_data;
  self->cancellable = g_cancellable_new ();
  self->node_info = g_dbus_node_info_new_for_xml (introspection_xml, NULL);
  g_bus_get (G_BUS_TYPE_SESSION, self->cancellable, on_bus, self);
  return self;
}

void
shellplus_end_session_free (ShellPlusEndSession *self)
{
  g_cancellable_cancel (self->cancellable);
  if (self->connection)
    {
      if (self->registration_id)
        g_dbus_connection_unregister_object (self->connection, self->registration_id);
      if (self->signal_id)
        g_dbus_connection_signal_unsubscribe (self->connection, self->signal_id);
    }
  g_clear_object (&self->connection);
  g_clear_object (&self->cancellable);
  g_dbus_node_info_unref (self->node_info);
  g_free (self->panel_unique_name);
  g_free (self);
}
