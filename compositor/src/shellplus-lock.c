#define G_LOG_DOMAIN "shellplus"

#include "shellplus-lock.h"

#include <gio/gunixfdlist.h>
#include <meta/compositor.h>
#include <meta/meta-backend.h>
#include <meta/meta-context.h>
#include <meta/meta-idle-monitor.h>
#include <meta/meta-window-actor.h>
#include <unistd.h>

#include "shellplus-panel-constraint.h"

/* The screen shield, like GNOME Shell's ScreenShield.
 *
 * States: INACTIVE; ACTIVE (the shield fades in to black after idle-delay,
 * any input dismisses it; it turns into a lock after lock-delay); LOCKED.
 *
 * While the shield is up, a black, reactive actor on top of the whole stage
 * (every monitor) hides everything. While locked, the window groups are
 * hidden too and the only thing above the black is the panel's lock window
 * ("Lock Screen"), whose actor is moved into the shield. If the panel isn't
 * running or hasn't mapped that window yet, the screen just stays black,
 * and a Clutter grab on the shield keeps the keyboard away from every
 * client. Keybindings are filtered (only grabbed accelerators that allow
 * the lock screen, e.g. media keys, and layout switching run), the overview
 * and Alt-Tab are blocked, and unredirection (direct scanout of a
 * fullscreen window) is disabled.
 *
 * Unlocking: only org.gnome.ShellPlus.Lock.Unlock() from the panel process
 * this compositor spawned unlocks; the caller's pid (from the bus daemon)
 * must be the panel subprocess's pid. org.gnome.ScreenSaver.SetActive(false)
 * only dismisses an unlocked shield, and logind's Unlock signal is ignored
 * (any process of the user may send it through `loginctl unlock-session`).
 *
 * D-Bus (session bus):
 *   org.gnome.ScreenSaver at /org/gnome/ScreenSaver, as in GNOME Shell:
 *     Lock(), GetActive() -> b, SetActive(b), GetActiveTime() -> u,
 *     signals ActiveChanged(b), WakeUpScreen()
 *   org.gnome.ShellPlus.Lock at /org/gnome/ShellPlus:
 *     Unlock()   panel only
 *
 * The panel learns about the lock through the "locked" key of
 * org.gnome.ShellPlus.Panel's state (see shellplus-dbus.c).
 */

#define SCREENSAVER_NAME  "org.gnome.ScreenSaver"
#define SCREENSAVER_PATH  "/org/gnome/ScreenSaver"
#define SCREENSAVER_IFACE "org.gnome.ScreenSaver"
#define LOCK_PATH         "/org/gnome/ShellPlus"
#define LOCK_IFACE        "org.gnome.ShellPlus.Lock"

#define LOGIND_NAME    "org.freedesktop.login1"
#define LOGIND_PATH    "/org/freedesktop/login1"
#define LOGIND_MANAGER "org.freedesktop.login1.Manager"
#define LOGIND_SESSION "org.freedesktop.login1.Session"

/* GNOME Shell's STANDARD_FADE_TIME: the shield fades in over this long
 * after idle-delay, and moving the mouse meanwhile cancels it. */
#define IDLE_FADE_MS          10000
/* How long suspend waits for the lock screen at most. */
#define SLEEP_RELEASE_MS      2000
/* Wake the screen (gsd-power) at most this often while locked. */
#define WAKE_UP_IDLE_MS       1000
#define SESSION_LOOKUP_TRIES  5
#define SESSION_RETRY_MS      1000

typedef enum
{
  STATE_INACTIVE,
  STATE_ACTIVE,
  STATE_LOCKED,
} State;

static const char screensaver_xml[] =
  "<node>"
  "  <interface name='" SCREENSAVER_IFACE "'>"
  "    <method name='Lock'/>"
  "    <method name='GetActive'>"
  "      <arg type='b' name='active' direction='out'/>"
  "    </method>"
  "    <method name='SetActive'>"
  "      <arg type='b' name='value' direction='in'/>"
  "    </method>"
  "    <method name='GetActiveTime'>"
  "      <arg type='u' name='value' direction='out'/>"
  "    </method>"
  "    <signal name='ActiveChanged'>"
  "      <arg type='b' name='new_value'/>"
  "    </signal>"
  "    <signal name='WakeUpScreen'/>"
  "  </interface>"
  "</node>";

static const char lock_xml[] =
  "<node>"
  "  <interface name='" LOCK_IFACE "'>"
  "    <method name='Unlock'/>"
  "  </interface>"
  "</node>";

struct _ShellPlusLock
{
  MetaDisplay *display;
  ShellPlusShellDBus *shell_dbus;
  ShellPlusLockHandlers handlers;
  gpointer user_data;

  State state;
  gint64 active_since;  /* monotonic µs */

  /* On top of the stage: the black curtain, then the lock window's actor
   * (in its own parent, since Mutter restacks window actors within their
   * parent). */
  ClutterActor *group;
  ClutterActor *curtain;
  ClutterActor *holder;
  ClutterGrab *grab;
  gboolean unredirect_disabled;
  guint fade_id;
  guint lock_delay_id;

  MetaExternalConstraint *constraint;
  MetaWindow *window;  /* the panel's lock window */

