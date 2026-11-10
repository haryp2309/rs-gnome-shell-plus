#define G_LOG_DOMAIN "shellplus"
#define _GNU_SOURCE

#include "shellplus-screenshot.h"

#include <cairo.h>
#include <errno.h>
#include <fcntl.h>
#include <gio/gunixfdlist.h>
#include <meta/compositor.h>
#include <meta/meta-selection.h>
#include <meta/meta-selection-source-memory.h>
#include <meta/meta-window-actor.h>
#include <meta/window.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include "shellplus-dbus.h"

/* Screenshots.
 *
 * Public: org.gnome.Shell.Screenshot on /org/gnome/Shell/Screenshot (bus
 * name org.gnome.Shell.Screenshot), the interface GNOME Shell exports for
 * xdg-desktop-portal-gnome and gnome-screenshot: Screenshot,
 * ScreenshotWindow, ScreenshotArea, FlashArea, SelectArea, PickColor and
 * InteractiveScreenshot. The last three need the user, so they're relayed
 * to the panel, which shows its screenshot UI and reports back (Finish).
 *
 * Who may call it. GNOME Shell lets only the senders that own certain
 * well-known names through (the media keys daemon and the GNOME portal
 * backend), except for PickColor. Owning a name proves little, so this
 * checks the caller's executable instead: the sender's pid
 * (GetConnectionUnixProcessID) → /proc/<pid>/exe must
 *   - have one of the basenames in allowed_executables
 *     (xdg-desktop-portal-gnome, xdg-desktop-portal-gtk, gnome-screenshot,
 *     gsd-media-keys),
 *   - live under /usr/, still exist (not "(deleted)"), and
 *   - be owned by root and not writable by group or others,
 * so a user can't pass the check with a copied or renamed binary of their
 * own. PickColor is open to everyone, as in GNOME Shell (it always asks
 * the user to click). Sandboxed (Flatpak) apps go through the portal.
 * SHELLPLUS_SCREENSHOT_ALLOW_ALL=1 turns the check off (tests only).
 *
 * Private: org.gnome.ShellPlus.Screenshot on /org/gnome/ShellPlus, for the
 * panel only (the caller's pid must be the panel's):
 *
 *   CaptureScreen(b include_cursor) -> (h image, i width, i height,
 *       i stride, d scale)
 *       The whole stage (every monitor), at the largest monitor scale, as
 *       a memfd holding cairo ARGB32 (premultiplied BGRA on little endian).
 *   CaptureWindow(t id, b include_frame) -> (h image, i width, i height,
 *       i stride)
 *       One window at full size (id 0: the focused one), no shadows.
 *   Flash(i x, i y, i width, i height)   the white flash over an area
 *   ShowUI(s mode)                       opens the UI ("", "screen", "window")
 *   Finish(u request, b success, a{sv} result)
 *       Ends a request the panel was given with ShowUI: "rect" (iiii) for
 *       select-area, "color" (ddd) for pick-color, "uri" s for interactive.
 *
 *   ShowUI(u request, s mode)   signal: show the UI. request is 0 for the
 *       keyboard shortcut/ShowUI, else a D-Bus request to Finish; mode is
 *       "", "screen", "window", "select-area", "pick-color" or "interactive".
 *   ScreenshotTaken(s path)     signal: a Shift+Print / Alt+Print shot was
 *       saved (and copied); the panel posts the notification.
 *
 * Key bindings, from org.gnome.shell.keybindings when it's installed:
 * show-screenshot-ui (Print), screenshot (Shift+Print: the whole screen
 * right away) and screenshot-window (Alt+Print: the focused window right
 * away), like GNOME Shell.
 */

#define PUBLIC_NAME  "org.gnome.Shell.Screenshot"
#define PUBLIC_PATH  "/org/gnome/Shell/Screenshot"
#define PUBLIC_IFACE "org.gnome.Shell.Screenshot"
#define PRIVATE_IFACE "org.gnome.ShellPlus.Screenshot"

#define FLASH_TIME_MS 500

static const char public_xml[] =
  "<node>"
  "  <interface name='" PUBLIC_IFACE "'>"
  "    <method name='InteractiveScreenshot'>"
  "      <arg type='b' direction='out' name='success'/>"
  "      <arg type='s' direction='out' name='uri'/>"
  "    </method>"
  "    <method name='Screenshot'>"
  "      <arg type='b' direction='in' name='include_cursor'/>"
  "      <arg type='b' direction='in' name='flash'/>"
  "      <arg type='s' direction='in' name='filename'/>"
  "      <arg type='b' direction='out' name='success'/>"
  "      <arg type='s' direction='out' name='filename_used'/>"
  "    </method>"
  "    <method name='ScreenshotWindow'>"
  "      <arg type='b' direction='in' name='include_frame'/>"
  "      <arg type='b' direction='in' name='include_cursor'/>"
  "      <arg type='b' direction='in' name='flash'/>"
  "      <arg type='s' direction='in' name='filename'/>"
  "      <arg type='b' direction='out' name='success'/>"
  "      <arg type='s' direction='out' name='filename_used'/>"
  "    </method>"
  "    <method name='ScreenshotArea'>"
  "      <arg type='i' direction='in' name='x'/>"
  "      <arg type='i' direction='in' name='y'/>"
  "      <arg type='i' direction='in' name='width'/>"
  "      <arg type='i' direction='in' name='height'/>"
  "      <arg type='b' direction='in' name='flash'/>"
  "      <arg type='s' direction='in' name='filename'/>"
  "      <arg type='b' direction='out' name='success'/>"
  "      <arg type='s' direction='out' name='filename_used'/>"
  "    </method>"
  "    <method name='PickColor'>"
  "      <arg type='a{sv}' direction='out' name='result'/>"
  "    </method>"
  "    <method name='FlashArea'>"
  "      <arg type='i' direction='in' name='x'/>"
  "      <arg type='i' direction='in' name='y'/>"
  "      <arg type='i' direction='in' name='width'/>"
  "      <arg type='i' direction='in' name='height'/>"
  "    </method>"
  "    <method name='SelectArea'>"
  "      <arg type='i' direction='out' name='x'/>"
  "      <arg type='i' direction='out' name='y'/>"
  "      <arg type='i' direction='out' name='width'/>"
  "      <arg type='i' direction='out' name='height'/>"
  "    </method>"
  "  </interface>"
  "</node>";

