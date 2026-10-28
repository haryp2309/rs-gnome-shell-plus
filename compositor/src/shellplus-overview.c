#define G_LOG_DOMAIN "shellplus"
#define _GNU_SOURCE

#include "shellplus-overview.h"

#include <cairo.h>
#include <errno.h>
#include <gio/gunixfdlist.h>
#include <meta/meta-window-actor.h>
#include <meta/workspace.h>
#include <sys/mman.h>
#include <unistd.h>

#include "shellplus-dbus.h"

/* The overview is drawn by the panel process, which can't see other
 * clients' windows. This module gives it what it needs:
 *
 *   ListWindows() -> a(ta{sv})
 *       Every normal window, bottom to top. Keys: title s, app-id s,
 *       workspace i (-1 = on all workspaces), monitor i, rect (iiii) frame
 *       rectangle, minimized b, focused b.
 *   GetPreview(t id, i max_width, i max_height) -> (h image, i width,
 *       i height, i stride)
 *       The window's current contents (frame rectangle, no shadows),
 *       scaled down to fit, as a memfd holding cairo ARGB32 (premultiplied
 *       BGRA in memory on little-endian).
 *   ActivateWindow(t id), CloseWindow(t id), MoveWindowToWorkspace(t id, i)
 *   SetOverviewActive(b)
 *       The panel says when the overview is up; WindowsDamaged is only
 *       sent then.
 *
 *   WindowsChanged()          the list changed; call ListWindows again
 *   WindowsDamaged(at ids)    these windows repainted (throttled)
 *   ShowOverview(s mode)      "toggle", "windows" or "apps" (Super key,
 *                             org.gnome.shell.keybindings, ToggleOverview)
 */

#define OVERVIEW_IFACE "org.gnome.ShellPlus.Overview"
#define CHANGED_DELAY_MS 50
#define DAMAGE_INTERVAL_MS 66  /* ~15 preview updates per second */

static const char introspection_xml[] =
  "<node>"
  "  <interface name='" OVERVIEW_IFACE "'>"
  "    <method name='ListWindows'>"
  "      <arg type='a(ta{sv})' name='windows' direction='out'/>"
  "    </method>"
  "    <method name='GetPreview'>"
  "      <arg type='t' name='id' direction='in'/>"
  "      <arg type='i' name='max_width' direction='in'/>"
  "      <arg type='i' name='max_height' direction='in'/>"
  "      <arg type='h' name='image' direction='out'/>"
  "      <arg type='i' name='width' direction='out'/>"
  "      <arg type='i' name='height' direction='out'/>"
  "      <arg type='i' name='stride' direction='out'/>"
  "    </method>"
  "    <method name='ActivateWindow'>"
  "      <arg type='t' name='id' direction='in'/>"
  "    </method>"
  "    <method name='CloseWindow'>"
  "      <arg type='t' name='id' direction='in'/>"
  "    </method>"
  "    <method name='MoveWindowToWorkspace'>"
  "      <arg type='t' name='id' direction='in'/>"
  "      <arg type='i' name='workspace' direction='in'/>"
  "    </method>"
  "    <method name='SetOverviewActive'>"
  "      <arg type='b' name='active' direction='in'/>"
  "    </method>"
  "    <signal name='WindowsChanged'/>"
  "    <signal name='WindowsDamaged'>"
  "      <arg type='at' name='ids'/>"
  "    </signal>"
  "    <signal name='ShowOverview'>"
  "      <arg type='s' name='mode'/>"
  "    </signal>"
  "  </interface>"
  "</node>";

struct _ShellPlusOverview
{
  MetaDisplay *display;
  ShellPlusOverviewFilter filter;
  gpointer user_data;

  GDBusNodeInfo *node_info;
  GDBusConnection *connection;
  guint registration_id;
  GCancellable *cancellable;

  GSettings *shell_keybindings;  /* org.gnome.shell.keybindings, if installed */