  GSettings *screensaver;
  GSettings *session;
  GSettings *lockdown;
  GSettings *interface;
  MetaIdleMonitor *idle_monitor;
  guint idle_watch;
  guint active_watch;
  guint wake_idle_watch;

  GCancellable *cancellable;
  GDBusNodeInfo *screensaver_info;
  GDBusNodeInfo *lock_info;
  GDBusConnection *session_bus;
  guint screensaver_owner_id;
  guint screensaver_registration_id;
  guint lock_registration_id;

  GDBusConnection *system_bus;
  guint logind_watch_id;
  char *session_path;
  guint session_lock_id;
  guint prepare_for_sleep_id;
  int session_lookup_tries;
  guint session_retry_id;
  int inhibit_fd;
  gboolean inhibit_pending;
  guint sleep_release_id;
};

static void set_inactive (ShellPlusLock *self);
static void take_inhibitor (ShellPlusLock *self);
static void release_inhibitor (ShellPlusLock *self);
static gboolean retry_session_lookup (gpointer user_data);

static ClutterActor *
stage (ShellPlusLock *self)
{
  return CLUTTER_ACTOR (meta_compositor_get_stage (meta_display_get_compositor (self->display)));
}

static gboolean
animations_enabled (ShellPlusLock *self)
{
  return g_settings_get_boolean (self->interface, "enable-animations");
}

static gboolean
lock_disabled (ShellPlusLock *self)
{
  return self->lockdown && g_settings_get_boolean (self->lockdown, "disable-lock-screen");
}

/* Locking automatically (idle, suspend) is wanted. */
static gboolean
auto_lock_enabled (ShellPlusLock *self)
{
  return g_settings_get_boolean (self->screensaver, "lock-enabled") && !lock_disabled (self);
}

static void
emit_screensaver (ShellPlusLock *self,
                  const char    *signal,
                  GVariant      *params)
{
  if (!self->session_bus || !self->screensaver_registration_id)
    {
      if (params)
        g_variant_unref (g_variant_ref_sink (params));
      return;
    }
  g_dbus_connection_emit_signal (self->session_bus, NULL, SCREENSAVER_PATH,
                                 SCREENSAVER_IFACE, signal, params, NULL);
}

/* ------------------------------------------------------------------------ */
/* The shield                                                               */

/* The lock window is mapped and above the shield. */
static gboolean
window_shown (ShellPlusLock *self)
{
  MetaWindowActor *actor;

  if (!self->window || meta_window_is_hidden (self->window))
    return FALSE;
  actor = META_WINDOW_ACTOR (meta_window_get_compositor_private (self->window));
  return actor && !meta_window_actor_is_destroyed (actor) &&
         clutter_actor_get_parent (CLUTTER_ACTOR (actor)) == self->holder;
}

/* Without a lock window to type into, a grab on the shield takes the
 * keyboard (and pointer) away from every client. */
static void
sync_grab (ShellPlusLock *self)
{
  gboolean want = self->state == STATE_ACTIVE ||
                  (self->state == STATE_LOCKED && !window_shown (self));

  if (want && !self->grab)
    self->grab = clutter_stage_grab (CLUTTER_STAGE (stage (self)), self->group);
  else if (!want && self->grab)
    {
      clutter_grab_dismiss (self->grab);
      g_clear_object (&self->grab);
    }
}

static void
move_actor (ClutterActor *actor,
            ClutterActor *parent)
{
  ClutterActor *old = clutter_actor_get_parent (actor);

  if (old == parent)
    return;

  g_object_ref (actor);
  if (old)
    clutter_actor_remove_child (old, actor);
  clutter_actor_add_child (parent, actor);
  g_object_unref (actor);
}

static void
adopt_window (ShellPlusLock *self)
{
  MetaWindowActor *actor;

  if (!self->window || self->state != STATE_LOCKED)
    return;

  actor = META_WINDOW_ACTOR (meta_window_get_compositor_private (self->window));
  if (actor && !meta_window_actor_is_destroyed (actor))
    move_actor (CLUTTER_ACTOR (actor), self->holder);
}

/* Back into the window group, where the panel can fade it out. */
static void
release_window (ShellPlusLock *self)
{
  MetaWindowActor *actor;

  if (!self->window)
    return;

  actor = META_WINDOW_ACTOR (meta_window_get_compositor_private (self->window));
  if (actor && clutter_actor_get_parent (CLUTTER_ACTOR (actor)) == self->holder)
    move_actor (CLUTTER_ACTOR (actor),
                meta_compositor_get_window_group (meta_display_get_compositor (self->display)));
}

static void
focus_window (ShellPlusLock *self)
{
  MetaWindow *focus = meta_display_get_focus_window (self->display);

  if (!window_shown (self) || focus == self->window ||
      (focus && meta_window_is_ancestor_of_transient (self->window, focus)))
    return;

  meta_window_focus (self->window, meta_display_get_current_time_roundtrip (self->display));
}

