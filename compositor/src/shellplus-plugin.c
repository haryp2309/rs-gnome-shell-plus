#define G_LOG_DOMAIN "shellplus"

#include "shellplus-plugin.h"

#include <cairo.h>
#include <meta/compositor.h>
#include <meta/display.h>
#include <meta/meta-backend.h>
#include <meta/meta-context.h>
#include <meta/meta-monitor-manager.h>
#include <meta/meta-wayland-client.h>
#include <meta/meta-window-actor.h>
#include <meta/meta-workspace-manager.h>
#include <meta/window.h>
#include <meta/workspace.h>

#include "shellplus-background.h"
#include "shellplus-dbus.h"
#include "shellplus-panel-constraint.h"

#define UPDATE_DELAY_MS          50
/* Apps usually need a frame or two to repaint after maximizing or after
 * their focus (backdrop) state changes; sample again once they have. */
#define FOLLOWUP_UPDATE_DELAY_MS 300
#define RESAMPLE_INTERVAL_MS     1000
#define PANEL_RESPAWN_DELAY_MS   1000
#define PANEL_MAX_QUICK_CRASHES  5
#define PANEL_QUICK_CRASH_US     (10 * G_USEC_PER_SEC)
/* Rows (in logical pixels, below the window's top edge) sampled for the
 * solid panel color. Skips the 1px highlight many header bars draw. */
#define SAMPLE_Y                 2
#define SAMPLE_HEIGHT            4

struct _ShellPlusPlugin
{
  MetaPlugin parent;

  ShellPlusBackground *background;
  ShellPlusDBus *dbus;
  GSettings *mutter_settings;

  MetaWaylandClient *panel_client;
  MetaWindow *panel_window;
  MetaExternalConstraint *panel_constraint;
  int panel_height;
  gint64 panel_spawn_time;
  int panel_quick_crashes;
  guint panel_respawn_id;

  guint update_id;
  guint followup_update_id;
  guint resample_id;
  guint workspace_check_id;

  gboolean solid;
  char *color;
  GVariant *last_state;
};

G_DEFINE_TYPE (ShellPlusPlugin, shellplus_plugin, META_TYPE_PLUGIN)

static void spawn_panel (ShellPlusPlugin *self);
static void queue_update (ShellPlusPlugin *self);

static MetaDisplay *
get_display (ShellPlusPlugin *self)
{
  return meta_plugin_get_display (META_PLUGIN (self));
}

static MetaWorkspaceManager *
get_workspace_manager (ShellPlusPlugin *self)
{
  return meta_display_get_workspace_manager (get_display (self));
}

static gboolean
get_primary_geometry (ShellPlusPlugin *self,
                      MtkRectangle    *rect)
{
  MetaDisplay *display = get_display (self);
  int primary = meta_display_get_primary_monitor (display);

  if (primary < 0)
    return FALSE;

  meta_display_get_monitor_geometry (display, primary, rect);
  return TRUE;
}

/* ------------------------------------------------------------------------ */
/* Panel state published over D-Bus                                         */

static GVariant *
build_state (ShellPlusPlugin *self)
{
  MetaWorkspaceManager *wm = get_workspace_manager (self);
  GVariantBuilder builder;
  MtkRectangle monitor = { 0 };

  get_primary_geometry (self, &monitor);

  g_variant_builder_init (&builder, G_VARIANT_TYPE_VARDICT);
  g_variant_builder_add (&builder, "{sv}", "mode",
                         g_variant_new_string (self->solid ? "solid" : "translucent"));
  g_variant_builder_add (&builder, "{sv}", "color",
                         g_variant_new_string (self->color ? self->color : "#000000"));
  g_variant_builder_add (&builder, "{sv}", "active-workspace",
                         g_variant_new_int32 (meta_workspace_manager_get_active_workspace_index (wm)));
  g_variant_builder_add (&builder, "{sv}", "n-workspaces",
                         g_variant_new_int32 (meta_workspace_manager_get_n_workspaces (wm)));
  g_variant_builder_add (&builder, "{sv}", "monitor",
                         g_variant_new ("(iiii)", monitor.x, monitor.y,
                                        monitor.width, monitor.height));
  return g_variant_builder_end (&builder);
}