  gboolean active;
  guint changed_id;
  guint damage_id;
  GHashTable *damaged;  /* window id (guint64*) set */
};

static MetaWindowActor *
window_actor (MetaWindow *window)
{
  GObject *actor = meta_window_get_compositor_private (window);

  return actor ? META_WINDOW_ACTOR (actor) : NULL;
}

static gboolean
is_minimized (MetaWindow *window)
{
  gboolean minimized;

  g_object_get (window, "minimized", &minimized, NULL);
  return minimized;
}

static gboolean
is_listed (ShellPlusOverview *self,
           MetaWindow        *window)
{
  if (self->filter && self->filter (window, self->user_data))
    return FALSE;
  if (meta_window_is_skip_taskbar (window))
    return FALSE;

  switch (meta_window_get_window_type (window))
    {
    case META_WINDOW_NORMAL:
    case META_WINDOW_DIALOG:
    case META_WINDOW_MODAL_DIALOG:
    case META_WINDOW_UTILITY:
      return TRUE;
    default:
      return FALSE;
    }
}

static MetaWindow *
find_window (ShellPlusOverview *self,
             guint64            id)
{
  g_autoptr (GList) windows = meta_display_list_all_windows (self->display);

  for (GList *l = windows; l; l = l->next)
    {
      if (meta_window_get_id (l->data) == id && is_listed (self, l->data))
        return l->data;
    }
  return NULL;
}

static void
emit (ShellPlusOverview *self,
      const char        *signal,
      GVariant          *params)
{
  if (!self->connection)
    {
      if (params)
        g_variant_unref (g_variant_ref_sink (params));
      return;
    }
  g_dbus_connection_emit_signal (self->connection, NULL, SHELLPLUS_DBUS_PATH,
                                 OVERVIEW_IFACE, signal, params, NULL);
}

/* ------------------------------------------------------------------------ */
/* Change and damage tracking                                               */

static gboolean
changed_cb (gpointer user_data)
{
  ShellPlusOverview *self = user_data;

  self->changed_id = 0;
  emit (self, "WindowsChanged", NULL);
  return G_SOURCE_REMOVE;
}

static void
queue_changed (ShellPlusOverview *self)
{
  if (!self->changed_id)
    self->changed_id = g_timeout_add (CHANGED_DELAY_MS, changed_cb, self);
}

static gboolean
damage_cb (gpointer user_data)
{
  ShellPlusOverview *self = user_data;
  GVariantBuilder builder;
  GHashTableIter iter;
  gpointer key;

  self->damage_id = 0;
  if (g_hash_table_size (self->damaged) == 0)
    return G_SOURCE_REMOVE;

  g_variant_builder_init (&builder, G_VARIANT_TYPE ("at"));
  g_hash_table_iter_init (&iter, self->damaged);
  while (g_hash_table_iter_next (&iter, &key, NULL))
    g_variant_builder_add (&builder, "t", *(guint64 *) key);
  g_hash_table_remove_all (self->damaged);

  emit (self, "WindowsDamaged", g_variant_new ("(at)", &builder));
  return G_SOURCE_REMOVE;
}

static void
on_actor_damaged (MetaWindowActor   *actor,
                  ShellPlusOverview *self)
{
  MetaWindow *window;
  guint64 *id;

  if (!self->active)
    return;

  window = meta_window_actor_get_meta_window (actor);
  if (!window || !is_listed (self, window))
    return;

  id = g_new (guint64, 1);
  *id = meta_window_get_id (window);
  g_hash_table_add (self->damaged, id);
  if (!self->damage_id)
    self->damage_id = g_timeout_add (DAMAGE_INTERVAL_MS, damage_cb, self);
}

/* The actor may not exist yet when the window is created; this is called
 * again whenever the list is read. */