static void
set_windows_visible (ShellPlusLock *self,
                     gboolean       visible)
{
  MetaCompositor *compositor = meta_display_get_compositor (self->display);

  ClutterActor *groups[] = {
    meta_compositor_get_window_group (compositor),
    meta_compositor_get_top_window_group (compositor),
  };

  for (guint i = 0; i < G_N_ELEMENTS (groups); i++)
    {
      if (visible)
        clutter_actor_show (groups[i]);
      else
        clutter_actor_hide (groups[i]);
    }
}

static void
raise_shield (ShellPlusLock *self,
              guint          fade_ms)
{
  clutter_actor_remove_all_transitions (self->curtain);
  clutter_actor_set_child_above_sibling (stage (self), self->group, NULL);
  clutter_actor_show (self->group);

  if (fade_ms > 0 && !clutter_actor_is_visible (self->curtain))
    {
      clutter_actor_set_opacity (self->curtain, 0);
      clutter_actor_show (self->curtain);
      clutter_actor_save_easing_state (self->curtain);
      clutter_actor_set_easing_mode (self->curtain, CLUTTER_EASE_IN_QUAD);
      clutter_actor_set_easing_duration (self->curtain, fade_ms);
      clutter_actor_set_opacity (self->curtain, 255);
      clutter_actor_restore_easing_state (self->curtain);
    }
  else
    {
      clutter_actor_set_opacity (self->curtain, 255);
      clutter_actor_show (self->curtain);
    }

  if (!self->unredirect_disabled)
    {
      meta_compositor_disable_unredirect (meta_display_get_compositor (self->display));
      self->unredirect_disabled = TRUE;
    }
}

static void
lower_shield (ShellPlusLock *self)
{
  clutter_actor_remove_all_transitions (self->curtain);
  release_window (self);
  clutter_actor_hide (self->curtain);
  clutter_actor_hide (self->group);
  set_windows_visible (self, TRUE);

  if (self->unredirect_disabled)
    {
      meta_compositor_enable_unredirect (meta_display_get_compositor (self->display));
      self->unredirect_disabled = FALSE;
    }
}

/* ------------------------------------------------------------------------ */
/* State changes                                                            */

static void
on_wake_idle (MetaIdleMonitor *monitor,
              guint            watch_id,
              gpointer         user_data);

static void
on_user_active (MetaIdleMonitor *monitor,
                guint            watch_id,
                gpointer         user_data)
{
  ShellPlusLock *self = user_data;

  self->active_watch = 0;

  if (self->state == STATE_ACTIVE)
    {
      /* Moving the mouse or pressing a key dismisses an unlocked shield. */
      set_inactive (self);
    }
  else if (self->state == STATE_LOCKED)
    {
      /* Like GNOME Shell: input on the lock screen turns the screen on. */
      emit_screensaver (self, "WakeUpScreen", NULL);
      if (!self->wake_idle_watch)
        self->wake_idle_watch =
          meta_idle_monitor_add_idle_watch (self->idle_monitor, WAKE_UP_IDLE_MS,
                                            on_wake_idle, self, NULL);
    }
}

static void
watch_user_active (ShellPlusLock *self)
{
  if (!self->active_watch)
    self->active_watch =
      meta_idle_monitor_add_user_active_watch (self->idle_monitor, on_user_active, self, NULL);
}

static void
on_wake_idle (MetaIdleMonitor *monitor,
              guint            watch_id,
              gpointer         user_data)
{
  ShellPlusLock *self = user_data;

  meta_idle_monitor_remove_watch (self->idle_monitor, self->wake_idle_watch);
  self->wake_idle_watch = 0;
  if (self->state != STATE_INACTIVE)
    watch_user_active (self);
}

static void
changed (ShellPlusLock *self)
{
  gboolean locked = self->state == STATE_LOCKED;

  shellplus_shell_dbus_set_action_mode (self->shell_dbus,
                                        locked ? SHELLPLUS_ACTION_MODE_LOCK_SCREEN :
                                        SHELLPLUS_ACTION_MODE_NORMAL);
  self->handlers.changed (self->user_data);
}

static void
lock_now (ShellPlusLock *self)
{
  State old = self->state;

  if (old == STATE_LOCKED)
    return;

  g_clear_handle_id (&self->fade_id, g_source_remove);
  g_clear_handle_id (&self->lock_delay_id, g_source_remove);

  /* Covered first, before anything else can paint. */
  raise_shield (self, 0);
  set_windows_visible (self, FALSE);
  self->state = STATE_LOCKED;
  if (old == STATE_INACTIVE)
    self->active_since = g_get_monotonic_time ();

  adopt_window (self);
  sync_grab (self);
  focus_window (self);
  watch_user_active (self);

  g_message ("Screen locked");
  if (old == STATE_INACTIVE)
    emit_screensaver (self, "ActiveChanged", g_variant_new ("(b)", TRUE));
  changed (self);
}

static gboolean
lock_delay_cb (gpointer user_data)
{
  ShellPlusLock *self = user_data;

  self->lock_delay_id = 0;
  if (self->state == STATE_ACTIVE && auto_lock_enabled (self))
    lock_now (self);
  return G_SOURCE_REMOVE;
}

