#define G_LOG_DOMAIN "shellplus"

#include "shellplus-osk.h"

#include <meta/compositor.h>
#include <meta/meta-external-constraint.h>
#include <meta/meta-window-actor.h>

/* The on-screen keyboard, compositor side. Follows GNOME Shell's
 * js/ui/keyboard.js (Keyboard, KeyboardController) where the compositor is
 * involved:
 *
 *  - The panel decides when the keyboard shows (screen-keyboard-enabled,
 *    a text field focused, not locked) from what this tells it:
 *
 *      org.gnome.ShellPlus.Osk at /org/gnome/ShellPlus/Osk
 *        GetFocus () → a{sv}       focused b, purpose s (normal, alpha,
 *                                  digits, number, phone, url, email, name,
 *                                  password, date, time, datetime,
 *                                  terminal), hints u (ClutterInputContent-
 *                                  HintFlags), cursor (iiii) in stage
 *                                  coordinates
 *        signal FocusChanged (a{sv})  the same, when focused, purpose or
 *                                  hints change (not on cursor moves)
 *        CommitText (s)            panel only: commits the text to the
 *                                  focused text field through the input
 *                                  method, or types it as key events if
 *                                  there is none
 *        Keyval (u keyval, b pressed)  panel only: a real key event from a
 *                                  virtual keyboard (Return, BackSpace,
 *                                  arrows…; the client repeats it while
 *                                  pressed)
 *
 *    "Panel only": the caller's pid (from the bus daemon) must be the pid of
 *    the panel process the compositor spawned, as for unlocking. Nothing is
 *    typed while the screen is locked: the lock screen's own keyboard types
 *    straight into its password entry, inside the panel.
 *
 *  - The keyboard window ("Screen Keyboard") is a dock: Mutter never
 *    focuses docks on click, so the text field keeps the focus. It is put
 *    full width at the bottom of the primary monitor, slides up when it
 *    maps, and while it is shown its height is a bottom strut, so maximized
 *    windows end above it. Like GNOME Shell, a focused window whose text
 *    cursor would be covered is moved up, and back down when the keyboard
 *    hides.
 */

#define DBUS_PATH  "/org/gnome/ShellPlus/Osk"
#define DBUS_IFACE "org.gnome.ShellPlus.Osk"

/* GNOME Shell's KEYBOARD_ANIMATION_TIME. */
#define SLIDE_MS           150
/* Space kept between the text cursor and the keyboard. */
#define CURSOR_MARGIN      12
#define MOVE_DELAY_MS      60

static const char introspection_xml[] =
  "<node>"
  "  <interface name='" DBUS_IFACE "'>"
  "    <method name='GetFocus'>"
  "      <arg type='a{sv}' name='focus' direction='out'/>"
  "    </method>"
  "    <method name='CommitText'>"
  "      <arg type='s' name='text' direction='in'/>"
  "    </method>"
  "    <method name='Keyval'>"
  "      <arg type='u' name='keyval' direction='in'/>"
  "      <arg type='b' name='pressed' direction='in'/>"
  "    </method>"
  "    <signal name='FocusChanged'>"
  "      <arg type='a{sv}' name='focus'/>"
  "    </signal>"
  "  </interface>"
  "</node>";

struct _ShellPlusOsk
{
  MetaDisplay *display;
  ShellPlusInputMethod *input_method;
  ShellPlusOskHandlers handlers;
  gpointer user_data;

  MetaExternalConstraint *constraint;
  MetaWindow *window;
  gboolean shown;
  int height;

  /* The window moved up so that its text cursor stays visible. */
  MetaWindow *moved_window;
  int moved_from_y;
  int moved_to_y;
  guint move_id;

  ClutterVirtualInputDevice *keyboard;
  GArray *pressed;                 /* keyvals pressed through Keyval() */

  GDBusConnection *connection;
  GDBusNodeInfo *node_info;
  guint registration_id;
  GCancellable *cancellable;
  /* The panel's unique bus name, once its pid checked out. */
  char *panel_sender;
  pid_t panel_sender_pid;