static void
watch_actor (ShellPlusOverview *self,
             MetaWindow        *window)
{
  MetaWindowActor *actor = window_actor (window);

  if (!actor || g_object_get_data (G_OBJECT (actor), "shellplus-overview-watched"))
    return;

  g_object_set_data (G_OBJECT (actor), "shellplus-overview-watched", GINT_TO_POINTER (1));
  g_signal_connect (actor, "damaged", G_CALLBACK (on_actor_damaged), self);
}

static void
watch_window (ShellPlusOverview *self,
              MetaWindow        *window)
{
  static const char *signals[] = {
    "notify::title", "notify::minimized", "notify::wm-class",
    "workspace-changed", "position-changed", "size-changed", "unmanaged",
  };

  for (guint i = 0; i < G_N_ELEMENTS (signals); i++)
    g_signal_connect_swapped (window, signals[i], G_CALLBACK (queue_changed), self);
}

static void
on_window_created (MetaDisplay       *display,
                   MetaWindow        *window,
                   ShellPlusOverview *self)
{
  watch_window (self, window);
  queue_changed (self);
}

/* ------------------------------------------------------------------------ */
/* D-Bus methods                                                            */

static char *
window_app_id (MetaWindow *window)
{
  const char *id = meta_window_get_gtk_application_id (window);

  if (!id || !*id)
    id = meta_window_get_sandboxed_app_id (window);
  if (!id || !*id)
    id = meta_window_get_wm_class (window);
  return g_strdup (id ? id : "");
}

static GVariant *
list_windows (ShellPlusOverview *self)
{
  g_autoptr (GList) all = meta_display_list_all_windows (self->display);
  MetaWindow *focus = meta_display_get_focus_window (self->display);
  GSList *unsorted = NULL, *sorted;
  GVariantBuilder builder;

  for (GList *l = all; l; l = l->next)
    {
      if (is_listed (self, l->data))
        unsorted = g_slist_prepend (unsorted, l->data);
    }
  sorted = meta_display_sort_windows_by_stacking (self->display, unsorted);
  g_slist_free (unsorted);

  g_variant_builder_init (&builder, G_VARIANT_TYPE ("a(ta{sv})"));
  for (GSList *l = sorted; l; l = l->next)
    {
      MetaWindow *window = l->data;
      MetaWorkspace *workspace = meta_window_get_workspace (window);
      g_autofree char *app_id = window_app_id (window);
      const char *title = meta_window_get_title (window);
      GVariantBuilder props;
      MtkRectangle rect;

      watch_actor (self, window);
      meta_window_get_frame_rect (window, &rect);

      g_variant_builder_init (&props, G_VARIANT_TYPE_VARDICT);
      g_variant_builder_add (&props, "{sv}", "title", g_variant_new_string (title ? title : ""));
      g_variant_builder_add (&props, "{sv}", "app-id", g_variant_new_string (app_id));
      g_variant_builder_add (&props, "{sv}", "workspace",
                             g_variant_new_int32 (meta_window_is_on_all_workspaces (window) || !workspace ?
                                                  -1 : meta_workspace_index (workspace)));
      g_variant_builder_add (&props, "{sv}", "monitor",
                             g_variant_new_int32 (meta_window_get_monitor (window)));
      g_variant_builder_add (&props, "{sv}", "rect",
                             g_variant_new ("(iiii)", rect.x, rect.y, rect.width, rect.height));
      g_variant_builder_add (&props, "{sv}", "minimized",
                             g_variant_new_boolean (is_minimized (window)));
      g_variant_builder_add (&props, "{sv}", "focused", g_variant_new_boolean (window == focus));
      g_variant_builder_add (&builder, "(ta{sv})", meta_window_get_id (window), &props);
    }
  g_slist_free (sorted);

  return g_variant_new ("(a(ta{sv}))", &builder);
}

/* Renders the window's frame rectangle, scaled to fit max_width x
 * max_height, into a sealed-size memfd. Returns -1 on failure. */