static gboolean
fade_done_cb (gpointer user_data)
{
  ShellPlusLock *self = user_data;
  guint delay;

  self->fade_id = 0;
  if (self->state != STATE_ACTIVE || !auto_lock_enabled (self))
    return G_SOURCE_REMOVE;

  delay = g_settings_get_uint (self->screensaver, "lock-delay");
  if (delay == 0)
    lock_now (self);
  else
    self->lock_delay_id = g_timeout_add_seconds (delay, lock_delay_cb, self);
  return G_SOURCE_REMOVE;
}

/* The shield without a lock (yet): idle, or SetActive(true). */
static void
activate (ShellPlusLock *self,
          gboolean       fade)
{
  guint fade_ms = fade && animations_enabled (self) ? IDLE_FADE_MS : 0;

  if (self->state != STATE_INACTIVE)
    return;

  raise_shield (self, fade_ms);
  self->state = STATE_ACTIVE;
  self->active_since = g_get_monotonic_time ();
  sync_grab (self);
  /* Ignore the input that may have triggered this (e.g. SetActive from a
   * key press); only later activity dismisses the shield. */
  g_clear_handle_id (&self->fade_id, g_source_remove);
  self->fade_id = g_timeout_add (fade_ms, fade_done_cb, self);
  watch_user_active (self);

  emit_screensaver (self, "ActiveChanged", g_variant_new ("(b)", TRUE));
  changed (self);
}

static void
set_inactive (ShellPlusLock *self)
{
  if (self->state == STATE_INACTIVE)
    return;

  g_clear_handle_id (&self->fade_id, g_source_remove);
  g_clear_handle_id (&self->lock_delay_id, g_source_remove);
  if (self->active_watch)
    {
      meta_idle_monitor_remove_watch (self->idle_monitor, self->active_watch);
      self->active_watch = 0;
    }
  if (self->wake_idle_watch)
    {
      meta_idle_monitor_remove_watch (self->idle_monitor, self->wake_idle_watch);
      self->wake_idle_watch = 0;
    }

  if (self->state == STATE_LOCKED)
    g_message ("Screen unlocked");
  self->state = STATE_INACTIVE;
  sync_grab (self);
  lower_shield (self);

  emit_screensaver (self, "ActiveChanged", g_variant_new ("(b)", FALSE));
  changed (self);

  /* Back from the lock screen: focus whatever was on top. */
  meta_display_focus_default_window (self->display,
                                     meta_display_get_current_time_roundtrip (self->display));
}

/* ------------------------------------------------------------------------ */
/* Idle                                                                     */

static void
on_idle (MetaIdleMonitor *monitor,
         guint            watch_id,
         gpointer         user_data)
{
  ShellPlusLock *self = user_data;

  if (self->state == STATE_INACTIVE)
    activate (self, TRUE);
}

static void
update_idle_watch (ShellPlusLock *self)
{
  guint delay = g_settings_get_uint (self->session, "idle-delay");

  if (self->idle_watch)
    {
      meta_idle_monitor_remove_watch (self->idle_monitor, self->idle_watch);
      self->idle_watch = 0;
    }
  if (delay > 0)
    self->idle_watch = meta_idle_monitor_add_idle_watch (self->idle_monitor,
                                                         (guint64) delay * 1000,
                                                         on_idle, self, NULL);
}

/* ------------------------------------------------------------------------ */
/* D-Bus                                                                    */

typedef struct
{
  ShellPlusLock *self;
  GDBusMethodInvocation *invocation;
  char *sender;
} UnlockRequest;

