#define G_LOG_DOMAIN "shellplus"

#include "shellplus-shell-dbus.h"

#include <clutter/clutter.h>

#define SHELL_DBUS_NAME  "org.gnome.Shell"
#define SHELL_DBUS_PATH  "/org/gnome/Shell"
#define SHELL_DBUS_IFACE "org.gnome.Shell"

/* GNOME Shell's Shell.ActionMode.NORMAL */
#define ACTION_MODE_NORMAL 1

static const char introspection_xml[] =
  "<node>"
  "  <interface name='" SHELL_DBUS_IFACE "'>"
  "    <method name='ShowOSD'>"
  "      <arg type='a{sv}' name='params' direction='in'/>"
  "    </method>"
  "    <method name='GrabAccelerator'>"
  "      <arg type='s' name='accelerator' direction='in'/>"
  "      <arg type='u' name='modeFlags' direction='in'/>"
  "      <arg type='u' name='grabFlags' direction='in'/>"
  "      <arg type='u' name='action' direction='out'/>"
  "    </method>"
  "    <method name='GrabAccelerators'>"
  "      <arg type='a(suu)' name='accelerators' direction='in'/>"
  "      <arg type='au' name='actions' direction='out'/>"
  "    </method>"
  "    <method name='UngrabAccelerator'>"
  "      <arg type='u' name='action' direction='in'/>"
  "      <arg type='b' name='success' direction='out'/>"
  "    </method>"
  "    <method name='UngrabAccelerators'>"
  "      <arg type='au' name='actions' direction='in'/>"
  "      <arg type='b' name='success' direction='out'/>"
  "    </method>"
  "    <signal name='AcceleratorActivated'>"
  "      <arg type='u' name='action'/>"
  "      <arg type='a{sv}' name='parameters'/>"
  "    </signal>"
  "    <property name='Mode' type='s' access='read'/>"
  "    <property name='OverviewActive' type='b' access='read'/>"
  "    <property name='ShellVersion' type='s' access='read'/>"
  "  </interface>"
  "</node>";

struct _ShellPlusShellDBus
{
  MetaDisplay *display;
  ShellPlusDBus *panel_dbus;

  GDBusNodeInfo *node_info;
  GDBusConnection *connection;
  guint owner_id;
  guint registration_id;
  gulong accelerator_activated_id;

  /* action id -> owning bus name, and bus name -> name watch id, so grabs
   * are released when the client that made them goes away. */
  GHashTable *grabs;
  GHashTable *watches;
};

static void
on_grabber_vanished (GDBusConnection *connection,
                     const char      *name,
                     gpointer         user_data)
{
  ShellPlusShellDBus *self = user_data;
  GHashTableIter iter;
  gpointer action, sender;

  g_hash_table_iter_init (&iter, self->grabs);
  while (g_hash_table_iter_next (&iter, &action, &sender))
    {
      if (g_strcmp0 (sender, name) == 0)
        {
          meta_display_ungrab_accelerator (self->display, GPOINTER_TO_UINT (action));
          g_hash_table_iter_remove (&iter);
        }
    }

  /* Removing the watch frees `name`. */
  g_hash_table_remove (self->watches, name);
}

static guint
grab_accelerator (ShellPlusShellDBus *self,
                  const char         *sender,
                  const char         *accelerator,
                  guint               grab_flags)
{
  guint action = meta_display_grab_accelerator (self->display, accelerator, grab_flags);

  if (action == 0)
    return 0;

  g_hash_table_insert (self->grabs, GUINT_TO_POINTER (action), g_strdup (sender));
  if (!g_hash_table_contains (self->watches, sender))
    {
      guint watch = g_bus_watch_name_on_connection (self->connection, sender,
                                                    G_BUS_NAME_WATCHER_FLAGS_NONE,
                                                    NULL, on_grabber_vanished,
                                                    self, NULL);
      g_hash_table_insert (self->watches, g_strdup (sender), GUINT_TO_POINTER (watch));
    }
  return action;
}

static gboolean
ungrab_accelerator (ShellPlusShellDBus *self,
                    const char         *sender,
                    guint               action)
{
  const char *owner = g_hash_table_lookup (self->grabs, GUINT_TO_POINTER (action));

  if (g_strcmp0 (owner, sender) != 0)
    return FALSE;

  g_hash_table_remove (self->grabs, GUINT_TO_POINTER (action));
  return meta_display_ungrab_accelerator (self->display, action);
}