  gboolean last_focused;
  ClutterInputContentPurpose last_purpose;
  ClutterInputContentHintFlags last_hints;
  guint focus_idle_id;

  GSettings *interface_settings;
};

/* ------------------------------------------------------------------------ */
/* Placement                                                                */

#define SHELLPLUS_TYPE_OSK_CONSTRAINT (shellplus_osk_constraint_get_type ())
G_DECLARE_FINAL_TYPE (ShellPlusOskConstraint, shellplus_osk_constraint,
                      SHELLPLUS, OSK_CONSTRAINT, GObject)

struct _ShellPlusOskConstraint
{
  GObject parent;
  MetaDisplay *display;
};

static void osk_constraint_iface_init (MetaExternalConstraintInterface *iface);

G_DEFINE_TYPE_WITH_CODE (ShellPlusOskConstraint, shellplus_osk_constraint, G_TYPE_OBJECT,
                         G_IMPLEMENT_INTERFACE (META_TYPE_EXTERNAL_CONSTRAINT,
                                                osk_constraint_iface_init))

/* Full width at the bottom of the primary monitor, at most half its height. */
static gboolean
osk_constraint_constrain (MetaExternalConstraint     *constraint,
                          MetaWindow                 *window,
                          MetaExternalConstraintInfo *info)
{
  ShellPlusOskConstraint *self = SHELLPLUS_OSK_CONSTRAINT (constraint);
  int primary = meta_display_get_primary_monitor (self->display);
  MtkRectangle *rect = info->new_rect;
  MtkRectangle monitor;

  if (primary < 0)
    return FALSE;

  meta_display_get_monitor_geometry (self->display, primary, &monitor);
  rect->x = monitor.x;
  rect->width = monitor.width;
  rect->height = CLAMP (rect->height, 1, monitor.height / 2);
  rect->y = monitor.y + monitor.height - rect->height;
  return TRUE;
}

static void
osk_constraint_iface_init (MetaExternalConstraintInterface *iface)
{
  iface->constrain = osk_constraint_constrain;
}

static void
shellplus_osk_constraint_class_init (ShellPlusOskConstraintClass *klass)
{
}

static void
shellplus_osk_constraint_init (ShellPlusOskConstraint *self)
{
}

/* ------------------------------------------------------------------------ */
/* Keeping the text cursor visible                                          */

static void
on_moved_window_unmanaged (MetaWindow   *window,
                           ShellPlusOsk *self)
{
  if (self->moved_window == window)
    self->moved_window = NULL;
}

static void
forget_moved_window (ShellPlusOsk *self)
{
  if (self->moved_window)
    g_signal_handlers_disconnect_by_func (self->moved_window, on_moved_window_unmanaged, self);
  self->moved_window = NULL;
}

/* Back where it was, unless the user moved it meanwhile. */
static void
restore_moved_window (ShellPlusOsk *self)
{
  MtkRectangle frame;

  g_clear_handle_id (&self->move_id, g_source_remove);
  if (!self->moved_window)
    return;

  meta_window_get_frame_rect (self->moved_window, &frame);
  if (frame.y == self->moved_to_y)
    meta_window_move_frame (self->moved_window, FALSE, frame.x, self->moved_from_y);
  forget_moved_window (self);
}