static const char private_xml[] =
  "<node>"
  "  <interface name='" PRIVATE_IFACE "'>"
  "    <method name='CaptureScreen'>"
  "      <arg type='b' name='include_cursor' direction='in'/>"
  "      <arg type='h' name='image' direction='out'/>"
  "      <arg type='i' name='width' direction='out'/>"
  "      <arg type='i' name='height' direction='out'/>"
  "      <arg type='i' name='stride' direction='out'/>"
  "      <arg type='d' name='scale' direction='out'/>"
  "    </method>"
  "    <method name='CaptureWindow'>"
  "      <arg type='t' name='id' direction='in'/>"
  "      <arg type='b' name='include_frame' direction='in'/>"
  "      <arg type='h' name='image' direction='out'/>"
  "      <arg type='i' name='width' direction='out'/>"
  "      <arg type='i' name='height' direction='out'/>"
  "      <arg type='i' name='stride' direction='out'/>"
  "    </method>"
  "    <method name='Flash'>"
  "      <arg type='i' name='x' direction='in'/>"
  "      <arg type='i' name='y' direction='in'/>"
  "      <arg type='i' name='width' direction='in'/>"
  "      <arg type='i' name='height' direction='in'/>"
  "    </method>"
  "    <method name='ShowUI'>"
  "      <arg type='s' name='mode' direction='in'/>"
  "    </method>"
  "    <method name='Finish'>"
  "      <arg type='u' name='request' direction='in'/>"
  "      <arg type='b' name='success' direction='in'/>"
  "      <arg type='a{sv}' name='result' direction='in'/>"
  "    </method>"
  "    <signal name='ShowUI'>"
  "      <arg type='u' name='request'/>"
  "      <arg type='s' name='mode'/>"
  "    </signal>"
  "    <signal name='ScreenshotTaken'>"
  "      <arg type='s' name='path'/>"
  "    </signal>"
  "  </interface>"
  "</node>";

static const char *const allowed_executables[] = {
  "xdg-desktop-portal-gnome",
  "xdg-desktop-portal-gtk",
  "gnome-screenshot",
  "gsd-media-keys",
  NULL,
};

struct _ShellPlusScreenshot
{
  MetaDisplay *display;
  ShellPlusScreenshotPanelPid panel_pid;
  gpointer user_data;

  GDBusNodeInfo *public_info;
  GDBusNodeInfo *private_info;
  GDBusConnection *connection;
  guint owner_id;
  guint public_registration;
  guint private_registration;
  GCancellable *cancellable;

  GSettings *shell_keybindings;

  /* While the screen is locked nothing may be captured: hidden windows
   * would still be readable. */
  gboolean blocked;

  /* The one UI request (SelectArea, PickColor, InteractiveScreenshot)
   * waiting for the panel. */
  GDBusMethodInvocation *pending;
  char *pending_mode;
  guint pending_id;
  guint next_id;
};

/* ------------------------------------------------------------------------ */
/* Captures                                                                 */

static ClutterStage *
get_stage (ShellPlusScreenshot *self)
{
  return meta_compositor_get_stage (meta_display_get_compositor (self->display));
}

static MtkRectangle
stage_rect (ShellPlusScreenshot *self)
{
  MtkRectangle rect = { 0, 0, 0, 0 };

  meta_display_get_size (self->display, &rect.width, &rect.height);
  return rect;
}

/* Paints `area` (stage coordinates) at the largest scale of the monitors
 * it touches. */
static cairo_surface_t *
capture_area (ShellPlusScreenshot *self,
              const MtkRectangle  *area,
              gboolean             include_cursor,
              float               *out_scale,
              GError             **error)
{
  ClutterStage *stage = get_stage (self);
  MtkRectangle rect = *area;
  ClutterPaintFlag flags = CLUTTER_PAINT_FLAG_CLEAR;
  cairo_surface_t *surface;
  int width, height;
  float scale;

  if (!clutter_stage_get_capture_final_size (stage, &rect, &width, &height, &scale))
    {
      g_set_error (error, G_IO_ERROR, G_IO_ERROR_FAILED, "The area is off screen");
      return NULL;
    }

  flags |= include_cursor ? CLUTTER_PAINT_FLAG_FORCE_CURSORS : CLUTTER_PAINT_FLAG_NO_CURSORS;
  surface = cairo_image_surface_create (CAIRO_FORMAT_ARGB32, width, height);
  if (!clutter_stage_paint_to_buffer (stage, &rect, scale,
                                      cairo_image_surface_get_data (surface),
                                      cairo_image_surface_get_stride (surface),
                                      COGL_PIXEL_FORMAT_CAIRO_ARGB32_COMPAT,
                                      clutter_actor_get_color_state (CLUTTER_ACTOR (stage)),
                                      flags, error))
    {
      cairo_surface_destroy (surface);
      return NULL;
    }
  cairo_surface_mark_dirty (surface);
  if (out_scale)
    *out_scale = scale;
  return surface;
}