static int
capture_preview (MetaWindow *window,
                 int         max_width,
                 int         max_height,
                 int        *out_width,
                 int        *out_height,
                 int        *out_stride,
                 GError    **error)
{
  MetaWindowActor *actor = window_actor (window);
  cairo_surface_t *image, *target;
  MtkRectangle frame, buffer, clip;
  double scale;
  int width, height, stride, fd;
  size_t size;
  void *data;
  cairo_t *cr;

  if (!actor)
    {
      g_set_error (error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND, "Window has no actor yet");
      return -1;
    }

  /* get_image's clip is relative to the buffer; leave out client-side
   * shadows by clipping to the frame. */
  meta_window_get_frame_rect (window, &frame);
  meta_window_get_buffer_rect (window, &buffer);
  clip = (MtkRectangle) { frame.x - buffer.x, frame.y - buffer.y, frame.width, frame.height };
  image = meta_window_actor_get_image (actor, &clip);
  if (!image)
    image = meta_window_actor_get_image (actor, NULL);
  if (!image || cairo_surface_status (image) != CAIRO_STATUS_SUCCESS)
    {
      g_clear_pointer (&image, cairo_surface_destroy);
      g_set_error (error, G_IO_ERROR, G_IO_ERROR_FAILED, "Could not read the window's contents");
      return -1;
    }

  width = cairo_image_surface_get_width (image);
  height = cairo_image_surface_get_height (image);
  scale = MIN (1.0, MIN ((double) MAX (max_width, 1) / width,
                         (double) MAX (max_height, 1) / height));
  width = MAX (1, (int) (width * scale));
  height = MAX (1, (int) (height * scale));
  stride = cairo_format_stride_for_width (CAIRO_FORMAT_ARGB32, width);
  size = (size_t) stride * height;

  fd = memfd_create ("shellplus-preview", MFD_CLOEXEC);
  if (fd < 0 || ftruncate (fd, size) < 0 ||
      (data = mmap (NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0)) == MAP_FAILED)
    {
      g_set_error (error, G_IO_ERROR, g_io_error_from_errno (errno),
                   "Could not create the preview buffer: %s", g_strerror (errno));
      if (fd >= 0)
        close (fd);
      cairo_surface_destroy (image);
      return -1;
    }

  target = cairo_image_surface_create_for_data (data, CAIRO_FORMAT_ARGB32, width, height, stride);
  cr = cairo_create (target);
  cairo_set_operator (cr, CAIRO_OPERATOR_SOURCE);
  cairo_scale (cr, (double) width / cairo_image_surface_get_width (image),
               (double) height / cairo_image_surface_get_height (image));
  cairo_set_source_surface (cr, image, 0, 0);
  cairo_pattern_set_filter (cairo_get_source (cr), CAIRO_FILTER_GOOD);
  cairo_paint (cr);
  cairo_destroy (cr);
  cairo_surface_flush (target);
  cairo_surface_destroy (target);
  cairo_surface_destroy (image);
  munmap (data, size);

  *out_width = width;
  *out_height = height;
  *out_stride = stride;
  return fd;
}