static gboolean
move_window_cb (gpointer user_data)
{
  ShellPlusOsk *self = user_data;
  MetaWindow *window = meta_display_get_focus_window (self->display);
  int primary = meta_display_get_primary_monitor (self->display);
  MtkRectangle cursor, frame, monitor, work_area;
  int keyboard_top, cursor_offset, new_y;

  self->move_id = 0;
  if (!self->shown || !window || primary < 0 ||
      !shellplus_input_method_get_focus (self->input_method, NULL, NULL) ||
      !shellplus_input_method_get_cursor_rect (self->input_method, &cursor))
    return G_SOURCE_REMOVE;

  /* Only ordinary windows that can move (not docks such as the panel's,
   * nor maximized or fullscreen ones, which the strut already shrank). */
  switch (meta_window_get_window_type (window))
    {
    case META_WINDOW_NORMAL:
    case META_WINDOW_DIALOG:
    case META_WINDOW_MODAL_DIALOG:
    case META_WINDOW_UTILITY:
      break;
    default:
      return G_SOURCE_REMOVE;
    }
  if (meta_window_get_maximize_flags (window) || meta_window_is_fullscreen (window) ||
      meta_window_get_monitor (window) != primary)
    return G_SOURCE_REMOVE;

  meta_display_get_monitor_geometry (self->display, primary, &monitor);
  keyboard_top = monitor.y + monitor.height - self->height;
  if (cursor.y + cursor.height + CURSOR_MARGIN <= keyboard_top)
    return G_SOURCE_REMOVE;

  meta_window_get_frame_rect (window, &frame);
  meta_window_get_work_area_for_monitor (window, primary, &work_area);
  cursor_offset = cursor.y - frame.y;
  new_y = MAX (work_area.y, keyboard_top - CURSOR_MARGIN - cursor.height - cursor_offset);
  if (new_y >= frame.y)
    return G_SOURCE_REMOVE;

  if (self->moved_window != window)
    {
      restore_moved_window (self);
      self->moved_window = window;
      self->moved_from_y = frame.y;
      g_signal_connect (window, "unmanaged", G_CALLBACK (on_moved_window_unmanaged), self);
    }
  g_debug ("osk: moving '%s' up from %d to %d", meta_window_get_title (window), frame.y, new_y);
  meta_window_move_frame (window, FALSE, frame.x, new_y);
  meta_window_get_frame_rect (window, &frame);
  self->moved_to_y = frame.y;
  return G_SOURCE_REMOVE;
}

static void
queue_move_window (ShellPlusOsk *self)
{
  if (self->shown && !self->move_id)
    self->move_id = g_timeout_add (MOVE_DELAY_MS, move_window_cb, self);
}

/* ------------------------------------------------------------------------ */
/* The keyboard window                                                      */

static void release_pressed_keys (ShellPlusOsk *self);

static void
set_shown (ShellPlusOsk *self,
           gboolean      shown,
           int           height)
{
  if (self->shown == shown && self->height == height)
    return;

  self->shown = shown;
  self->height = shown ? height : 0;
  g_debug ("osk: %s (height %d)", shown ? "shown" : "hidden", self->height);
  if (self->handlers.struts_changed)
    self->handlers.struts_changed (self->user_data);

  if (shown)
    queue_move_window (self);
  else
    {
      restore_moved_window (self);
      release_pressed_keys (self);
    }
}

static void
on_window_size_changed (MetaWindow   *window,
                        ShellPlusOsk *self)
{
  MtkRectangle frame;

  if (window != self->window || !self->shown)
    return;
  meta_window_get_frame_rect (window, &frame);
  set_shown (self, TRUE, frame.height);
}

static void
on_window_unmanaging (MetaWindow   *window,
                      ShellPlusOsk *self)
{
  if (window != self->window)
    return;

  g_signal_handlers_disconnect_by_data (window, self);
  self->window = NULL;
  set_shown (self, FALSE, 0);
}

void
shellplus_osk_setup_window (ShellPlusOsk *self,
                            MetaWindow   *window)
{
  MtkRectangle frame;

  if (self->window && self->window != window)
    on_window_unmanaging (self->window, self);
  self->window = window;

  meta_window_set_type (window, META_WINDOW_DOCK);
  meta_window_hide_from_window_list (window);
  meta_window_stick (window);
  meta_window_add_external_constraint (window, self->constraint);
  g_signal_connect (window, "unmanaging", G_CALLBACK (on_window_unmanaging), self);
  g_signal_connect (window, "size-changed", G_CALLBACK (on_window_size_changed), self);

  /* Re-run the constraints in case the window is already placed. */
  meta_window_get_frame_rect (window, &frame);
  meta_window_move_frame (window, FALSE, frame.x, frame.y);
  if (!meta_window_is_hidden (window) && meta_window_get_compositor_private (window))
    shellplus_osk_window_mapped (self, window);
}

static gboolean
animations_enabled (ShellPlusOsk *self)
{
  return g_settings_get_boolean (self->interface_settings, "enable-animations");
}