static MetaWindow *
find_window (ShellPlusScreenshot *self,
             guint64              id)
{
  g_autoptr (GList) windows = NULL;

  if (id == 0)
    return meta_display_get_focus_window (self->display);

  windows = meta_display_list_all_windows (self->display);
  for (GList *l = windows; l; l = l->next)
    {
      if (meta_window_get_id (l->data) == id)
        return l->data;
    }
  return NULL;
}

/* The window's frame rectangle (or only its client area), at buffer
 * scale, without shadows. */
static cairo_surface_t *
capture_window (MetaWindow  *window,
                gboolean     include_frame,
                MtkRectangle *out_rect,
                GError     **error)
{
  GObject *actor = window ? meta_window_get_compositor_private (window) : NULL;
  MtkRectangle rect, buffer, clip;
  cairo_surface_t *image;

  if (!actor)
    {
      g_set_error (error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND, "No window to capture");
      return NULL;
    }

  meta_window_get_frame_rect (window, &rect);
  if (!include_frame)
    meta_window_frame_rect_to_client_rect (window, &rect, &rect);
  meta_window_get_buffer_rect (window, &buffer);
  clip = (MtkRectangle) { rect.x - buffer.x, rect.y - buffer.y, rect.width, rect.height };
  image = meta_window_actor_get_image (META_WINDOW_ACTOR (actor), &clip);
  if (!image || cairo_surface_status (image) != CAIRO_STATUS_SUCCESS ||
      cairo_image_surface_get_format (image) != CAIRO_FORMAT_ARGB32)
    {
      g_clear_pointer (&image, cairo_surface_destroy);
      g_set_error (error, G_IO_ERROR, G_IO_ERROR_FAILED, "Could not read the window's contents");
      return NULL;
    }
  cairo_surface_flush (image);
  if (out_rect)
    *out_rect = rect;
  return image;
}

/* Copies an ARGB32 image surface into a memfd. Returns -1 on failure. */
static int
surface_to_memfd (cairo_surface_t *surface,
                  GError         **error)
{
  int stride = cairo_image_surface_get_stride (surface);
  size_t size = (size_t) stride * cairo_image_surface_get_height (surface);
  int fd = memfd_create ("shellplus-screenshot", MFD_CLOEXEC);
  void *data;

  if (fd < 0 || ftruncate (fd, size) < 0 ||
      (data = mmap (NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0)) == MAP_FAILED)
    {
      g_set_error (error, G_IO_ERROR, g_io_error_from_errno (errno),
                   "Could not create the image buffer: %s", g_strerror (errno));
      if (fd >= 0)
        close (fd);
      return -1;
    }
  cairo_surface_flush (surface);
  memcpy (data, cairo_image_surface_get_data (surface), size);
  munmap (data, size);
  return fd;
}

static void
flash_area (ShellPlusScreenshot *self,
            const MtkRectangle  *rect)
{
  ClutterActor *stage = CLUTTER_ACTOR (get_stage (self));
  ClutterActor *flash = clutter_actor_new ();
  CoglColor white;

  cogl_color_init_from_4f (&white, 1.0, 1.0, 1.0, 1.0);
  clutter_actor_set_background_color (flash, &white);
  clutter_actor_set_position (flash, rect->x, rect->y);
  clutter_actor_set_size (flash, rect->width, rect->height);
  clutter_actor_set_reactive (flash, FALSE);
  clutter_actor_add_child (stage, flash);

  clutter_actor_save_easing_state (flash);
  clutter_actor_set_easing_mode (flash, CLUTTER_EASE_OUT_QUAD);
  clutter_actor_set_easing_duration (flash, FLASH_TIME_MS);
  clutter_actor_set_opacity (flash, 0);
  clutter_actor_restore_easing_state (flash);
  g_signal_connect (flash, "transitions-completed", G_CALLBACK (clutter_actor_destroy), NULL);
}

/* ------------------------------------------------------------------------ */
/* Saving                                                                   */

typedef struct
{
  cairo_surface_t *surface;
  char *filename;   /* as requested; "" = clipboard */
  char *used;       /* out */
  GBytes *png;      /* out, for the clipboard */
} SaveJob;

static void
save_job_free (SaveJob *job)
{
  cairo_surface_destroy (job->surface);
  g_free (job->filename);
  g_free (job->used);
  g_clear_pointer (&job->png, g_bytes_unref);
  g_free (job);
}

static cairo_status_t
append_png (void                *closure,
            const unsigned char *data,
            unsigned int         length)
{
  g_byte_array_append (closure, data, length);
  return CAIRO_STATUS_SUCCESS;
}

/* Where a relative name goes: ~/Pictures/Screenshots (the XDG pictures
 * directory, else ~/Pictures). */
static char *
screenshots_dir (void)
{
  const char *pictures = g_get_user_special_dir (G_USER_DIRECTORY_PICTURES);

  if (pictures && *pictures && g_strcmp0 (pictures, g_get_home_dir ()) != 0)
    return g_build_filename (pictures, "Screenshots", NULL);
  return g_build_filename (g_get_home_dir (), "Pictures", "Screenshots", NULL);
}