static void
handle_get_preview (ShellPlusOverview     *self,
                    GVariant              *parameters,
                    GDBusMethodInvocation *invocation)
{
  g_autoptr (GError) error = NULL;
  g_autoptr (GUnixFDList) fds = NULL;
  MetaWindow *window;
  guint64 id;
  int max_width, max_height, width, height, stride, fd;

  g_variant_get (parameters, "(tii)", &id, &max_width, &max_height);
  window = find_window (self, id);
  if (!window)
    {
      g_dbus_method_invocation_return_error (invocation, G_DBUS_ERROR,
                                             G_DBUS_ERROR_INVALID_ARGS,
                                             "No window %" G_GUINT64_FORMAT, id);
      return;
    }

  fd = capture_preview (window, max_width, max_height, &width, &height, &stride, &error);
  if (fd < 0)
    {
      g_dbus_method_invocation_return_gerror (invocation, error);
      return;
    }

  fds = g_unix_fd_list_new_from_array (&fd, 1);  /* takes the fd */
  g_dbus_method_invocation_return_value_with_unix_fd_list (invocation,
                                                           g_variant_new ("(hiii)", 0, width,
                                                                          height, stride),
                                                           fds);
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
  ShellPlusOverview *self = user_data;
  guint32 now = meta_display_get_current_time_roundtrip (self->display);
  MetaWindow *window = NULL;
  guint64 id;

  if (g_strcmp0 (method_name, "ListWindows") == 0)
    {
      g_dbus_method_invocation_return_value (invocation, list_windows (self));
      return;
    }
  if (g_strcmp0 (method_name, "GetPreview") == 0)
    {
      handle_get_preview (self, parameters, invocation);
      return;
    }
  if (g_strcmp0 (method_name, "SetOverviewActive") == 0)
    {
      g_variant_get (parameters, "(b)", &self->active);
      if (!self->active)
        g_hash_table_remove_all (self->damaged);
      g_dbus_method_invocation_return_value (invocation, NULL);
      return;
    }

  /* The rest act on one window. */
  g_variant_get_child (parameters, 0, "t", &id);
  window = find_window (self, id);
  if (!window)
    {
      g_dbus_method_invocation_return_error (invocation, G_DBUS_ERROR,
                                             G_DBUS_ERROR_INVALID_ARGS,
                                             "No window %" G_GUINT64_FORMAT, id);
      return;
    }

  if (g_strcmp0 (method_name, "ActivateWindow") == 0)
    {
      MetaWorkspace *workspace = meta_window_get_workspace (window);

      if (workspace && !meta_window_is_on_all_workspaces (window))
        meta_workspace_activate_with_focus (workspace, window, now);
      else
        meta_window_activate (window, now);
    }
  else if (g_strcmp0 (method_name, "CloseWindow") == 0)
    {
      meta_window_delete (window, now);
    }
  else if (g_strcmp0 (method_name, "MoveWindowToWorkspace") == 0)
    {
      int index;

      g_variant_get (parameters, "(ti)", &id, &index);
      meta_window_change_workspace_by_index (window, index, TRUE);
    }
  else
    {
      g_dbus_method_invocation_return_error (invocation, G_DBUS_ERROR,
                                             G_DBUS_ERROR_UNKNOWN_METHOD,
                                             "Unknown method %s", method_name);
      return;
    }
  g_dbus_method_invocation_return_value (invocation, NULL);
}

static const GDBusInterfaceVTable interface_vtable = {
  handle_method_call,
};