void
shellplus_osk_window_mapped (ShellPlusOsk *self,
                             MetaWindow   *window)
{
  ClutterActor *actor;
  MtkRectangle frame;

  if (window != self->window)
    return;

  /* Above the other docks (top bar, overview, system dialogs). */
  meta_window_raise (window);
  meta_window_get_frame_rect (window, &frame);
  set_shown (self, TRUE, frame.height);

  actor = CLUTTER_ACTOR (meta_window_get_compositor_private (window));
  if (actor && animations_enabled (self))
    {
      clutter_actor_remove_all_transitions (actor);
      clutter_actor_set_translation (actor, 0.0f, (float) frame.height, 0.0f);
      clutter_actor_save_easing_state (actor);
      clutter_actor_set_easing_mode (actor, CLUTTER_EASE_OUT_QUAD);
      clutter_actor_set_easing_duration (actor, SLIDE_MS);
      clutter_actor_set_translation (actor, 0.0f, 0.0f, 0.0f);
      clutter_actor_restore_easing_state (actor);
    }
}

gboolean
shellplus_osk_get_strut (ShellPlusOsk *self,
                         MetaStrut    *strut)
{
  int primary = meta_display_get_primary_monitor (self->display);
  MtkRectangle monitor;

  if (!self->shown || self->height <= 0 || primary < 0)
    return FALSE;

  meta_display_get_monitor_geometry (self->display, primary, &monitor);
  strut->rect.x = monitor.x;
  strut->rect.width = monitor.width;
  strut->rect.height = self->height;
  strut->rect.y = monitor.y + monitor.height - self->height;
  strut->side = META_SIDE_BOTTOM;
  return TRUE;
}

/* ------------------------------------------------------------------------ */
/* Focus                                                                    */

static const char *
purpose_name (ClutterInputContentPurpose purpose)
{
  switch (purpose)
    {
    case CLUTTER_INPUT_CONTENT_PURPOSE_ALPHA:    return "alpha";
    case CLUTTER_INPUT_CONTENT_PURPOSE_DIGITS:   return "digits";
    case CLUTTER_INPUT_CONTENT_PURPOSE_NUMBER:   return "number";
    case CLUTTER_INPUT_CONTENT_PURPOSE_PHONE:    return "phone";
    case CLUTTER_INPUT_CONTENT_PURPOSE_URL:      return "url";
    case CLUTTER_INPUT_CONTENT_PURPOSE_EMAIL:    return "email";
    case CLUTTER_INPUT_CONTENT_PURPOSE_NAME:     return "name";
    case CLUTTER_INPUT_CONTENT_PURPOSE_PASSWORD: return "password";
    case CLUTTER_INPUT_CONTENT_PURPOSE_DATE:     return "date";
    case CLUTTER_INPUT_CONTENT_PURPOSE_TIME:     return "time";
    case CLUTTER_INPUT_CONTENT_PURPOSE_DATETIME: return "datetime";
    case CLUTTER_INPUT_CONTENT_PURPOSE_TERMINAL: return "terminal";
    default:                                     return "normal";
    }
}

static GVariant *
build_focus (ShellPlusOsk *self)
{
  ClutterInputContentPurpose purpose;
  ClutterInputContentHintFlags hints;
  MtkRectangle cursor = { 0 };
  gboolean focused;
  GVariantBuilder builder;

  focused = shellplus_input_method_get_focus (self->input_method, &purpose, &hints);
  if (focused)
    shellplus_input_method_get_cursor_rect (self->input_method, &cursor);

  g_variant_builder_init (&builder, G_VARIANT_TYPE_VARDICT);
  g_variant_builder_add (&builder, "{sv}", "focused", g_variant_new_boolean (focused));
  g_variant_builder_add (&builder, "{sv}", "purpose", g_variant_new_string (purpose_name (purpose)));
  g_variant_builder_add (&builder, "{sv}", "hints", g_variant_new_uint32 (hints));
  g_variant_builder_add (&builder, "{sv}", "cursor",
                         g_variant_new ("(iiii)", cursor.x, cursor.y, cursor.width, cursor.height));
  return g_variant_builder_end (&builder);
}