static void
publish_state (ShellPlusPlugin *self)
{
  g_autoptr (GVariant) state = g_variant_ref_sink (build_state (self));

  if (self->last_state && g_variant_equal (state, self->last_state))
    return;

  g_clear_pointer (&self->last_state, g_variant_unref);
  self->last_state = g_variant_ref (state);
  shellplus_dbus_emit_state_changed (self->dbus, state);
}

static GVariant *
dbus_get_state (gpointer user_data)
{
  return build_state (user_data);
}

static void
dbus_activate_workspace (gpointer user_data,
                         int      index)
{
  ShellPlusPlugin *self = user_data;
  MetaDisplay *display = get_display (self);
  MetaWorkspace *workspace =
    meta_workspace_manager_get_workspace_by_index (get_workspace_manager (self), index);

  if (workspace)
    meta_workspace_activate (workspace, meta_display_get_current_time_roundtrip (display));
}

static void
dbus_toggle_overview (gpointer user_data)
{
  g_message ("Overview requested (not implemented yet)");
}

static void
dbus_log_out (gpointer user_data)
{
  MetaDisplay *display = get_display (user_data);

  meta_context_terminate (meta_display_get_context (display));
}

static const ShellPlusDBusHandlers dbus_handlers = {
  .get_state = dbus_get_state,
  .activate_workspace = dbus_activate_workspace,
  .toggle_overview = dbus_toggle_overview,
  .log_out = dbus_log_out,
};

/* ------------------------------------------------------------------------ */
/* Maximized window detection and color sampling                            */

static MetaWindow *
find_maximized_window (ShellPlusPlugin *self)
{
  MetaDisplay *display = get_display (self);
  MetaWorkspace *workspace =
    meta_workspace_manager_get_active_workspace (get_workspace_manager (self));
  int primary = meta_display_get_primary_monitor (display);
  GList *windows = meta_workspace_list_windows (workspace);
  GSList *unsorted = NULL, *sorted, *l;
  MetaWindow *result = NULL;
  GList *w;

  for (w = windows; w; w = w->next)
    unsorted = g_slist_prepend (unsorted, w->data);
  g_list_free (windows);

  sorted = g_slist_reverse (meta_display_sort_windows_by_stacking (display, unsorted));
  g_slist_free (unsorted);

  /* Topmost first. Vertically maximized covers both full maximize and
   * half-tiled windows, which also touch the panel. */
  for (l = sorted; l; l = l->next)
    {
      MetaWindow *window = l->data;

      if (window == self->panel_window ||
          meta_window_get_window_type (window) != META_WINDOW_NORMAL ||
          meta_window_is_hidden (window) ||
          meta_window_is_fullscreen (window) ||
          meta_window_get_monitor (window) != primary)
        continue;

      if (meta_window_get_maximize_flags (window) & META_MAXIMIZE_VERTICAL)
        {
          result = window;
          break;
        }
    }

  g_slist_free (sorted);
  return result;
}

/* Returns the dominant color of a thin strip just below the top edge of the
 * window, which for most apps is the header bar / tab strip. Using the most
 * common color rather than the average ignores titles, icons and buttons. */