/* Creates a new file for a relative name: <name>.png, <name>-1.png, … */
static char *
create_unique (const char *name,
               GError    **error)
{
  g_autofree char *dir = screenshots_dir ();
  g_autofree char *base = g_str_has_suffix (name, ".png") ?
                          g_strndup (name, strlen (name) - 4) : g_strdup (name);

  if (g_mkdir_with_parents (dir, 0755) < 0)
    {
      g_set_error (error, G_IO_ERROR, g_io_error_from_errno (errno),
                   "Could not create %s: %s", dir, g_strerror (errno));
      return NULL;
    }

  for (int i = 0; i < 1000; i++)
    {
      g_autofree char *file = i ? g_strdup_printf ("%s-%d.png", base, i) :
                                  g_strdup_printf ("%s.png", base);
      g_autofree char *path = g_build_filename (dir, file, NULL);
      int fd = open (path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0644);

      if (fd >= 0)
        {
          close (fd);
          return g_steal_pointer (&path);
        }
      if (errno != EEXIST)
        {
          g_set_error (error, G_IO_ERROR, g_io_error_from_errno (errno),
                       "Could not create %s: %s", path, g_strerror (errno));
          return NULL;
        }
    }
  g_set_error (error, G_IO_ERROR, G_IO_ERROR_EXISTS, "Too many screenshots named %s", base);
  return NULL;
}

static void
save_thread (GTask        *task,
             gpointer      source,
             gpointer      task_data,
             GCancellable *cancellable)
{
  SaveJob *job = task_data;
  GError *error = NULL;
  cairo_status_t status;

  if (!*job->filename)
    {
      GByteArray *bytes = g_byte_array_new ();

      status = cairo_surface_write_to_png_stream (job->surface, append_png, bytes);
      job->png = g_byte_array_free_to_bytes (bytes);
      job->used = g_strdup ("");
    }
  else
    {
      if (g_path_is_absolute (job->filename))
        job->used = g_strdup (job->filename);
      else if (!(job->used = create_unique (job->filename, &error)))
        {
          g_task_return_error (task, error);
          return;
        }
      status = cairo_surface_write_to_png (job->surface, job->used);
    }

  if (status != CAIRO_STATUS_SUCCESS)
    g_task_return_new_error (task, G_IO_ERROR, G_IO_ERROR_FAILED, "Could not write %s: %s",
                             *job->used ? job->used : "the image",
                             cairo_status_to_string (status));
  else
    g_task_return_boolean (task, TRUE);
}

static void
set_clipboard (ShellPlusScreenshot *self,
               GBytes              *png)
{
  g_autoptr (GError) error = NULL;
  MetaSelectionSource *source = meta_selection_source_memory_new ("image/png", png, &error);

  if (!source)
    {
      g_warning ("Could not copy the screenshot: %s", error->message);
      return;
    }
  meta_selection_set_owner (meta_display_get_selection (self->display),
                            META_SELECTION_CLIPBOARD, source);
  g_object_unref (source);
}

typedef void (*SavedFunc) (ShellPlusScreenshot *self,
                           const char          *used,
                           const GError        *error,
                           gpointer             data);

typedef struct
{
  ShellPlusScreenshot *self;
  SavedFunc func;
  gpointer data;
} SaveClosure;