static gboolean
focus_idle_cb (gpointer user_data)
{
  ShellPlusOsk *self = user_data;
  ClutterInputContentPurpose purpose;
  ClutterInputContentHintFlags hints;
  gboolean focused;

  self->focus_idle_id = 0;
  focused = shellplus_input_method_get_focus (self->input_method, &purpose, &hints);

  if (focused == self->last_focused && purpose == self->last_purpose && hints == self->last_hints)
    {
      /* Only the cursor moved. */
      return G_SOURCE_REMOVE;
    }

  if (focused && !self->last_focused)
    queue_move_window (self);
  self->last_focused = focused;
  self->last_purpose = purpose;
  self->last_hints = hints;

  if (self->connection && self->registration_id)
    g_dbus_connection_emit_signal (self->connection, NULL, DBUS_PATH, DBUS_IFACE,
                                   "FocusChanged",
                                   g_variant_new ("(@a{sv})", build_focus (self)), NULL);
  return G_SOURCE_REMOVE;
}

/* A focus out and the next focus in usually come together (moving between
 * fields); one idle reports where it ended up. */
static void
on_focus_changed (gpointer user_data)
{
  ShellPlusOsk *self = user_data;

  if (!self->focus_idle_id)
    self->focus_idle_id = g_idle_add (focus_idle_cb, self);
}

/* ------------------------------------------------------------------------ */
/* Typing                                                                   */

static ClutterVirtualInputDevice *
get_keyboard (ShellPlusOsk *self)
{
  if (!self->keyboard)
    {
      MetaCompositor *compositor = meta_display_get_compositor (self->display);
      ClutterActor *stage = CLUTTER_ACTOR (meta_compositor_get_stage (compositor));
      ClutterBackend *backend = clutter_context_get_backend (clutter_actor_get_context (stage));
      ClutterSeat *seat = clutter_backend_get_default_seat (backend);

      self->keyboard = clutter_seat_create_virtual_device (seat, CLUTTER_KEYBOARD_DEVICE);
    }
  return self->keyboard;
}

static void
notify_keyval (ShellPlusOsk *self,
               guint         keyval,
               gboolean      pressed)
{
  clutter_virtual_input_device_notify_keyval (get_keyboard (self), g_get_monotonic_time (),
                                              keyval,
                                              pressed ? CLUTTER_KEY_STATE_PRESSED :
                                                        CLUTTER_KEY_STATE_RELEASED);
}

static void
release_pressed_keys (ShellPlusOsk *self)
{
  while (self->pressed && self->pressed->len > 0)
    {
      guint keyval = g_array_index (self->pressed, guint, self->pressed->len - 1);

      g_array_remove_index (self->pressed, self->pressed->len - 1);
      notify_keyval (self, keyval, FALSE);
    }
}

static void
press_keyval (ShellPlusOsk *self,
              guint         keyval,
              gboolean      pressed)
{
  guint i;

  for (i = 0; i < self->pressed->len; i++)
    if (g_array_index (self->pressed, guint, i) == keyval)
      break;

  if (pressed && i == self->pressed->len)
    g_array_append_val (self->pressed, keyval);
  else if (!pressed && i < self->pressed->len)
    g_array_remove_index (self->pressed, i);
  else if (!pressed)
    return;  /* never pressed (or released when the keyboard hid) */

  notify_keyval (self, keyval, pressed);
}

static void
commit_text (ShellPlusOsk *self,
             const char   *text)
{
  const char *p;

  if (shellplus_input_method_get_focus (self->input_method, NULL, NULL))
    {
      clutter_input_method_commit (CLUTTER_INPUT_METHOD (self->input_method), text);
      return;
    }

  /* No text-input client (e.g. an X11 app): type it. Characters the
   * keymap doesn't have are skipped by Mutter. */
  for (p = text; *p; p = g_utf8_next_char (p))
    {
      guint keyval = clutter_unicode_to_keysym (g_utf8_get_char (p));

      notify_keyval (self, keyval, TRUE);
      notify_keyval (self, keyval, FALSE);
    }
}