static char *
sample_window_color (MetaWindow *window)
{
  MetaWindowActor *actor = META_WINDOW_ACTOR (meta_window_get_compositor_private (window));
  MtkRectangle frame, buffer, clip;
  cairo_surface_t *surface;
  g_autofree guint *counts = NULL;
  g_autofree guint64 *sums = NULL;
  guint best = 0, best_count = 0;
  const guint8 *data;
  int width, height, stride, x, y;
  char *color = NULL;

  if (!actor || meta_window_actor_is_destroyed (actor))
    return NULL;

  meta_window_get_frame_rect (window, &frame);
  meta_window_get_buffer_rect (window, &buffer);

  /* Actor coordinates are relative to the buffer, which can be larger than
   * the frame (client-side shadows). */
  clip.x = frame.x - buffer.x;
  clip.y = frame.y - buffer.y + SAMPLE_Y;
  clip.width = frame.width;
  clip.height = SAMPLE_HEIGHT;

  surface = meta_window_actor_get_image (actor, &clip);
  if (!surface)
    return NULL;

  if (cairo_surface_get_type (surface) != CAIRO_SURFACE_TYPE_IMAGE ||
      cairo_image_surface_get_format (surface) != CAIRO_FORMAT_ARGB32)
    goto out;

  cairo_surface_flush (surface);
  data = cairo_image_surface_get_data (surface);
  width = cairo_image_surface_get_width (surface);
  height = cairo_image_surface_get_height (surface);
  stride = cairo_image_surface_get_stride (surface);

  /* 15-bit color histogram, plus per-bin channel sums to recover the exact
   * color of the winning bin. */
  counts = g_new0 (guint, 1 << 15);
  sums = g_new0 (guint64, (1 << 15) * 3);

  for (y = 0; y < height; y++)
    {
      const guint32 *row = (const guint32 *) (data + y * stride);

      for (x = 0; x < width; x++)
        {
          guint32 px = row[x];
          guint a = px >> 24;
          guint r = (px >> 16) & 0xff;
          guint g = (px >> 8) & 0xff;
          guint b = px & 0xff;
          guint bin;

          if (a < 128)
            continue;
          if (a < 255)
            {
              r = r * 255 / a;
              g = g * 255 / a;
              b = b * 255 / a;
            }

          bin = ((r >> 3) << 10) | ((g >> 3) << 5) | (b >> 3);
          counts[bin]++;
          sums[bin * 3] += r;
          sums[bin * 3 + 1] += g;
          sums[bin * 3 + 2] += b;

          if (counts[bin] > best_count)
            {
              best_count = counts[bin];
              best = bin;
            }
        }
    }

  if (best_count > 0)
    color = g_strdup_printf ("#%02x%02x%02x",
                             (guint) (sums[best * 3] / best_count),
                             (guint) (sums[best * 3 + 1] / best_count),
                             (guint) (sums[best * 3 + 2] / best_count));

out:
  cairo_surface_destroy (surface);
  return color;
}

static gboolean
resample_cb (gpointer user_data);

static void
update_panel_state (ShellPlusPlugin *self)
{
  MetaWindow *maximized = find_maximized_window (self);

  self->solid = maximized != NULL;
  if (maximized)
    {
      g_autofree char *color = sample_window_color (maximized);

      /* Keep the previous color if sampling failed (e.g. no frame yet). */
      if (color)
        {
          g_free (self->color);
          self->color = g_steal_pointer (&color);
        }
    }

  if (self->solid && !self->resample_id)
    self->resample_id = g_timeout_add (RESAMPLE_INTERVAL_MS, resample_cb, self);
  else if (!self->solid)
    g_clear_handle_id (&self->resample_id, g_source_remove);

  publish_state (self);
}

static gboolean
resample_cb (gpointer user_data)
{
  ShellPlusPlugin *self = user_data;

  self->resample_id = 0;
  update_panel_state (self);
  return G_SOURCE_REMOVE;
}

static gboolean
update_cb (gpointer user_data)
{
  ShellPlusPlugin *self = user_data;

  self->update_id = 0;
  update_panel_state (self);
  return G_SOURCE_REMOVE;
}

static gboolean
followup_update_cb (gpointer user_data)
{
  ShellPlusPlugin *self = user_data;

  self->followup_update_id = 0;
  update_panel_state (self);
  return G_SOURCE_REMOVE;
}

static void
queue_update (ShellPlusPlugin *self)
{
  if (!self->update_id)
    self->update_id = g_timeout_add (UPDATE_DELAY_MS, update_cb, self);

  g_clear_handle_id (&self->followup_update_id, g_source_remove);
  self->followup_update_id =
    g_timeout_add (FOLLOWUP_UPDATE_DELAY_MS, followup_update_cb, self);
}

/* ------------------------------------------------------------------------ */
/* Dynamic workspaces (GNOME Shell keeps exactly one empty workspace at the */
/* end; mutter leaves that policy to the shell)                             */

