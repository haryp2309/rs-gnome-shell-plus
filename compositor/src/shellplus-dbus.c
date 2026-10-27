#define G_LOG_DOMAIN "shellplus"

#include "shellplus-dbus.h"

/* Private interface between the compositor and the panel client. The state
 * dictionary is deliberately open-ended so new keys can be added without
 * breaking older panels. Current keys:
 *
 *   mode              s     "translucent" | "solid"
 *   color             s     "#rrggbb" sampled from the maximized window
 *   active-workspace  i
 *   n-workspaces      i
 *   monitor           (iiii) primary monitor x, y, width, height
 */
static const char introspection_xml[] =
  "<node>"
  "  <interface name='" SHELLPLUS_DBUS_IFACE "'>"
  "    <method name='GetState'>"
  "      <arg type='a{sv}' name='state' direction='out'/>"
  "    </method>"
  "    <method name='ActivateWorkspace'>"
  "      <arg type='i' name='index' direction='in'/>"
  "    </method>"
  "    <method name='ToggleOverview'/>"
  "    <method name='LogOut'/>"
  "    <signal name='StateChanged'>"
  "      <arg type='a{sv}' name='state'/>"
  "    </signal>"
  "  </interface>"
  "</node>";

struct _ShellPlusDBus
{
  ShellPlusDBusHandlers handlers;
  gpointer user_data;

  GDBusNodeInfo *node_info;
  GDBusConnection *connection;
  guint owner_id;
  guint registration_id;
};

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
  ShellPlusDBus *self = user_data;

  if (g_strcmp0 (method_name, "GetState") == 0)
    {
      GVariant *state = self->handlers.get_state (self->user_data);

      g_dbus_method_invocation_return_value (invocation,
                                             g_variant_new ("(@a{sv})", state));
    }
  else if (g_strcmp0 (method_name, "ActivateWorkspace") == 0)
    {
      int index;

      g_variant_get (parameters, "(i)", &index);
      self->handlers.activate_workspace (self->user_data, index);
      g_dbus_method_invocation_return_value (invocation, NULL);
    }
  else if (g_strcmp0 (method_name, "ToggleOverview") == 0)
    {
      self->handlers.toggle_overview (self->user_data);
      g_dbus_method_invocation_return_value (invocation, NULL);
    }
  else if (g_strcmp0 (method_name, "LogOut") == 0)
    {
      g_dbus_method_invocation_return_value (invocation, NULL);
      self->handlers.log_out (self->user_data);
    }
  else
    {
      g_dbus_method_invocation_return_error (invocation, G_DBUS_ERROR,
                                             G_DBUS_ERROR_UNKNOWN_METHOD,
                                             "Unknown method %s", method_name);
    }
}

static const GDBusInterfaceVTable interface_vtable = {
  handle_method_call,
};

static void
on_bus_acquired (GDBusConnection *connection,
                 const char      *name,
                 gpointer         user_data)
{
  ShellPlusDBus *self = user_data;
  g_autoptr (GError) error = NULL;

  self->connection = g_object_ref (connection);
  self->registration_id =
    g_dbus_connection_register_object (connection, SHELLPLUS_DBUS_PATH,
                                       self->node_info->interfaces[0],
                                       &interface_vtable, self, NULL, &error);
  if (!self->registration_id)
    g_warning ("Failed to export D-Bus interface: %s", error->message);
}

static void
on_name_lost (GDBusConnection *connection,
              const char      *name,
              gpointer         user_data)
{
  g_warning ("Lost (or could not acquire) D-Bus name %s; the panel won't "
             "receive updates. Is a session bus running?", name);
}

ShellPlusDBus *
shellplus_dbus_new (const ShellPlusDBusHandlers *handlers,
                    gpointer                     user_data)
{
  ShellPlusDBus *self = g_new0 (ShellPlusDBus, 1);

  self->handlers = *handlers;
  self->user_data = user_data;
  self->node_info = g_dbus_node_info_new_for_xml (introspection_xml, NULL);
  self->owner_id = g_bus_own_name (G_BUS_TYPE_SESSION, SHELLPLUS_DBUS_NAME,
                                   G_BUS_NAME_OWNER_FLAGS_REPLACE,
                                   on_bus_acquired, NULL, on_name_lost,
                                   self, NULL);
  return self;
}

void
shellplus_dbus_emit_state_changed (ShellPlusDBus *self,
                                   GVariant      *state)
{
  if (!self->connection || !self->registration_id)
    return;

  g_dbus_connection_emit_signal (self->connection, NULL, SHELLPLUS_DBUS_PATH,
                                 SHELLPLUS_DBUS_IFACE, "StateChanged",
                                 g_variant_new ("(@a{sv})", state), NULL);
}

void
shellplus_dbus_free (ShellPlusDBus *self)
{
  if (self->registration_id)
    g_dbus_connection_unregister_object (self->connection, self->registration_id);
  g_bus_unown_name (self->owner_id);
  g_clear_object (&self->connection);
  g_dbus_node_info_unref (self->node_info);
  g_free (self);
}