static void
on_saved (GObject      *source,
          GAsyncResult *result,
          gpointer      user_data)
{
  SaveClosure *closure = user_data;
  SaveJob *job = g_task_get_task_data (G_TASK (result));
  g_autoptr (GError) error = NULL;

  if (!g_task_propagate_boolean (G_TASK (result), &error) &&
      g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
    {
      /* Shutting down; self is gone. */
      if (closure->data)
        g_object_unref (closure->data);
      g_free (closure);
      return;
    }

  if (!error && job->png)
    set_clipboard (closure->self, job->png);
  closure->func (closure->self, error ? NULL : job->used, error, closure->data);
  g_free (closure);
}

/* Takes the surface. `data` is a GObject (or NULL) that func unrefs. */
static void
save_async (ShellPlusScreenshot *self,
            cairo_surface_t     *surface,
            const char          *filename,
            SavedFunc            func,
            gpointer             data)
{
  SaveJob *job = g_new0 (SaveJob, 1);
  SaveClosure *closure = g_new0 (SaveClosure, 1);
  g_autoptr (GTask) task = NULL;

  job->surface = surface;
  job->filename = g_strdup (filename);
  closure->self = self;
  closure->func = func;
  closure->data = data;

  task = g_task_new (NULL, self->cancellable, on_saved, closure);
  g_task_set_return_on_cancel (task, FALSE);
  g_task_set_task_data (task, job, (GDestroyNotify) save_job_free);
  g_task_run_in_thread (task, save_thread);
}

static void
reply_saved (ShellPlusScreenshot *self,
             const char          *used,
             const GError        *error,
             gpointer             data)
{
  GDBusMethodInvocation *invocation = data;

  if (error)
    g_dbus_method_invocation_return_gerror (invocation, error);
  else
    g_dbus_method_invocation_return_value (invocation, g_variant_new ("(bs)", TRUE, used));
  g_object_unref (invocation);
}

/* ------------------------------------------------------------------------ */
/* Requests for the panel's UI                                              */

static void
emit_private (ShellPlusScreenshot *self,
              const char          *signal,
              GVariant            *params)
{
  if (!self->connection)
    {
      g_variant_unref (g_variant_ref_sink (params));
      return;
    }
  g_dbus_connection_emit_signal (self->connection, NULL, SHELLPLUS_DBUS_PATH,
                                 PRIVATE_IFACE, signal, params, NULL);
}

void
shellplus_screenshot_set_blocked (ShellPlusScreenshot *self,
                                  gboolean             blocked)
{
  self->blocked = blocked;
}

void
shellplus_screenshot_show_ui (ShellPlusScreenshot *self,
                              const char          *mode)
{
  if (self->blocked)
    return;
  emit_private (self, "ShowUI", g_variant_new ("(us)", 0, mode));
}

static void
return_cancelled (GDBusMethodInvocation *invocation)
{
  g_dbus_method_invocation_return_error_literal (invocation, G_IO_ERROR, G_IO_ERROR_CANCELLED,
                                                 "Operation was cancelled");
}

static void
end_pending (ShellPlusScreenshot *self,
             gboolean             success,
             GVariant            *result)
{
  GDBusMethodInvocation *invocation = g_steal_pointer (&self->pending);
  g_autofree char *mode = g_steal_pointer (&self->pending_mode);
  g_autoptr (GVariantDict) dict = g_variant_dict_new (result);

  self->pending_id = 0;
  if (!invocation)
    return;

  if (g_strcmp0 (mode, "interactive") == 0)
    {
      const char *uri = NULL;

      if (!success || !g_variant_dict_lookup (dict, "uri", "&s", &uri))
        uri = NULL;
      g_dbus_method_invocation_return_value (invocation,
                                             g_variant_new ("(bs)", uri != NULL, uri ? uri : ""));
    }
  else if (g_strcmp0 (mode, "select-area") == 0)
    {
      int x, y, w, h;

      if (success && g_variant_dict_lookup (dict, "rect", "(iiii)", &x, &y, &w, &h))
        g_dbus_method_invocation_return_value (invocation, g_variant_new ("(iiii)", x, y, w, h));
      else
        return_cancelled (invocation);
    }
  else /* pick-color */
    {
      double r, g, b;

      if (success && g_variant_dict_lookup (dict, "color", "(ddd)", &r, &g, &b))
        {
          GVariantBuilder builder;

          g_variant_builder_init (&builder, G_VARIANT_TYPE_VARDICT);
          g_variant_builder_add (&builder, "{sv}", "color", g_variant_new ("(ddd)", r, g, b));
          g_dbus_method_invocation_return_value (invocation, g_variant_new ("(a{sv})", &builder));
        }
      else
        return_cancelled (invocation);
    }
  g_object_unref (invocation);
}

static void
start_request (ShellPlusScreenshot   *self,
               GDBusMethodInvocation *invocation,
               const char            *mode)
{
  if (self->pending)
    {
      g_dbus_method_invocation_return_error_literal (invocation, G_IO_ERROR, G_IO_ERROR_BUSY,
                                                     "There is an ongoing operation");
      return;
    }
  self->pending = g_object_ref (invocation);
  self->pending_mode = g_strdup (mode);
  self->pending_id = ++self->next_id ? self->next_id : ++self->next_id;
  emit_private (self, "ShowUI", g_variant_new ("(us)", self->pending_id, mode));
}

/* ------------------------------------------------------------------------ */
/* Public methods                                                           */

static gboolean
area_is_valid (ShellPlusScreenshot *self,
               int x, int y, int width, int height)
{
  MtkRectangle stage = stage_rect (self);

  return x >= 0 && y >= 0 && width > 0 && height > 0 &&
         x + width <= stage.width && y + height <= stage.height;
}

static void
return_invalid (GDBusMethodInvocation *invocation)
{
  g_dbus_method_invocation_return_error_literal (invocation, G_IO_ERROR, G_IO_ERROR_CANCELLED,
                                                 "Invalid params");
}

static void
save_capture (ShellPlusScreenshot   *self,
              GDBusMethodInvocation *invocation,
              cairo_surface_t       *surface,
              const GError          *error,
              const char            *filename)
{
  if (!surface)
    {
      g_debug ("Screenshot failed: %s", error ? error->message : "?");
      g_dbus_method_invocation_return_value (invocation, g_variant_new ("(bs)", FALSE, ""));
      return;
    }
  save_async (self, surface, filename, reply_saved, g_object_ref (invocation));
}

static void
handle_public (ShellPlusScreenshot   *self,
               GDBusMethodInvocation *invocation)
{
  const char *method = g_dbus_method_invocation_get_method_name (invocation);
  GVariant *parameters = g_dbus_method_invocation_get_parameters (invocation);
  g_autoptr (GError) error = NULL;

  if (g_strcmp0 (method, "Screenshot") == 0)
    {
      MtkRectangle rect = stage_rect (self);
      gboolean cursor, flash;
      const char *filename;
      cairo_surface_t *surface;

      g_variant_get (parameters, "(bb&s)", &cursor, &flash, &filename);
      surface = capture_area (self, &rect, cursor, NULL, &error);
      if (surface && flash)
        flash_area (self, &rect);
      save_capture (self, invocation, surface, error, filename);
    }
  else if (g_strcmp0 (method, "ScreenshotWindow") == 0)
    {
      gboolean frame, cursor, flash;
      const char *filename;
      cairo_surface_t *surface;
      MtkRectangle rect;

      g_variant_get (parameters, "(bbb&s)", &frame, &cursor, &flash, &filename);
      surface = capture_window (meta_display_get_focus_window (self->display), frame, &rect, &error);
      if (surface && flash)
        flash_area (self, &rect);
      save_capture (self, invocation, surface, error, filename);
    }
  else if (g_strcmp0 (method, "ScreenshotArea") == 0)
    {
      MtkRectangle rect;
      gboolean flash;
      const char *filename;
      cairo_surface_t *surface;

      g_variant_get (parameters, "(iiiib&s)", &rect.x, &rect.y, &rect.width, &rect.height,
                     &flash, &filename);
      if (!area_is_valid (self, rect.x, rect.y, rect.width, rect.height))
        {
          return_invalid (invocation);
          return;
        }
      surface = capture_area (self, &rect, FALSE, NULL, &error);
      if (surface && flash)
        flash_area (self, &rect);
      save_capture (self, invocation, surface, error, filename);
    }
  else if (g_strcmp0 (method, "FlashArea") == 0)
    {
      MtkRectangle rect;

      g_variant_get (parameters, "(iiii)", &rect.x, &rect.y, &rect.width, &rect.height);
      if (!area_is_valid (self, rect.x, rect.y, rect.width, rect.height))
        {
          return_invalid (invocation);
          return;
        }
      flash_area (self, &rect);
      g_dbus_method_invocation_return_value (invocation, NULL);
    }
  else if (g_strcmp0 (method, "SelectArea") == 0)
    start_request (self, invocation, "select-area");
  else if (g_strcmp0 (method, "PickColor") == 0)
    start_request (self, invocation, "pick-color");
  else if (g_strcmp0 (method, "InteractiveScreenshot") == 0)
    start_request (self, invocation, "interactive");
  else
    g_dbus_method_invocation_return_error (invocation, G_DBUS_ERROR, G_DBUS_ERROR_UNKNOWN_METHOD,
                                           "Unknown method %s", method);
}

/* ------------------------------------------------------------------------ */
/* Private methods                                                          */

static void
return_image (GDBusMethodInvocation *invocation,
              cairo_surface_t       *surface,
              const GError          *error,
              gboolean               with_scale,
              double                 scale)
{
  g_autoptr (GError) fd_error = NULL;
  g_autoptr (GUnixFDList) fds = NULL;
  int fd;

  if (!surface)
    {
      g_dbus_method_invocation_return_gerror (invocation, error);
      return;
    }
  fd = surface_to_memfd (surface, &fd_error);
  if (fd < 0)
    {
      cairo_surface_destroy (surface);
      g_dbus_method_invocation_return_gerror (invocation, fd_error);
      return;
    }
  fds = g_unix_fd_list_new_from_array (&fd, 1);
  g_dbus_method_invocation_return_value_with_unix_fd_list (
    invocation,
    with_scale ?
    g_variant_new ("(hiiid)", 0, cairo_image_surface_get_width (surface),
                   cairo_image_surface_get_height (surface),
                   cairo_image_surface_get_stride (surface), scale) :
    g_variant_new ("(hiii)", 0, cairo_image_surface_get_width (surface),
                   cairo_image_surface_get_height (surface),
                   cairo_image_surface_get_stride (surface)),
    fds);
  cairo_surface_destroy (surface);
}

static void
handle_private (ShellPlusScreenshot   *self,
                GDBusMethodInvocation *invocation)
{
  const char *method = g_dbus_method_invocation_get_method_name (invocation);
  GVariant *parameters = g_dbus_method_invocation_get_parameters (invocation);
  g_autoptr (GError) error = NULL;

  if (g_strcmp0 (method, "CaptureScreen") == 0)
    {
      MtkRectangle rect = stage_rect (self);
      cairo_surface_t *surface;
      gboolean cursor;
      float scale = 1.0;

      g_variant_get (parameters, "(b)", &cursor);
      surface = capture_area (self, &rect, cursor, &scale, &error);
      return_image (invocation, surface, error, TRUE, scale);
    }
  else if (g_strcmp0 (method, "CaptureWindow") == 0)
    {
      cairo_surface_t *surface;
      gboolean frame;
      guint64 id;

      g_variant_get (parameters, "(tb)", &id, &frame);
      surface = capture_window (find_window (self, id), frame, NULL, &error);
      return_image (invocation, surface, error, FALSE, 1.0);
    }
  else if (g_strcmp0 (method, "Flash") == 0)
    {
      MtkRectangle rect;

      g_variant_get (parameters, "(iiii)", &rect.x, &rect.y, &rect.width, &rect.height);
      if (rect.width > 0 && rect.height > 0)
        flash_area (self, &rect);
      g_dbus_method_invocation_return_value (invocation, NULL);
    }
  else if (g_strcmp0 (method, "ShowUI") == 0)
    {
      const char *mode;

      g_variant_get (parameters, "(&s)", &mode);
      shellplus_screenshot_show_ui (self, mode);
      g_dbus_method_invocation_return_value (invocation, NULL);
    }
  else if (g_strcmp0 (method, "Finish") == 0)
    {
      g_autoptr (GVariant) result = NULL;
      gboolean success;
      guint id;

      g_variant_get (parameters, "(ub@a{sv})", &id, &success, &result);
      if (id != 0 && id == self->pending_id)
        end_pending (self, success, result);
      g_dbus_method_invocation_return_value (invocation, NULL);
    }
  else
    g_dbus_method_invocation_return_error (invocation, G_DBUS_ERROR, G_DBUS_ERROR_UNKNOWN_METHOD,
                                           "Unknown method %s", method);
}

/* ------------------------------------------------------------------------ */
/* Caller checks                                                            */

static gboolean
allow_all (void)
{
  return g_strcmp0 (g_getenv ("SHELLPLUS_SCREENSHOT_ALLOW_ALL"), "1") == 0;
}

/* See the comment at the top. */
static gboolean
executable_allowed (pid_t pid)
{
  g_autofree char *link = g_strdup_printf ("/proc/%d/exe", (int) pid);
  g_autofree char *exe = g_file_read_link (link, NULL);
  g_autofree char *base = NULL;
  struct stat st;

  if (!exe || !g_str_has_prefix (exe, "/usr/") || g_str_has_suffix (exe, " (deleted)"))
    return FALSE;
  base = g_path_get_basename (exe);
  if (!g_strv_contains (allowed_executables, base))
    return FALSE;
  /* Through the link, so it's the very file the process runs. */
  if (stat (link, &st) < 0 || st.st_uid != 0 || (st.st_mode & (S_IWGRP | S_IWOTH)))
    return FALSE;
  return TRUE;
}

typedef struct
{
  ShellPlusScreenshot *self;
  GDBusMethodInvocation *invocation;
  gboolean private_iface;
} SenderCheck;

static void
on_sender_pid (GObject      *source,
               GAsyncResult *result,
               gpointer      user_data)
{
  SenderCheck *check = user_data;
  g_autoptr (GVariant) reply = NULL;
  g_autoptr (GError) error = NULL;
  gboolean allowed = FALSE;
  guint32 pid = 0;

  reply = g_dbus_connection_call_finish (G_DBUS_CONNECTION (source), result, &error);
  if (g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
    {
      g_object_unref (check->invocation);
      g_free (check);
      return;
    }

  if (reply)
    {
      g_variant_get (reply, "(u)", &pid);
      if (check->private_iface)
        allowed = pid != 0 && check->self->panel_pid &&
                  (pid_t) pid == check->self->panel_pid (check->self->user_data);
      else
        allowed = pid != 0 && executable_allowed (pid);
    }

  if (!allowed)
    {
      const char *method = g_dbus_method_invocation_get_method_name (check->invocation);

      g_message ("Refused %s from %s (pid %u)", method,
                 g_dbus_method_invocation_get_sender (check->invocation), pid);
      g_dbus_method_invocation_return_error (check->invocation, G_DBUS_ERROR,
                                             G_DBUS_ERROR_ACCESS_DENIED,
                                             "%s is not allowed", method);
    }
  else if (check->private_iface)
    handle_private (check->self, check->invocation);
  else
    handle_public (check->self, check->invocation);

  g_object_unref (check->invocation);
  g_free (check);
}

static void
check_sender (ShellPlusScreenshot   *self,
              GDBusMethodInvocation *invocation,
              gboolean               private_iface)
{
  SenderCheck *check = g_new0 (SenderCheck, 1);

  check->self = self;
  check->invocation = g_object_ref (invocation);
  check->private_iface = private_iface;
  g_dbus_connection_call (g_dbus_method_invocation_get_connection (invocation),
                          "org.freedesktop.DBus", "/org/freedesktop/DBus",
                          "org.freedesktop.DBus", "GetConnectionUnixProcessID",
                          g_variant_new ("(s)", g_dbus_method_invocation_get_sender (invocation)),
                          G_VARIANT_TYPE ("(u)"), G_DBUS_CALL_FLAGS_NONE, -1,
                          self->cancellable, on_sender_pid, check);
}

static void
public_method_call (GDBusConnection       *connection,
                    const char            *sender,
                    const char            *object_path,
                    const char            *interface_name,
                    const char            *method_name,
                    GVariant              *parameters,
                    GDBusMethodInvocation *invocation,
                    gpointer               user_data)
{
  ShellPlusScreenshot *self = user_data;

  if (self->blocked)
    {
      g_dbus_method_invocation_return_dbus_error (invocation,
                                                  "org.freedesktop.DBus.Error.AccessDenied",
                                                  "The screen is locked");
      return;
    }

  /* As in GNOME Shell, anyone may pick a color: the user clicks. */
  if (allow_all () || g_strcmp0 (method_name, "PickColor") == 0)
    handle_public (self, invocation);
  else
    check_sender (self, invocation, FALSE);
}

static void
private_method_call (GDBusConnection       *connection,
                     const char            *sender,
                     const char            *object_path,
                     const char            *interface_name,
                     const char            *method_name,
                     GVariant              *parameters,
                     GDBusMethodInvocation *invocation,
                     gpointer               user_data)
{
  ShellPlusScreenshot *self = user_data;

  if (self->blocked)
    {
      g_dbus_method_invocation_return_dbus_error (invocation,
                                                  "org.freedesktop.DBus.Error.AccessDenied",
                                                  "The screen is locked");
      return;
    }

  if (allow_all ())
    handle_private (self, invocation);
  else
    check_sender (self, invocation, TRUE);
}

static const GDBusInterfaceVTable public_vtable = { public_method_call };
static const GDBusInterfaceVTable private_vtable = { private_method_call };

static void
on_bus_acquired (GDBusConnection *connection,
                 const char      *name,
                 gpointer         user_data)
{
  ShellPlusScreenshot *self = user_data;
  g_autoptr (GError) error = NULL;

  self->connection = g_object_ref (connection);
  self->public_registration =
    g_dbus_connection_register_object (connection, PUBLIC_PATH, self->public_info->interfaces[0],
                                       &public_vtable, self, NULL, &error);
  if (!self->public_registration)
    g_warning ("Failed to export %s: %s", PUBLIC_IFACE, error->message);
  g_clear_error (&error);
  self->private_registration =
    g_dbus_connection_register_object (connection, SHELLPLUS_DBUS_PATH,
                                       self->private_info->interfaces[0],
                                       &private_vtable, self, NULL, &error);
  if (!self->private_registration)
    g_warning ("Failed to export %s: %s", PRIVATE_IFACE, error->message);
}

static void
on_name_lost (GDBusConnection *connection,
              const char      *name,
              gpointer         user_data)
{
  g_warning ("Could not own %s; is GNOME Shell running? Screenshot tools won't work.", name);
}

/* ------------------------------------------------------------------------ */
/* Key bindings                                                             */

static void
notify_taken (ShellPlusScreenshot *self,
              const char          *used,
              const GError        *error,
              gpointer             data)
{
  if (error)
    g_warning ("Screenshot failed: %s", error->message);
  else
    emit_private (self, "ScreenshotTaken", g_variant_new ("(s)", used));
}

static void
take_now (ShellPlusScreenshot *self,
          gboolean             window)
{
  g_autoptr (GError) error = NULL;
  g_autoptr (GDateTime) now = g_date_time_new_now_local ();
  g_autofree char *name = g_date_time_format (now, "Screenshot From %Y-%m-%d %H-%M-%S");
  cairo_surface_t *surface;
  MtkRectangle rect = stage_rect (self);

  if (window)
    surface = capture_window (meta_display_get_focus_window (self->display), TRUE, &rect, &error);
  else
    surface = capture_area (self, &rect, FALSE, NULL, &error);
  if (!surface)
    {
      g_warning ("Screenshot failed: %s", error->message);
      return;
    }
  flash_area (self, &rect);

  /* Also onto the clipboard, as GNOME Shell does. */
  {
    GByteArray *bytes = g_byte_array_new ();
    g_autoptr (GBytes) png = NULL;

    cairo_surface_write_to_png_stream (surface, append_png, bytes);
    png = g_byte_array_free_to_bytes (bytes);
    set_clipboard (self, png);
  }
  save_async (self, surface, name, notify_taken, NULL);
}

static void
on_show_ui (MetaDisplay *display, MetaWindow *window, const ClutterEvent *event,
            MetaKeyBinding *binding, gpointer user_data)
{
  shellplus_screenshot_show_ui (user_data, "");
}

static void
on_screenshot (MetaDisplay *display, MetaWindow *window, const ClutterEvent *event,
               MetaKeyBinding *binding, gpointer user_data)
{
  take_now (user_data, FALSE);
}

static void
on_screenshot_window (MetaDisplay *display, MetaWindow *window, const ClutterEvent *event,
                      MetaKeyBinding *binding, gpointer user_data)
{
  take_now (user_data, TRUE);
}

static const struct
{
  const char *key;
  MetaKeyHandlerFunc handler;
} bindings[] = {
  { "show-screenshot-ui", on_show_ui },
  { "screenshot", on_screenshot },
  { "screenshot-window", on_screenshot_window },
};

static void
add_keybindings (ShellPlusScreenshot *self)
{
  GSettingsSchemaSource *source = g_settings_schema_source_get_default ();
  g_autoptr (GSettingsSchema) schema =
    source ? g_settings_schema_source_lookup (source, "org.gnome.shell.keybindings", TRUE) : NULL;

  if (!schema)
    {
      g_debug ("org.gnome.shell.keybindings not installed; no Print key bindings");
      return;
    }

  self->shell_keybindings = g_settings_new_full (schema, NULL, NULL);
  for (guint i = 0; i < G_N_ELEMENTS (bindings); i++)
    {
      if (g_settings_schema_has_key (schema, bindings[i].key))
        meta_display_add_keybinding (self->display, bindings[i].key, self->shell_keybindings,
                                     META_KEY_BINDING_IGNORE_AUTOREPEAT, bindings[i].handler,
                                     self, NULL);
    }
}

/* ------------------------------------------------------------------------ */

ShellPlusScreenshot *
shellplus_screenshot_new (MetaDisplay                 *display,
                          ShellPlusScreenshotPanelPid  panel_pid,
                          gpointer                     user_data)
{
  ShellPlusScreenshot *self = g_new0 (ShellPlusScreenshot, 1);

  self->display = display;
  self->panel_pid = panel_pid;
  self->user_data = user_data;
  self->cancellable = g_cancellable_new ();
  self->public_info = g_dbus_node_info_new_for_xml (public_xml, NULL);
  self->private_info = g_dbus_node_info_new_for_xml (private_xml, NULL);
  self->owner_id = g_bus_own_name (G_BUS_TYPE_SESSION, PUBLIC_NAME,
                                   G_BUS_NAME_OWNER_FLAGS_REPLACE,
                                   on_bus_acquired, NULL, on_name_lost, self, NULL);
  if (allow_all ())
    g_message ("SHELLPLUS_SCREENSHOT_ALLOW_ALL=1: anyone may take screenshots");
  add_keybindings (self);
  return self;
}

void
shellplus_screenshot_free (ShellPlusScreenshot *self)
{
  if (self->shell_keybindings)
    {
      for (guint i = 0; i < G_N_ELEMENTS (bindings); i++)
        meta_display_remove_keybinding (self->display, bindings[i].key);
    }
  g_clear_object (&self->shell_keybindings);
  g_cancellable_cancel (self->cancellable);
  g_clear_object (&self->cancellable);
  if (self->pending)
    return_cancelled (self->pending);
  g_clear_object (&self->pending);
  g_free (self->pending_mode);
  if (self->public_registration)
    g_dbus_connection_unregister_object (self->connection, self->public_registration);
  if (self->private_registration)
    g_dbus_connection_unregister_object (self->connection, self->private_registration);
  g_bus_unown_name (self->owner_id);
  g_clear_object (&self->connection);
  g_dbus_node_info_unref (self->public_info);
  g_dbus_node_info_unref (self->private_info);
  g_free (self);
}