static void
on_bus_ready (GObject      *source,
              GAsyncResult *result,
              gpointer      user_data)
{
  ShellPlusOverview *self = user_data;
  g_autoptr (GError) error = NULL;
  GDBusConnection *connection = g_bus_get_finish (result, &error);

  if (!connection)
    {
      if (!g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
        g_warning ("No session bus for the overview: %s", error->message);
      return;
    }

  self->connection = connection;
  self->registration_id =
    g_dbus_connection_register_object (connection, SHELLPLUS_DBUS_PATH,
                                       self->node_info->interfaces[0],
                                       &interface_vtable, self, NULL, &error);
  if (!self->registration_id)
    g_warning ("Failed to export the overview interface: %s", error->message);
}

/* ------------------------------------------------------------------------ */
/* Triggers                                                                 */

void
shellplus_overview_request (ShellPlusOverview *self,
                            const char        *mode)
{
  emit (self, "ShowOverview", g_variant_new ("(s)", mode));
}

static void
on_overlay_key (ShellPlusOverview *self)
{
  shellplus_overview_request (self, "toggle");
}

static void
on_toggle_overview (MetaDisplay          *display,
                    MetaWindow           *window,
                    const ClutterEvent   *event,
                    MetaKeyBinding       *binding,
                    gpointer              user_data)
{
  shellplus_overview_request (user_data, "windows");
}

static void
on_toggle_apps (MetaDisplay          *display,
                MetaWindow           *window,
                const ClutterEvent   *event,
                MetaKeyBinding       *binding,
                gpointer              user_data)
{
  shellplus_overview_request (user_data, "apps");
}

/* GNOME Shell's own Super+S / Super+A bindings live in its schema; use
 * them when GNOME Shell is installed alongside. */
static void
add_shell_keybindings (ShellPlusOverview *self)
{
  GSettingsSchemaSource *source = g_settings_schema_source_get_default ();
  g_autoptr (GSettingsSchema) schema =
    source ? g_settings_schema_source_lookup (source, "org.gnome.shell.keybindings", TRUE) : NULL;

  if (!schema)
    {
      g_debug ("org.gnome.shell.keybindings not installed; only Super opens the overview");
      return;
    }

  self->shell_keybindings = g_settings_new_full (schema, NULL, NULL);
  if (g_settings_schema_has_key (schema, "toggle-overview"))
    meta_display_add_keybinding (self->display, "toggle-overview", self->shell_keybindings,
                                 META_KEY_BINDING_IGNORE_AUTOREPEAT, on_toggle_overview,
                                 self, NULL);
  if (g_settings_schema_has_key (schema, "toggle-application-view"))
    meta_display_add_keybinding (self->display, "toggle-application-view",
                                 self->shell_keybindings, META_KEY_BINDING_IGNORE_AUTOREPEAT,
                                 on_toggle_apps, self, NULL);
}

/* ------------------------------------------------------------------------ */

ShellPlusOverview *
shellplus_overview_new (MetaDisplay             *display,
                        ShellPlusOverviewFilter  filter,
                        gpointer                 user_data)
{
  ShellPlusOverview *self = g_new0 (ShellPlusOverview, 1);
  g_autoptr (GList) windows = NULL;

  self->display = display;
  self->filter = filter;
  self->user_data = user_data;
  self->damaged = g_hash_table_new_full (g_int64_hash, g_int64_equal, g_free, NULL);
  self->node_info = g_dbus_node_info_new_for_xml (introspection_xml, NULL);
  self->cancellable = g_cancellable_new ();
  g_bus_get (G_BUS_TYPE_SESSION, self->cancellable, on_bus_ready, self);

  g_signal_connect (display, "window-created", G_CALLBACK (on_window_created), self);
  g_signal_connect_swapped (display, "restacked", G_CALLBACK (queue_changed), self);
  g_signal_connect_swapped (display, "notify::focus-window", G_CALLBACK (queue_changed), self);
  g_signal_connect_swapped (display, "overlay-key", G_CALLBACK (on_overlay_key), self);

  windows = meta_display_list_all_windows (display);
  for (GList *l = windows; l; l = l->next)
    watch_window (self, l->data);

  add_shell_keybindings (self);
  return self;
}

void
shellplus_overview_free (ShellPlusOverview *self)
{
  g_autoptr (GList) windows = NULL;

  if (self->shell_keybindings)
    {
      meta_display_remove_keybinding (self->display, "toggle-overview");
      meta_display_remove_keybinding (self->display, "toggle-application-view");
    }
  g_clear_object (&self->shell_keybindings);
  windows = meta_display_list_all_windows (self->display);
  for (GList *l = windows; l; l = l->next)
    {
      MetaWindowActor *actor = window_actor (l->data);

      g_signal_handlers_disconnect_by_data (l->data, self);
      if (actor)
        g_signal_handlers_disconnect_by_data (actor, self);
    }
  g_signal_handlers_disconnect_by_data (self->display, self);
  g_cancellable_cancel (self->cancellable);
  g_clear_object (&self->cancellable);
  g_clear_handle_id (&self->changed_id, g_source_remove);
  g_clear_handle_id (&self->damage_id, g_source_remove);
  if (self->registration_id)
    g_dbus_connection_unregister_object (self->connection, self->registration_id);
  g_clear_object (&self->connection);
  g_dbus_node_info_unref (self->node_info);
  g_hash_table_unref (self->damaged);
  g_free (self);
}