static void
on_accelerator_activated (MetaDisplay        *display,
                          guint               action,
                          ClutterInputDevice *device,
                          guint               timestamp,
                          ShellPlusShellDBus *self)
{
  const char *sender = g_hash_table_lookup (self->grabs, GUINT_TO_POINTER (action));
  const char *device_node = device ? clutter_input_device_get_device_node (device) : NULL;
  GVariantBuilder params;

  if (!sender || !self->connection)
    return;

  g_variant_builder_init (&params, G_VARIANT_TYPE_VARDICT);
  g_variant_builder_add (&params, "{sv}", "timestamp", g_variant_new_uint32 (timestamp));
  g_variant_builder_add (&params, "{sv}", "action-mode", g_variant_new_uint32 (ACTION_MODE_NORMAL));
  if (device_node)
    g_variant_builder_add (&params, "{sv}", "device-node", g_variant_new_string (device_node));

  /* Like GNOME Shell, only tell the client that made the grab. */
  g_dbus_connection_emit_signal (self->connection, sender, SHELL_DBUS_PATH,
                                 SHELL_DBUS_IFACE, "AcceleratorActivated",
                                 g_variant_new ("(ua{sv})", action, &params), NULL);
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
  ShellPlusShellDBus *self = user_data;

  if (g_strcmp0 (method_name, "ShowOSD") == 0)
    {
      g_autoptr (GVariant) osd = g_variant_get_child_value (parameters, 0);

      shellplus_dbus_emit_show_osd (self->panel_dbus, osd);
      g_dbus_method_invocation_return_value (invocation, NULL);
    }
  else if (g_strcmp0 (method_name, "GrabAccelerator") == 0)
    {
      const char *accelerator;
      guint mode_flags, grab_flags;

      g_variant_get (parameters, "(&suu)", &accelerator, &mode_flags, &grab_flags);
      g_dbus_method_invocation_return_value (
        invocation,
        g_variant_new ("(u)", grab_accelerator (self, sender, accelerator, grab_flags)));
    }
  else if (g_strcmp0 (method_name, "GrabAccelerators") == 0)
    {
      g_autoptr (GVariantIter) iter = NULL;
      GVariantBuilder actions;
      const char *accelerator;
      guint mode_flags, grab_flags;

      g_variant_builder_init (&actions, G_VARIANT_TYPE ("au"));
      g_variant_get (parameters, "(a(suu))", &iter);
      while (g_variant_iter_loop (iter, "(&suu)", &accelerator, &mode_flags, &grab_flags))
        g_variant_builder_add (&actions, "u",
                               grab_accelerator (self, sender, accelerator, grab_flags));
      g_dbus_method_invocation_return_value (invocation, g_variant_new ("(au)", &actions));
    }
  else if (g_strcmp0 (method_name, "UngrabAccelerator") == 0)
    {
      guint action;

      g_variant_get (parameters, "(u)", &action);
      g_dbus_method_invocation_return_value (
        invocation, g_variant_new ("(b)", ungrab_accelerator (self, sender, action)));
    }
  else if (g_strcmp0 (method_name, "UngrabAccelerators") == 0)
    {
      g_autoptr (GVariantIter) iter = NULL;
      gboolean success = TRUE;
      guint action;

      g_variant_get (parameters, "(au)", &iter);
      while (g_variant_iter_loop (iter, "u", &action))
        success &= ungrab_accelerator (self, sender, action);
      g_dbus_method_invocation_return_value (invocation, g_variant_new ("(b)", success));
    }
  else
    {
      g_dbus_method_invocation_return_error (invocation, G_DBUS_ERROR,
                                             G_DBUS_ERROR_UNKNOWN_METHOD,
                                             "Unknown method %s", method_name);
    }
}

static GVariant *
handle_get_property (GDBusConnection  *connection,
                     const char       *sender,
                     const char       *object_path,
                     const char       *interface_name,
                     const char       *property_name,
                     GError          **error,
                     gpointer          user_data)
{
  if (g_strcmp0 (property_name, "Mode") == 0)
    return g_variant_new_string ("user");
  if (g_strcmp0 (property_name, "OverviewActive") == 0)
    return g_variant_new_boolean (FALSE);
  if (g_strcmp0 (property_name, "ShellVersion") == 0)
    return g_variant_new_string ("50.0");
  return NULL;
}

static const GDBusInterfaceVTable interface_vtable = {
  handle_method_call,
  handle_get_property,
};

static void
on_bus_acquired (GDBusConnection *connection,
                 const char      *name,
                 gpointer         user_data)
{
  ShellPlusShellDBus *self = user_data;
  g_autoptr (GError) error = NULL;

  self->connection = g_object_ref (connection);
  self->registration_id =
    g_dbus_connection_register_object (connection, SHELL_DBUS_PATH,
                                       self->node_info->interfaces[0],
                                       &interface_vtable, self, NULL, &error);
  if (!self->registration_id)
    g_warning ("Failed to export org.gnome.Shell: %s", error->message);
}

static void
on_name_lost (GDBusConnection *connection,
              const char      *name,
              gpointer         user_data)
{
  g_warning ("Could not own %s; is GNOME Shell running? Media keys and "
             "OSDs won't work.", name);
}

static void
remove_watch (gpointer watch)
{
  g_bus_unwatch_name (GPOINTER_TO_UINT (watch));
}

ShellPlusShellDBus *
shellplus_shell_dbus_new (MetaDisplay   *display,
                          ShellPlusDBus *panel_dbus)
{
  ShellPlusShellDBus *self = g_new0 (ShellPlusShellDBus, 1);

  self->display = display;
  self->panel_dbus = panel_dbus;
  self->grabs = g_hash_table_new_full (NULL, NULL, NULL, g_free);
  self->watches = g_hash_table_new_full (g_str_hash, g_str_equal, g_free, remove_watch);
  self->node_info = g_dbus_node_info_new_for_xml (introspection_xml, NULL);
  self->accelerator_activated_id =
    g_signal_connect (display, "accelerator-activated",
                      G_CALLBACK (on_accelerator_activated), self);
  self->owner_id = g_bus_own_name (G_BUS_TYPE_SESSION, SHELL_DBUS_NAME,
                                   G_BUS_NAME_OWNER_FLAGS_NONE,
                                   on_bus_acquired, NULL, on_name_lost,
                                   self, NULL);
  return self;
}

void
shellplus_shell_dbus_free (ShellPlusShellDBus *self)
{
  g_clear_signal_handler (&self->accelerator_activated_id, self->display);
  if (self->registration_id)
    g_dbus_connection_unregister_object (self->connection, self->registration_id);
  g_bus_unown_name (self->owner_id);
  g_hash_table_destroy (self->watches);
  g_hash_table_destroy (self->grabs);
  g_clear_object (&self->connection);
  g_dbus_node_info_unref (self->node_info);
  g_free (self);
}