/* ------------------------------------------------------------------------ */
/* D-Bus                                                                    */

static void
run_method (ShellPlusOsk          *self,
            const char            *method_name,
            GVariant              *parameters,
            GDBusMethodInvocation *invocation)
{
  if (self->handlers.is_locked && self->handlers.is_locked (self->user_data))
    {
      g_dbus_method_invocation_return_error_literal (invocation, G_DBUS_ERROR,
                                                     G_DBUS_ERROR_ACCESS_DENIED,
                                                     "The screen is locked");
      return;
    }

  if (g_strcmp0 (method_name, "CommitText") == 0)
    {
      const char *text;

      g_variant_get (parameters, "(&s)", &text);
      if (!g_utf8_validate (text, -1, NULL))
        {
          g_dbus_method_invocation_return_error_literal (invocation, G_DBUS_ERROR,
                                                         G_DBUS_ERROR_INVALID_ARGS,
                                                         "Invalid UTF-8");
          return;
        }
      commit_text (self, text);
    }
  else
    {
      guint keyval;
      gboolean pressed;

      g_variant_get (parameters, "(ub)", &keyval, &pressed);
      press_keyval (self, keyval, pressed);
    }
  g_dbus_method_invocation_return_value (invocation, NULL);
}

typedef struct
{
  ShellPlusOsk *self;
  GDBusMethodInvocation *invocation;
  char *sender;
  char *method_name;
  GVariant *parameters;
} PendingCall;

static void
pending_call_free (PendingCall *call)
{
  g_object_unref (call->invocation);
  g_free (call->sender);
  g_free (call->method_name);
  g_variant_unref (call->parameters);
  g_free (call);
}