static gboolean
check_workspaces_cb (gpointer user_data)
{
  ShellPlusPlugin *self = user_data;
  MetaDisplay *display = get_display (self);
  MetaWorkspaceManager *wm = get_workspace_manager (self);
  int n = meta_workspace_manager_get_n_workspaces (wm);
  int active = meta_workspace_manager_get_active_workspace_index (wm);
  guint32 timestamp = meta_display_get_current_time_roundtrip (display);
  g_autofree gboolean *empty = g_new (gboolean, n);
  GList *windows, *l;
  int i;

  self->workspace_check_id = 0;

  if (!g_settings_get_boolean (self->mutter_settings, "dynamic-workspaces"))
    return G_SOURCE_REMOVE;

  for (i = 0; i < n; i++)
    empty[i] = TRUE;

  windows = meta_display_list_all_windows (display);
  for (l = windows; l; l = l->next)
    {
      MetaWindow *window = l->data;
      MetaWorkspace *workspace;

      if (window == self->panel_window ||
          meta_window_is_on_all_workspaces (window) ||
          meta_window_is_skip_taskbar (window))
        continue;

      workspace = meta_window_get_workspace (window);
      if (workspace)
        empty[meta_workspace_index (workspace)] = FALSE;
    }
  g_list_free (windows);

  if (!empty[n - 1])
    meta_workspace_manager_append_new_workspace (wm, FALSE, timestamp);

  /* Iterate backwards so indices of the remaining workspaces stay valid. */
  for (i = n - 2; i >= 0; i--)
    {
      if (empty[i] && i != active)
        meta_workspace_manager_remove_workspace (wm,
                                                 meta_workspace_manager_get_workspace_by_index (wm, i),
                                                 timestamp);
    }

  return G_SOURCE_REMOVE;
}

static void
queue_workspace_check (ShellPlusPlugin *self)
{
  if (!self->workspace_check_id)
    self->workspace_check_id = g_idle_add (check_workspaces_cb, self);
}

/* ------------------------------------------------------------------------ */
/* Panel window management                                                  */

static void
update_struts (ShellPlusPlugin *self)
{
  MetaWorkspaceManager *wm = get_workspace_manager (self);
  GList *workspaces = meta_workspace_manager_get_workspaces (wm);
  GSList *struts = NULL;
  MetaStrut strut;
  GList *l;

  if (self->panel_window && self->panel_height > 0 &&
      get_primary_geometry (self, &strut.rect))
    {
      strut.rect.height = self->panel_height;
      strut.side = META_SIDE_TOP;
      struts = g_slist_prepend (NULL, &strut);
    }

  for (l = workspaces; l; l = l->next)
    meta_workspace_set_builtin_struts (l->data, struts);

  g_slist_free (struts);
}

static void
sync_panel_geometry (ShellPlusPlugin *self)
{
  MtkRectangle monitor, frame;

  if (!self->panel_window || !get_primary_geometry (self, &monitor))
    return;

  meta_window_get_frame_rect (self->panel_window, &frame);

  if (frame.x != monitor.x || frame.y != monitor.y || frame.width != monitor.width)
    meta_window_move_resize_frame (self->panel_window, FALSE,
                                   monitor.x, monitor.y, monitor.width, frame.height);

  if (frame.height != self->panel_height)
    {
      self->panel_height = frame.height;
      update_struts (self);
    }
}

static void
on_panel_size_changed (MetaWindow      *window,
                       ShellPlusPlugin *self)
{
  sync_panel_geometry (self);
}

static void
on_panel_unmanaged (MetaWindow      *window,
                    ShellPlusPlugin *self)
{
  if (window != self->panel_window)
    return;

  self->panel_window = NULL;
  self->panel_height = 0;
  update_struts (self);
}

static void
setup_panel_window (ShellPlusPlugin *self,
                    MetaWindow      *window)
{
  self->panel_window = window;

  meta_window_set_type (window, META_WINDOW_DOCK);
  meta_window_hide_from_window_list (window);
  meta_window_stick (window);
  meta_window_add_external_constraint (window, self->panel_constraint);

  g_signal_connect_object (window, "size-changed",
                           G_CALLBACK (on_panel_size_changed), self, 0);
  g_signal_connect_object (window, "unmanaged",
                           G_CALLBACK (on_panel_unmanaged), self, 0);
}