static void
on_sender_pid (GObject      *source,
               GAsyncResult *result,
               gpointer      user_data)
{
  UnlockRequest *request = user_data;
  g_autoptr (GVariant) reply = NULL;
  g_autoptr (GError) error = NULL;
  ShellPlusLock *self;
  guint32 pid = 0;
  pid_t panel_pid;

  reply = g_dbus_connection_call_finish (G_DBUS_CONNECTION (source), result, &error);
  if (g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
    {
      g_dbus_method_invocation_return_error_literal (request->invocation, G_DBUS_ERROR,
                                                     G_DBUS_ERROR_FAILED, "Shutting down");
      goto out;
    }

  self = request->self;
  if (reply)
    g_variant_get (reply, "(u)", &pid);
  /* Looked up now, not when the call came in: the panel may have died. */
  panel_pid = self->handlers.get_panel_pid (self->user_data);

  if (!reply || pid == 0 || panel_pid <= 0 || pid != (guint32) panel_pid)
    {
      g_warning ("Ignoring Unlock from %s (pid %u): not the panel", request->sender, pid);
      g_dbus_method_invocation_return_error_literal (request->invocation, G_DBUS_ERROR,
                                                     G_DBUS_ERROR_ACCESS_DENIED,
                                                     "Only the panel may unlock the screen");
      goto out;
    }

  set_inactive (self);
  g_dbus_method_invocation_return_value (request->invocation, NULL);

out:
  g_object_unref (request->invocation);
  g_free (request->sender);
  g_free (request);
}

static void
handle_unlock (ShellPlusLock         *self,
               const char            *sender,
               GDBusMethodInvocation *invocation)
{
  UnlockRequest *request;

  if (self->state == STATE_INACTIVE)
    {
      g_dbus_method_invocation_return_value (invocation, NULL);
      return;
    }

  request = g_new0 (UnlockRequest, 1);
  request->self = self;
  request->invocation = g_object_ref (invocation);
  request->sender = g_strdup (sender);
  g_dbus_connection_call (self->session_bus, "org.freedesktop.DBus", "/org/freedesktop/DBus",
                          "org.freedesktop.DBus", "GetConnectionUnixProcessID",
                          g_variant_new ("(s)", sender), G_VARIANT_TYPE ("(u)"),
                          G_DBUS_CALL_FLAGS_NONE, -1, self->cancellable,
                          on_sender_pid, request);
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
  ShellPlusLock *self = user_data;

  if (g_strcmp0 (interface_name, LOCK_IFACE) == 0 && g_strcmp0 (method_name, "Unlock") == 0)
    {
      handle_unlock (self, sender, invocation);
      return;
    }

  if (g_strcmp0 (method_name, "Lock") == 0)
    {
      shellplus_lock_lock (self);
      g_dbus_method_invocation_return_value (invocation, NULL);
    }
  else if (g_strcmp0 (method_name, "GetActive") == 0)
    {
      g_dbus_method_invocation_return_value (invocation,
                                             g_variant_new ("(b)", self->state != STATE_INACTIVE));
    }
  else if (g_strcmp0 (method_name, "SetActive") == 0)
    {
      gboolean active;

      g_variant_get (parameters, "(b)", &active);
      if (active)
        activate (self, FALSE);
      else if (self->state == STATE_ACTIVE)
        set_inactive (self);
      /* Like GNOME Shell, SetActive(false) never unlocks. */
      g_dbus_method_invocation_return_value (invocation, NULL);
    }
  else if (g_strcmp0 (method_name, "GetActiveTime") == 0)
    {
      guint32 seconds = 0;

      if (self->state != STATE_INACTIVE)
        seconds = (g_get_monotonic_time () - self->active_since) / G_USEC_PER_SEC;
      g_dbus_method_invocation_return_value (invocation, g_variant_new ("(u)", seconds));
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
on_screensaver_bus_acquired (GDBusConnection *connection,
                             const char      *name,
                             gpointer         user_data)
{
  ShellPlusLock *self = user_data;
  g_autoptr (GError) error = NULL;

  g_set_object (&self->session_bus, connection);
  self->screensaver_registration_id =
    g_dbus_connection_register_object (connection, SCREENSAVER_PATH,
                                       self->screensaver_info->interfaces[0],
                                       &interface_vtable, self, NULL, &error);
  if (!self->screensaver_registration_id)
    g_warning ("Failed to export %s: %s", SCREENSAVER_IFACE, error->message);

  g_clear_error (&error);
  self->lock_registration_id =
    g_dbus_connection_register_object (connection, LOCK_PATH,
                                       self->lock_info->interfaces[0],
                                       &interface_vtable, self, NULL, &error);
  if (!self->lock_registration_id)
    g_warning ("Failed to export %s: %s", LOCK_IFACE, error->message);
}

static void
on_screensaver_name_lost (GDBusConnection *connection,
                          const char      *name,
                          gpointer         user_data)
{
  g_warning ("Could not own %s; Super+L and automatic locking from other "
             "services won't reach the lock screen", name);
}

/* ------------------------------------------------------------------------ */
/* logind                                                                   */

static void
on_session_lock (GDBusConnection *connection,
                 const char      *sender_name,
                 const char      *object_path,
                 const char      *interface_name,
                 const char      *signal_name,
                 GVariant        *parameters,
                 gpointer         user_data)
{
  ShellPlusLock *self = user_data;

  g_debug ("logind asked to lock the session");
  shellplus_lock_lock (self);
}

static gboolean
sleep_release_cb (gpointer user_data)
{
  ShellPlusLock *self = user_data;

  self->sleep_release_id = 0;
  release_inhibitor (self);
  return G_SOURCE_REMOVE;
}

static void
on_prepare_for_sleep (GDBusConnection *connection,
                      const char      *sender_name,
                      const char      *object_path,
                      const char      *interface_name,
                      const char      *signal_name,
                      GVariant        *parameters,
                      gpointer         user_data)
{
  ShellPlusLock *self = user_data;
  gboolean about_to_sleep;

  if (!g_variant_is_of_type (parameters, G_VARIANT_TYPE ("(b)")))
    return;
  g_variant_get (parameters, "(b)", &about_to_sleep);

  if (!about_to_sleep)
    {
      g_clear_handle_id (&self->sleep_release_id, g_source_remove);
      take_inhibitor (self);
      return;
    }

  if (!auto_lock_enabled (self))
    {
      release_inhibitor (self);
      return;
    }

  shellplus_lock_lock (self);
  /* Let suspend go ahead once the lock screen is up (see
   * shellplus_lock_window_mapped), or after a while regardless: the black
   * shield is up already. */
  if (window_shown (self))
    release_inhibitor (self);
  else if (!self->sleep_release_id)
    self->sleep_release_id = g_timeout_add (SLEEP_RELEASE_MS, sleep_release_cb, self);
}

static void
release_inhibitor (ShellPlusLock *self)
{
  g_clear_handle_id (&self->sleep_release_id, g_source_remove);
  if (self->inhibit_fd >= 0)
    {
      close (self->inhibit_fd);
      self->inhibit_fd = -1;
    }
}

static void
on_inhibit (GObject      *source,
            GAsyncResult *result,
            gpointer      user_data)
{
  ShellPlusLock *self;
  g_autoptr (GUnixFDList) fds = NULL;
  g_autoptr (GVariant) reply = NULL;
  g_autoptr (GError) error = NULL;
  int index;

  reply = g_dbus_connection_call_with_unix_fd_list_finish (G_DBUS_CONNECTION (source),
                                                           &fds, result, &error);
  if (g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
    return;

  self = user_data;
  self->inhibit_pending = FALSE;
  if (!reply)
    {
      g_warning ("Can't delay suspend for the lock screen: %s", error->message);
      return;
    }

  g_variant_get (reply, "(h)", &index);
  release_inhibitor (self);
  if (fds)
    self->inhibit_fd = g_unix_fd_list_get (fds, index, NULL);
}

static void
take_inhibitor (ShellPlusLock *self)
{
  if (!self->system_bus || !self->session_path || self->inhibit_fd >= 0 ||
      self->inhibit_pending)
    return;

  self->inhibit_pending = TRUE;
  g_dbus_connection_call_with_unix_fd_list (self->system_bus, LOGIND_NAME, LOGIND_PATH,
                                            LOGIND_MANAGER, "Inhibit",
                                            g_variant_new ("(ssss)", "sleep", "GNOME Shell Plus",
                                                           "GNOME Shell Plus needs to lock the screen",
                                                           "delay"),
                                            G_VARIANT_TYPE ("(h)"), G_DBUS_CALL_FLAGS_NONE, -1,
                                            NULL, self->cancellable, on_inhibit, self);
}

static void
unsubscribe_logind (ShellPlusLock *self)
{
  if (self->session_lock_id)
    g_dbus_connection_signal_unsubscribe (self->system_bus, self->session_lock_id);
  if (self->prepare_for_sleep_id)
    g_dbus_connection_signal_unsubscribe (self->system_bus, self->prepare_for_sleep_id);
  self->session_lock_id = 0;
  self->prepare_for_sleep_id = 0;
  g_clear_pointer (&self->session_path, g_free);
  release_inhibitor (self);
}

static void
on_session_found (GObject      *source,
                  GAsyncResult *result,
                  gpointer      user_data)
{
  ShellPlusLock *self;
  g_autoptr (GVariant) reply = NULL;
  g_autoptr (GError) error = NULL;

  reply = g_dbus_connection_call_finish (G_DBUS_CONNECTION (source), result, &error);
  if (g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
    return;

  self = user_data;
  if (!reply)
    {
      /* logind may still be starting up. */
      if (++self->session_lookup_tries < SESSION_LOOKUP_TRIES)
        self->session_retry_id = g_timeout_add (SESSION_RETRY_MS, retry_session_lookup, self);
      else
        g_warning ("No logind session; loginctl lock-session won't lock: %s", error->message);
      return;
    }

  unsubscribe_logind (self);
  g_variant_get (reply, "(o)", &self->session_path);
  g_debug ("logind session %s", self->session_path);

  self->session_lock_id =
    g_dbus_connection_signal_subscribe (self->system_bus, LOGIND_NAME, LOGIND_SESSION, "Lock",
                                        self->session_path, NULL, G_DBUS_SIGNAL_FLAGS_NONE,
                                        on_session_lock, self, NULL);
  self->prepare_for_sleep_id =
    g_dbus_connection_signal_subscribe (self->system_bus, LOGIND_NAME, LOGIND_MANAGER,
                                        "PrepareForSleep", LOGIND_PATH, NULL,
                                        G_DBUS_SIGNAL_FLAGS_NONE, on_prepare_for_sleep,
                                        self, NULL);
  take_inhibitor (self);
}

static void
look_up_session (ShellPlusLock   *self,
                 GDBusConnection *connection)
{
  const char *session_id = g_getenv ("XDG_SESSION_ID");
  gboolean valid = session_id && *session_id;

  /* logind's session ids are alphanumeric; anything else (e.g. a made-up
   * id in a test session) isn't one of its sessions. */
  for (const char *c = session_id; valid && *c; c++)
    valid = g_ascii_isalnum (*c) || *c == '_';

  /* Like GNOME Shell: the session from the environment, else ours. */
  if (valid)
    g_dbus_connection_call (connection, LOGIND_NAME, LOGIND_PATH, LOGIND_MANAGER, "GetSession",
                            g_variant_new ("(s)", session_id), G_VARIANT_TYPE ("(o)"),
                            G_DBUS_CALL_FLAGS_NONE, -1, self->cancellable,
                            on_session_found, self);
  else
    g_dbus_connection_call (connection, LOGIND_NAME, LOGIND_PATH, LOGIND_MANAGER,
                            "GetSessionByPID", g_variant_new ("(u)", (guint32) getpid ()),
                            G_VARIANT_TYPE ("(o)"), G_DBUS_CALL_FLAGS_NONE, -1,
                            self->cancellable, on_session_found, self);
}

static gboolean
retry_session_lookup (gpointer user_data)
{
  ShellPlusLock *self = user_data;

  self->session_retry_id = 0;
  if (self->system_bus)
    look_up_session (self, self->system_bus);
  return G_SOURCE_REMOVE;
}

static void
on_logind_appeared (GDBusConnection *connection,
                    const char      *name,
                    const char      *owner,
                    gpointer         user_data)
{
  ShellPlusLock *self = user_data;

  g_clear_handle_id (&self->session_retry_id, g_source_remove);
  self->session_lookup_tries = 0;
  look_up_session (self, connection);
}

static void
on_logind_vanished (GDBusConnection *connection,
                    const char      *name,
                    gpointer         user_data)
{
  ShellPlusLock *self = user_data;

  g_clear_handle_id (&self->session_retry_id, g_source_remove);
  if (self->system_bus)
    unsubscribe_logind (self);
}

static void
on_system_bus (GObject      *source,
               GAsyncResult *result,
               gpointer      user_data)
{
  ShellPlusLock *self;
  g_autoptr (GError) error = NULL;
  GDBusConnection *connection = g_bus_get_finish (result, &error);

  if (!connection)
    {
      if (!g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
        g_warning ("No system bus; the lock screen won't follow logind: %s", error->message);
      return;
    }

  self = user_data;
  self->system_bus = connection;
  self->logind_watch_id =
    g_bus_watch_name_on_connection (connection, LOGIND_NAME, G_BUS_NAME_WATCHER_FLAGS_NONE,
                                    on_logind_appeared, on_logind_vanished, self, NULL);
}

/* ------------------------------------------------------------------------ */
/* Lock window                                                              */

static void
on_window_unmanaging (MetaWindow    *window,
                      ShellPlusLock *self)
{
  if (window != self->window)
    return;

  g_signal_handlers_disconnect_by_func (window, on_window_unmanaging, self);
  release_window (self);
  self->window = NULL;
  /* The panel crashed or closed it: back to plain black, keyboard grabbed. */
  sync_grab (self);
}

void
shellplus_lock_setup_window (ShellPlusLock *self,
                             MetaWindow    *window)
{
  MetaDisplay *display = self->display;
  int primary = meta_display_get_primary_monitor (display);

  if (self->window && self->window != window)
    {
      g_signal_handlers_disconnect_by_func (self->window, on_window_unmanaging, self);
      release_window (self);
    }
  self->window = window;

  meta_window_set_type (window, META_WINDOW_DOCK);
  meta_window_hide_from_window_list (window);
  meta_window_stick (window);
  meta_window_add_external_constraint (window, self->constraint);
  g_signal_connect (window, "unmanaging", G_CALLBACK (on_window_unmanaging), self);

  if (primary >= 0)
    {
      MtkRectangle monitor;

      meta_display_get_monitor_geometry (display, primary, &monitor);
      meta_window_move_resize_frame (window, FALSE, monitor.x, monitor.y,
                                     monitor.width, monitor.height);
    }

  if (!meta_window_is_hidden (window))
    shellplus_lock_window_mapped (self, window);
}

void
shellplus_lock_window_mapped (ShellPlusLock *self,
                              MetaWindow    *window)
{
  if (window != self->window || self->state != STATE_LOCKED)
    return;

  adopt_window (self);
  sync_grab (self);
  focus_window (self);
  if (self->sleep_release_id)
    release_inhibitor (self);
}

/* ------------------------------------------------------------------------ */

gboolean
shellplus_lock_is_locked (ShellPlusLock *self)
{
  return self->state == STATE_LOCKED;
}

gboolean
shellplus_lock_is_active (ShellPlusLock *self)
{
  return self->state != STATE_INACTIVE;
}

void
shellplus_lock_lock (ShellPlusLock *self)
{
  if (lock_disabled (self))
    {
      g_message ("Screen lock is locked down (disable-lock-screen); not locking");
      return;
    }
  lock_now (self);
}

gboolean
shellplus_lock_filter_keybinding (ShellPlusLock  *self,
                                  MetaKeyBinding *binding)
{
  const char *name = meta_key_binding_get_name (binding);

  if (self->state == STATE_INACTIVE)
    return FALSE;

  /* Keyboard layouts can be switched to type the password (GNOME Shell
   * allows this in every mode). */
  if (g_strcmp0 (name, "switch-input-source") == 0 ||
      g_strcmp0 (name, "switch-input-source-backward") == 0)
    return FALSE;

  /* Media keys, volume, brightness, power button…: whatever
   * gnome-settings-daemon grabbed for the lock screen. */
  if (shellplus_shell_dbus_binding_allowed (self->shell_dbus, name,
                                            SHELLPLUS_ACTION_MODE_LOCK_SCREEN |
                                            SHELLPLUS_ACTION_MODE_UNLOCK_SCREEN))
    return FALSE;

  return TRUE;
}

gboolean
shellplus_lock_sync_focus (ShellPlusLock *self)
{
  if (self->state == STATE_INACTIVE)
    return FALSE;

  if (self->state == STATE_LOCKED)
    focus_window (self);
  return TRUE;
}

ShellPlusLock *
shellplus_lock_new (MetaDisplay                 *display,
                    ShellPlusShellDBus          *shell_dbus,
                    const ShellPlusLockHandlers *handlers,
                    gpointer                     user_data)
{
  ShellPlusLock *self = g_new0 (ShellPlusLock, 1);
  MetaBackend *backend = meta_context_get_backend (meta_display_get_context (display));
  GSettingsSchemaSource *source = g_settings_schema_source_get_default ();
  g_autoptr (GSettingsSchema) lockdown = NULL;
  CoglColor black;

  self->display = display;
  self->shell_dbus = shell_dbus;
  self->handlers = *handlers;
  self->user_data = user_data;
  self->inhibit_fd = -1;
  self->cancellable = g_cancellable_new ();
  self->constraint = shellplus_panel_constraint_new (display, SHELLPLUS_ROLE_LOCK);

  /* The shield: covers the whole stage (all monitors), takes pointer input. */
  self->group = clutter_actor_new ();
  clutter_actor_set_name (self->group, "shellplus-lock-shield");
  clutter_actor_set_reactive (self->group, TRUE);
  clutter_actor_add_constraint (self->group,
                                clutter_bind_constraint_new (stage (self), CLUTTER_BIND_SIZE, 0));
  self->curtain = clutter_actor_new ();
  cogl_color_init_from_4f (&black, 0.0f, 0.0f, 0.0f, 1.0f);
  clutter_actor_set_background_color (self->curtain, &black);
  clutter_actor_set_reactive (self->curtain, TRUE);
  clutter_actor_add_constraint (self->curtain,
                                clutter_bind_constraint_new (self->group, CLUTTER_BIND_SIZE, 0));
  clutter_actor_hide (self->curtain);
  self->holder = clutter_actor_new ();
  clutter_actor_add_child (self->group, self->curtain);
  clutter_actor_add_child (self->group, self->holder);
  clutter_actor_hide (self->group);
  clutter_actor_add_child (stage (self), self->group);

  self->screensaver = g_settings_new ("org.gnome.desktop.screensaver");
  self->session = g_settings_new ("org.gnome.desktop.session");
  self->interface = g_settings_new ("org.gnome.desktop.interface");
  lockdown = source ? g_settings_schema_source_lookup (source, "org.gnome.desktop.lockdown", TRUE) : NULL;
  if (lockdown)
    self->lockdown = g_settings_new_full (lockdown, NULL, NULL);

  self->idle_monitor = meta_backend_get_core_idle_monitor (backend);
  g_signal_connect_swapped (self->session, "changed::idle-delay",
                            G_CALLBACK (update_idle_watch), self);
  update_idle_watch (self);

  self->screensaver_info = g_dbus_node_info_new_for_xml (screensaver_xml, NULL);
  self->lock_info = g_dbus_node_info_new_for_xml (lock_xml, NULL);
  self->screensaver_owner_id =
    g_bus_own_name (G_BUS_TYPE_SESSION, SCREENSAVER_NAME, G_BUS_NAME_OWNER_FLAGS_REPLACE,
                    on_screensaver_bus_acquired, NULL, on_screensaver_name_lost, self, NULL);
  g_bus_get (G_BUS_TYPE_SYSTEM, self->cancellable, on_system_bus, self);

  return self;
}

void
shellplus_lock_free (ShellPlusLock *self)
{
  g_cancellable_cancel (self->cancellable);
  g_clear_object (&self->cancellable);

  g_clear_handle_id (&self->fade_id, g_source_remove);
  g_clear_handle_id (&self->lock_delay_id, g_source_remove);
  g_clear_handle_id (&self->sleep_release_id, g_source_remove);
  g_clear_handle_id (&self->session_retry_id, g_source_remove);
  if (self->idle_watch)
    meta_idle_monitor_remove_watch (self->idle_monitor, self->idle_watch);
  if (self->active_watch)
    meta_idle_monitor_remove_watch (self->idle_monitor, self->active_watch);
  if (self->wake_idle_watch)
    meta_idle_monitor_remove_watch (self->idle_monitor, self->wake_idle_watch);

  if (self->system_bus)
    {
      unsubscribe_logind (self);
      if (self->logind_watch_id)
        g_bus_unwatch_name (self->logind_watch_id);
      g_clear_object (&self->system_bus);
    }
  release_inhibitor (self);

  if (self->screensaver_registration_id)
    g_dbus_connection_unregister_object (self->session_bus, self->screensaver_registration_id);
  if (self->lock_registration_id)
    g_dbus_connection_unregister_object (self->session_bus, self->lock_registration_id);
  g_bus_unown_name (self->screensaver_owner_id);
  g_clear_object (&self->session_bus);
  g_dbus_node_info_unref (self->screensaver_info);
  g_dbus_node_info_unref (self->lock_info);

  if (self->window)
    g_signal_handlers_disconnect_by_func (self->window, on_window_unmanaging, self);
  if (self->grab)
    {
      clutter_grab_dismiss (self->grab);
      g_clear_object (&self->grab);
    }
  g_signal_handlers_disconnect_by_data (self->session, self);
  g_clear_object (&self->screensaver);
  g_clear_object (&self->session);
  g_clear_object (&self->lockdown);
  g_clear_object (&self->interface);
  g_clear_object (&self->constraint);
  clutter_actor_destroy (self->group);
  g_free (self);
}