static void
on_sender_pid (GObject      *source,
               GAsyncResult *result,
               gpointer      user_data)
{
  PendingCall *call = user_data;
  g_autoptr (GVariant) reply = NULL;
  g_autoptr (GError) error = NULL;
  ShellPlusOsk *self;
  guint32 pid = 0;
  pid_t panel_pid;

  reply = g_dbus_connection_call_finish (G_DBUS_CONNECTION (source), result, &error);
  if (g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
    {
      g_dbus_method_invocation_return_error_literal (call->invocation, G_DBUS_ERROR,
                                                     G_DBUS_ERROR_FAILED, "Shutting down");
      pending_call_free (call);
      return;
    }

  self = call->self;
  if (reply)
    g_variant_get (reply, "(u)", &pid);
  /* Looked up now, not when the call came in: the panel may have died. */
  panel_pid = self->handlers.get_panel_pid (self->user_data);

  if (!reply || pid == 0 || panel_pid <= 0 || pid != (guint32) panel_pid)
    {
      g_warning ("Ignoring %s from %s (pid %u): not the panel",
                 call->method_name, call->sender, pid);
      g_dbus_method_invocation_return_error_literal (call->invocation, G_DBUS_ERROR,
                                                     G_DBUS_ERROR_ACCESS_DENIED,
                                                     "Only the panel may use the screen keyboard");
      pending_call_free (call);
      return;
    }

  /* A unique name never moves to another connection. */
  g_free (self->panel_sender);
  self->panel_sender = g_strdup (call->sender);
  self->panel_sender_pid = panel_pid;
  run_method (self, call->method_name, call->parameters, call->invocation);
  pending_call_free (call);
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
  ShellPlusOsk *self = user_data;
  PendingCall *call;
  pid_t panel_pid;

  if (g_strcmp0 (method_name, "GetFocus") == 0)
    {
      g_dbus_method_invocation_return_value (invocation,
                                             g_variant_new ("(@a{sv})", build_focus (self)));
      return;
    }

  if (g_strcmp0 (method_name, "CommitText") != 0 && g_strcmp0 (method_name, "Keyval") != 0)
    {
      g_dbus_method_invocation_return_error (invocation, G_DBUS_ERROR,
                                             G_DBUS_ERROR_UNKNOWN_METHOD,
                                             "Unknown method %s", method_name);
      return;
    }

  /* Calls from the already checked panel connection run in order. */
  panel_pid = self->handlers.get_panel_pid (self->user_data);
  if (self->panel_sender && g_strcmp0 (sender, self->panel_sender) == 0 &&
      panel_pid > 0 && panel_pid == self->panel_sender_pid)
    {
      run_method (self, method_name, parameters, invocation);
      return;
    }

  call = g_new0 (PendingCall, 1);
  call->self = self;
  call->invocation = g_object_ref (invocation);
  call->sender = g_strdup (sender);
  call->method_name = g_strdup (method_name);
  call->parameters = g_variant_ref (parameters);
  g_dbus_connection_call (connection, "org.freedesktop.DBus", "/org/freedesktop/DBus",
                          "org.freedesktop.DBus", "GetConnectionUnixProcessID",
                          g_variant_new ("(s)", sender), G_VARIANT_TYPE ("(u)"),
                          G_DBUS_CALL_FLAGS_NONE, -1, self->cancellable,
                          on_sender_pid, call);
}

static const GDBusInterfaceVTable interface_vtable = {
  handle_method_call,
};

static void
export_interface (ShellPlusOsk *self)
{
  g_autoptr (GError) error = NULL;

  self->connection = g_bus_get_sync (G_BUS_TYPE_SESSION, NULL, &error);
  if (!self->connection)
    {
      g_warning ("No session bus for the screen keyboard: %s", error->message);
      return;
    }
  self->node_info = g_dbus_node_info_new_for_xml (introspection_xml, NULL);
  self->registration_id =
    g_dbus_connection_register_object (self->connection, DBUS_PATH,
                                       self->node_info->interfaces[0],
                                       &interface_vtable, self, NULL, &error);
  if (!self->registration_id)
    g_warning ("Failed to export %s: %s", DBUS_IFACE, error->message);
}

/* ------------------------------------------------------------------------ */

ShellPlusOsk *
shellplus_osk_new (MetaDisplay                *display,
                   ShellPlusInputMethod       *input_method,
                   const ShellPlusOskHandlers *handlers,
                   gpointer                    user_data)
{
  ShellPlusOsk *self = g_new0 (ShellPlusOsk, 1);
  ShellPlusOskConstraint *constraint = g_object_new (SHELLPLUS_TYPE_OSK_CONSTRAINT, NULL);

  self->display = display;
  self->input_method = g_object_ref (input_method);
  self->handlers = *handlers;
  self->user_data = user_data;
  constraint->display = display;
  self->constraint = META_EXTERNAL_CONSTRAINT (constraint);
  self->pressed = g_array_new (FALSE, FALSE, sizeof (guint));
  self->cancellable = g_cancellable_new ();
  self->interface_settings = g_settings_new ("org.gnome.desktop.interface");
  self->last_purpose = CLUTTER_INPUT_CONTENT_PURPOSE_NORMAL;

  shellplus_input_method_set_focus_callback (input_method, on_focus_changed, self);
  export_interface (self);
  /* Now, not on the first key: a new device's first event can get lost. */
  get_keyboard (self);
  return self;
}

void
shellplus_osk_free (ShellPlusOsk *self)
{
  shellplus_input_method_set_focus_callback (self->input_method, NULL, NULL);
  g_clear_handle_id (&self->focus_idle_id, g_source_remove);
  g_clear_handle_id (&self->move_id, g_source_remove);
  if (self->window)
    g_signal_handlers_disconnect_by_data (self->window, self);
  forget_moved_window (self);
  release_pressed_keys (self);
  g_cancellable_cancel (self->cancellable);
  g_clear_object (&self->cancellable);
  if (self->registration_id)
    g_dbus_connection_unregister_object (self->connection, self->registration_id);
  g_clear_object (&self->connection);
  g_clear_pointer (&self->node_info, g_dbus_node_info_unref);
  g_clear_object (&self->keyboard);
  g_clear_pointer (&self->pressed, g_array_unref);
  g_clear_object (&self->constraint);
  g_clear_object (&self->interface_settings);
  g_clear_object (&self->input_method);
  g_free (self->panel_sender);
  g_free (self);
}