static gboolean
respawn_panel_cb (gpointer user_data)
{
  ShellPlusPlugin *self = user_data;

  self->panel_respawn_id = 0;
  spawn_panel (self);
  return G_SOURCE_REMOVE;
}

static void
on_panel_exited (GObject      *source,
                 GAsyncResult *result,
                 gpointer      user_data)
{
  ShellPlusPlugin *self = user_data;
  GSubprocess *subprocess = G_SUBPROCESS (source);
  g_autoptr (GError) error = NULL;

  if (!g_subprocess_wait_finish (subprocess, result, &error))
    return;

  g_clear_object (&self->panel_client);

  if (g_get_monotonic_time () - self->panel_spawn_time < PANEL_QUICK_CRASH_US)
    self->panel_quick_crashes++;
  else
    self->panel_quick_crashes = 0;

  if (self->panel_quick_crashes >= PANEL_MAX_QUICK_CRASHES)
    {
      g_warning ("Panel keeps exiting; not restarting it again");
      return;
    }

  g_warning ("Panel exited; restarting it");
  self->panel_respawn_id = g_timeout_add (PANEL_RESPAWN_DELAY_MS, respawn_panel_cb, self);
}

static void
spawn_panel (ShellPlusPlugin *self)
{
  MetaContext *context = meta_display_get_context (get_display (self));
  g_autoptr (GSubprocessLauncher) launcher = NULL;
  g_autoptr (GError) error = NULL;
  const char *panel = g_getenv ("SHELLPLUS_PANEL");
  const char *argv[] = { NULL, NULL };

  argv[0] = panel && *panel ? panel : SHELLPLUS_PANEL_BINARY;

  launcher = g_subprocess_launcher_new (G_SUBPROCESS_FLAGS_NONE);
  g_subprocess_launcher_setenv (launcher, "GDK_BACKEND", "wayland", TRUE);

  self->panel_client = meta_wayland_client_new_subprocess (context, launcher, argv, &error);
  if (!self->panel_client)
    {
      g_warning ("Failed to launch panel '%s': %s", argv[0], error->message);
      return;
    }

  self->panel_spawn_time = g_get_monotonic_time ();
  g_subprocess_wait_async (meta_wayland_client_get_subprocess (self->panel_client),
                           NULL, on_panel_exited, self);
}

/* ------------------------------------------------------------------------ */
/* Signal plumbing                                                          */

static void
on_window_changed (ShellPlusPlugin *self)
{
  queue_update (self);
  queue_workspace_check (self);
}

static void
on_window_created (MetaDisplay     *display,
                   MetaWindow      *window,
                   ShellPlusPlugin *self)
{
  if (!self->panel_window &&
      self->panel_client &&
      meta_window_get_window_type (window) == META_WINDOW_NORMAL &&
      !meta_window_get_transient_for (window) &&
      meta_wayland_client_owns_window (self->panel_client, window))
    {
      setup_panel_window (self, window);
      return;
    }

  g_signal_connect_object (window, "notify::maximized-vertically",
                           G_CALLBACK (queue_update), self, G_CONNECT_SWAPPED);
  g_signal_connect_object (window, "notify::minimized",
                           G_CALLBACK (queue_update), self, G_CONNECT_SWAPPED);
  g_signal_connect_object (window, "notify::appears-focused",
                           G_CALLBACK (queue_update), self, G_CONNECT_SWAPPED);
  g_signal_connect_object (window, "size-changed",
                           G_CALLBACK (queue_update), self, G_CONNECT_SWAPPED);
  g_signal_connect_object (window, "workspace-changed",
                           G_CALLBACK (on_window_changed), self, G_CONNECT_SWAPPED);
  g_signal_connect_object (window, "unmanaged",
                           G_CALLBACK (on_window_changed), self, G_CONNECT_SWAPPED);

  on_window_changed (self);
}

static void
on_workspaces_changed (ShellPlusPlugin *self)
{
  update_struts (self);
  queue_update (self);
  queue_workspace_check (self);
}

static void
on_monitors_changed (ShellPlusPlugin *self)
{
  sync_panel_geometry (self);
  update_struts (self);
  queue_update (self);
}

/* ------------------------------------------------------------------------ */
/* MetaPlugin vfuncs                                                        */

static void
shellplus_plugin_start (MetaPlugin *plugin)
{
  ShellPlusPlugin *self = SHELLPLUS_PLUGIN (plugin);
  MetaDisplay *display = get_display (self);
  MetaCompositor *compositor = meta_display_get_compositor (display);
  MetaBackend *backend = meta_context_get_backend (meta_display_get_context (display));
  MetaWorkspaceManager *wm = get_workspace_manager (self);

  self->background = shellplus_background_new (display,
                                               meta_compositor_get_window_group (compositor));
  self->panel_constraint = shellplus_panel_constraint_new (display);
  self->mutter_settings = g_settings_new ("org.gnome.mutter");
  self->dbus = shellplus_dbus_new (&dbus_handlers, self);

  g_signal_connect_object (display, "window-created",
                           G_CALLBACK (on_window_created), self, 0);
  g_signal_connect_object (display, "restacked",
                           G_CALLBACK (queue_update), self, G_CONNECT_SWAPPED);
  g_signal_connect_object (wm, "active-workspace-changed",
                           G_CALLBACK (on_workspaces_changed), self, G_CONNECT_SWAPPED);
  g_signal_connect_object (wm, "workspace-added",
                           G_CALLBACK (on_workspaces_changed), self, G_CONNECT_SWAPPED);
  g_signal_connect_object (wm, "workspace-removed",
                           G_CALLBACK (on_workspaces_changed), self, G_CONNECT_SWAPPED);
  g_signal_connect_object (meta_backend_get_monitor_manager (backend), "monitors-changed",
                           G_CALLBACK (on_monitors_changed), self, G_CONNECT_SWAPPED);
  g_signal_connect_object (self->mutter_settings, "changed::dynamic-workspaces",
                           G_CALLBACK (queue_workspace_check), self, G_CONNECT_SWAPPED);

  spawn_panel (self);
  queue_workspace_check (self);
  queue_update (self);

  clutter_actor_show (CLUTTER_ACTOR (meta_compositor_get_stage (compositor)));
}

static void
shellplus_plugin_map (MetaPlugin      *plugin,
                      MetaWindowActor *actor)
{
  ShellPlusPlugin *self = SHELLPLUS_PLUGIN (plugin);

  if (meta_window_actor_get_meta_window (actor) == self->panel_window)
    sync_panel_geometry (self);

  meta_plugin_map_completed (plugin, actor);
}

static void
shellplus_plugin_finalize (GObject *object)
{
  ShellPlusPlugin *self = SHELLPLUS_PLUGIN (object);

  g_clear_handle_id (&self->update_id, g_source_remove);
  g_clear_handle_id (&self->followup_update_id, g_source_remove);
  g_clear_handle_id (&self->resample_id, g_source_remove);
  g_clear_handle_id (&self->workspace_check_id, g_source_remove);
  g_clear_handle_id (&self->panel_respawn_id, g_source_remove);
  g_clear_pointer (&self->dbus, shellplus_dbus_free);
  g_clear_pointer (&self->background, shellplus_background_free);
  g_clear_pointer (&self->last_state, g_variant_unref);
  g_clear_object (&self->panel_client);
  g_clear_object (&self->panel_constraint);
  g_clear_object (&self->mutter_settings);
  g_free (self->color);

  G_OBJECT_CLASS (shellplus_plugin_parent_class)->finalize (object);
}

static void
shellplus_plugin_class_init (ShellPlusPluginClass *klass)
{
  GObjectClass *object_class = G_OBJECT_CLASS (klass);
  MetaPluginClass *plugin_class = META_PLUGIN_CLASS (klass);

  object_class->finalize = shellplus_plugin_finalize;

  plugin_class->start = shellplus_plugin_start;
  plugin_class->map = shellplus_plugin_map;
}

static void
shellplus_plugin_init (ShellPlusPlugin *self)
{
}
