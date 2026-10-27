#define G_LOG_DOMAIN "shellplus"

#include "shellplus-switcher.h"

#include <clutter/clutter.h>
#include <meta/compositor.h>
#include <meta/keybindings.h>
#include <meta/meta-backend.h>
#include <meta/meta-context.h>
#include <meta/meta-cursor-tracker.h>
#include <meta/meta-workspace-manager.h>

/* GNOME Shell's Alt-Tab, modelled on js/ui/switcherPopup.js and
 * altTab.js:
 *
 * - The keybinding handler builds the list from the current workspace's
 *   tab list (most recently used first) and preselects the next entry (or
 *   the last one when going backward).
 * - If the binding's modifier is already up (a quick tap), the selection is
 *   activated straight away. Otherwise the stage is grabbed so the
 *   compositor sees every key, including the modifier's release, and the
 *   pop-up is shown after POPUP_DELAY_MS.
 * - While the grab is active, Mutter still runs keybindings, so further
 *   Alt+Tab / Alt+Shift+Tab presses arrive in the binding handler and move
 *   the selection. Arrow keys, Escape and the modifier release arrive at the
 *   grab actor.
 *
 * The panel draws the pop-up from the SwitcherShow / SwitcherSelect /
 * SwitcherHide D-Bus signals; see panel/src/switcher.rs. */

#define POPUP_DELAY_MS 150
/* Title of the panel's pop-up window (panel/src/switcher.rs). */
#define SWITCHER_TITLE "Switcher"

typedef enum
{
  MODE_APPLICATIONS,  /* one entry per app, activates its most recent window */
  MODE_WINDOWS,       /* one entry per window */
  MODE_GROUP,         /* the windows of the focused window's app */
} Mode;

typedef struct
{
  char *app_id;
  char *title;        /* the (most recent) window's title */
  GPtrArray *windows; /* MetaWindow, most recently used first */
} Item;

struct _ShellPlusSwitcher
{
  MetaDisplay *display;
  ShellPlusDBus *dbus;
  ShellPlusSwitcherFilter filter;
  gpointer user_data;

  ClutterActor *grab_actor;
  ClutterGrab *grab;

  gboolean active;
  Mode mode;
  GPtrArray *items;   /* Item */
  GPtrArray *watched; /* MetaWindow whose "unmanaged" we listen to */
  int selected;
  ClutterModifierType modifier;
  guint show_id;
  gboolean shown;
};

static const char *bindings[] = {
  "switch-applications",
  "switch-applications-backward",
  "switch-windows",
  "switch-windows-backward",
  "switch-group",
  "switch-group-backward",
};

static void
item_free (Item *item)
{
  g_free (item->app_id);
  g_free (item->title);
  g_ptr_array_unref (item->windows);
  g_free (item);
}

/* The id the panel uses to find the app's .desktop file and icon. */
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

static Item *
item_new (MetaWindow *window,
          char       *app_id)
{
  Item *item = g_new0 (Item, 1);

  item->app_id = app_id;
  item->title = g_strdup (meta_window_get_title (window) ? meta_window_get_title (window) : "");
  item->windows = g_ptr_array_new_with_free_func (g_object_unref);
  g_ptr_array_add (item->windows, g_object_ref (window));
  return item;
}

static MetaWorkspace *
active_workspace (ShellPlusSwitcher *self)
{
  return meta_workspace_manager_get_active_workspace (
    meta_display_get_workspace_manager (self->display));
}

/* ------------------------------------------------------------------------ */
/* Talking to the panel                                                     */

static void
emit_show (ShellPlusSwitcher *self)
{
  GVariantBuilder builder;

  g_variant_builder_init (&builder, G_VARIANT_TYPE ("a(ssi)"));
  for (guint i = 0; i < self->items->len; i++)
    {
      Item *item = g_ptr_array_index (self->items, i);

      g_variant_builder_add (&builder, "(ssi)", item->app_id, item->title,
                             (int) item->windows->len);
    }

  shellplus_dbus_emit (self->dbus, "SwitcherShow",
                       g_variant_new ("(sa(ssi)i)",
                                      self->mode == MODE_APPLICATIONS ? "applications" : "windows",
                                      &builder, self->selected));
  self->shown = TRUE;
}

static gboolean
show_cb (gpointer user_data)
{
  ShellPlusSwitcher *self = user_data;

  self->show_id = 0;
  emit_show (self);
  return G_SOURCE_REMOVE;
}

/* ------------------------------------------------------------------------ */
/* Building the list                                                        */

static void
build_items (ShellPlusSwitcher *self,
             Mode               mode)
{
  GList *windows = meta_display_get_tab_list (self->display, META_TAB_LIST_NORMAL,
                                              active_workspace (self));
  g_autofree char *group_id = NULL;
  GList *l;

  if (mode == MODE_GROUP)
    {
      MetaWindow *focus = meta_display_get_focus_window (self->display);

      if (!focus && windows)
        focus = windows->data;
      if (focus)
        group_id = window_app_id (focus);
    }

  for (l = windows; l; l = l->next)
    {
      MetaWindow *window = l->data;
      char *app_id;

      if (self->filter && self->filter (window, self->user_data))
        continue;

      app_id = window_app_id (window);

      if (mode == MODE_GROUP && g_strcmp0 (app_id, group_id) != 0)
        {
          g_free (app_id);
          continue;
        }

      /* Windows without any id never group with each other. */
      if (mode == MODE_APPLICATIONS && *app_id)
        {
          Item *existing = NULL;

          for (guint i = 0; i < self->items->len && !existing; i++)
            {
              Item *item = g_ptr_array_index (self->items, i);

              if (g_strcmp0 (item->app_id, app_id) == 0)
                existing = item;
            }
          if (existing)
            {
              g_ptr_array_add (existing->windows, g_object_ref (window));
              g_free (app_id);
              continue;
            }
        }

      g_ptr_array_add (self->items, item_new (window, app_id));
    }
  g_list_free (windows);
}

/* ------------------------------------------------------------------------ */
/* Ending                                                                   */

static void
on_window_unmanaged (MetaWindow        *window,
                     ShellPlusSwitcher *self);

static void
teardown (ShellPlusSwitcher *self)
{
  g_clear_handle_id (&self->show_id, g_source_remove);
  if (self->shown)
    shellplus_dbus_emit (self->dbus, "SwitcherHide", g_variant_new ("()"));
  self->shown = FALSE;

  if (self->grab)
    {
      clutter_grab_dismiss (self->grab);
      g_clear_object (&self->grab);
    }
  clutter_actor_hide (self->grab_actor);

  for (guint i = 0; i < self->watched->len; i++)
    g_signal_handlers_disconnect_by_func (g_ptr_array_index (self->watched, i),
                                          on_window_unmanaged, self);
  g_ptr_array_set_size (self->watched, 0);
  g_ptr_array_set_size (self->items, 0);
  self->active = FALSE;
}

static void
cancel (ShellPlusSwitcher *self)
{
  teardown (self);
}

/* time is the triggering event's timestamp: Mutter ignores activations
 * older than the last user interaction, so it must be in the same clock as
 * the input events. */
static void
finish (ShellPlusSwitcher *self,
        guint32            time)
{
  MetaWindow *window = NULL;

  if (self->selected >= 0 && self->selected < (int) self->items->len)
    {
      Item *item = g_ptr_array_index (self->items, self->selected);

      window = g_ptr_array_index (item->windows, 0);
    }

  if (!time)
    time = meta_display_get_current_time (self->display);
  if (!time)
    time = meta_display_get_current_time_roundtrip (self->display);

  /* Activate while the grab is still up (the items keep the window
   * alive), so that dropping the grab hands keyboard focus straight to the
   * new window instead of briefly back to the old one. */
  if (window)
    meta_window_activate (window, time);

  teardown (self);
}

static void
on_window_unmanaged (MetaWindow        *window,
                     ShellPlusSwitcher *self)
{
  /* GNOME Shell drops the entry; cancelling keeps this simple and safe. */
  cancel (self);
}

/* ------------------------------------------------------------------------ */
/* Selection                                                                */

static void
select_index (ShellPlusSwitcher *self,
              int                index)
{
  int n = self->items->len;

  if (n == 0)
    return;

  self->selected = ((index % n) + n) % n;
  if (self->shown)
    shellplus_dbus_emit (self->dbus, "SwitcherSelect",
                         g_variant_new ("(i)", self->selected));
}

static void
initial_selection (ShellPlusSwitcher *self,
                   gboolean           backward)
{
  int n = self->items->len;

  if (backward)
    self->selected = n - 1;
  else
    self->selected = n > 1 ? 1 : 0;
}

/* Like GNOME Shell's primaryModifier(): the highest modifier bit of the
 * binding, e.g. Alt for Alt+Shift+Tab. Releasing it ends the switcher. */
static ClutterModifierType
primary_modifier (ClutterModifierType mask)
{
  ClutterModifierType primary = 1;

  if (mask == 0)
    return 0;
  while (mask > 1)
    {
      mask >>= 1;
      primary <<= 1;
    }
  return primary;
}

static ClutterModifierType
current_modifiers (ShellPlusSwitcher *self)
{
  MetaBackend *backend = meta_context_get_backend (meta_display_get_context (self->display));
  ClutterModifierType mods = 0;

  meta_cursor_tracker_get_pointer (meta_backend_get_cursor_tracker (backend), NULL, &mods);
  return mods;
}

/* ------------------------------------------------------------------------ */
/* Input while the switcher is up                                           */

static gboolean
on_key_press (ClutterActor      *actor,
              ClutterEvent      *event,
              ShellPlusSwitcher *self)
{
  if (!self->active)
    return CLUTTER_EVENT_PROPAGATE;

  switch (clutter_event_get_key_symbol (event))
    {
    case CLUTTER_KEY_Escape:
      cancel (self);
      break;
    case CLUTTER_KEY_Left:
    case CLUTTER_KEY_KP_Left:
    case CLUTTER_KEY_ISO_Left_Tab:
      select_index (self, self->selected - 1);
      break;
    case CLUTTER_KEY_Right:
    case CLUTTER_KEY_KP_Right:
    case CLUTTER_KEY_Tab:
      if (clutter_event_get_state (event) & CLUTTER_SHIFT_MASK)
        select_index (self, self->selected - 1);
      else
        select_index (self, self->selected + 1);
      break;
    case CLUTTER_KEY_Return:
    case CLUTTER_KEY_KP_Enter:
      finish (self, clutter_event_get_time (event));
      break;
    default:
      break;
    }
  return CLUTTER_EVENT_STOP;
}

static gboolean
on_key_release (ClutterActor      *actor,
                ClutterEvent      *event,
                ShellPlusSwitcher *self)
{
  if (self->active && !(current_modifiers (self) & self->modifier))
    finish (self, clutter_event_get_time (event));
  return CLUTTER_EVENT_STOP;
}

static gboolean
point_in_popup (ShellPlusSwitcher *self,
                float              x,
                float              y)
{
  GList *windows = meta_display_list_all_windows (self->display);
  gboolean inside = FALSE;

  for (GList *l = windows; l && !inside; l = l->next)
    {
      MetaWindow *window = l->data;
      MtkRectangle rect;

      if (g_strcmp0 (meta_window_get_title (window), SWITCHER_TITLE) != 0 ||
          !self->filter || !self->filter (window, self->user_data))
        continue;

      meta_window_get_frame_rect (window, &rect);
      inside = x >= rect.x && x < rect.x + rect.width &&
               y >= rect.y && y < rect.y + rect.height;
    }
  g_list_free (windows);
  return inside;
}

/* Like GNOME Shell, a click outside the pop-up cancels. */
static gboolean
on_button_press (ClutterActor      *actor,
                 ClutterEvent      *event,
                 ShellPlusSwitcher *self)
{
  float x, y;

  if (!self->active)
    return CLUTTER_EVENT_PROPAGATE;

  clutter_event_get_coords (event, &x, &y);
  if (!self->shown || !point_in_popup (self, x, y))
    cancel (self);
  return CLUTTER_EVENT_STOP;
}

/* ------------------------------------------------------------------------ */
/* Starting                                                                 */

static void
start (ShellPlusSwitcher *self,
       Mode               mode,
       gboolean           backward,
       MetaKeyBinding    *binding,
       guint32            time)
{
  MetaCompositor *compositor = meta_display_get_compositor (self->display);
  ClutterStage *stage = CLUTTER_STAGE (meta_compositor_get_stage (compositor));
  float width, height;

  self->mode = mode;
  build_items (self, mode);
  if (self->items->len == 0)
    return;

  initial_selection (self, backward);
  self->modifier = primary_modifier (meta_key_binding_get_mask (binding));

  /* A binding without modifiers can't be held: switch right away. */
  if (!self->modifier)
    {
      self->active = TRUE;
      finish (self, time);
      return;
    }

  self->active = TRUE;
  for (guint i = 0; i < self->items->len; i++)
    {
      Item *item = g_ptr_array_index (self->items, i);

      for (guint j = 0; j < item->windows->len; j++)
        {
          MetaWindow *window = g_ptr_array_index (item->windows, j);

          g_signal_connect (window, "unmanaged", G_CALLBACK (on_window_unmanaged), self);
          g_ptr_array_add (self->watched, g_object_ref (window));
        }
    }

  clutter_actor_get_size (CLUTTER_ACTOR (stage), &width, &height);
  clutter_actor_set_size (self->grab_actor, width, height);
  clutter_actor_show (self->grab_actor);
  self->grab = clutter_stage_grab (stage, self->grab_actor);

  /* The modifier may have been released before we had the grab, in which
   * case we'd never see the release. */
  if (!(current_modifiers (self) & self->modifier))
    {
      finish (self, time);
      return;
    }

  self->show_id = g_timeout_add (POPUP_DELAY_MS, show_cb, self);
}

static void
on_switch (MetaDisplay        *display,
           MetaWindow         *window,
           const ClutterEvent *event,
           MetaKeyBinding     *binding,
           gpointer            user_data)
{
  ShellPlusSwitcher *self = user_data;
  const char *name = meta_key_binding_get_name (binding);
  gboolean backward = meta_key_binding_is_reversed (binding);
  Mode mode;

  if (self->active)
    {
      select_index (self, self->selected + (backward ? -1 : 1));
      return;
    }

  if (g_str_has_prefix (name, "switch-applications"))
    mode = MODE_APPLICATIONS;
  else if (g_str_has_prefix (name, "switch-group"))
    mode = MODE_GROUP;
  else
    mode = MODE_WINDOWS;

  start (self, mode, backward, binding, clutter_event_get_time (event));
}

gboolean
shellplus_switcher_filter_keybinding (ShellPlusSwitcher *self,
                                      MetaKeyBinding    *binding)
{
  const char *name = meta_key_binding_get_name (binding);

  if (!self->active)
    return FALSE;

  for (guint i = 0; i < G_N_ELEMENTS (bindings); i++)
    {
      if (g_strcmp0 (name, bindings[i]) == 0)
        return FALSE;
    }
  return TRUE;
}

ShellPlusSwitcher *
shellplus_switcher_new (MetaDisplay             *display,
                        ShellPlusDBus           *dbus,
                        ShellPlusSwitcherFilter  filter,
                        gpointer                 user_data)
{
  ShellPlusSwitcher *self = g_new0 (ShellPlusSwitcher, 1);
  MetaCompositor *compositor = meta_display_get_compositor (display);

  self->display = display;
  self->dbus = dbus;
  self->filter = filter;
  self->user_data = user_data;
  self->items = g_ptr_array_new_with_free_func ((GDestroyNotify) item_free);
  self->watched = g_ptr_array_new_with_free_func (g_object_unref);

  /* An invisible, stage-sized actor that holds the grab while the
   * switcher is up and receives the keys Mutter doesn't consume. */
  self->grab_actor = clutter_actor_new ();
  clutter_actor_set_name (self->grab_actor, "shellplus-switcher-grab");
  clutter_actor_set_reactive (self->grab_actor, TRUE);
  clutter_actor_hide (self->grab_actor);
  clutter_actor_add_child (CLUTTER_ACTOR (meta_compositor_get_stage (compositor)),
                           self->grab_actor);
  g_signal_connect (self->grab_actor, "key-press-event", G_CALLBACK (on_key_press), self);
  g_signal_connect (self->grab_actor, "key-release-event", G_CALLBACK (on_key_release), self);
  g_signal_connect (self->grab_actor, "button-press-event", G_CALLBACK (on_button_press), self);

  for (guint i = 0; i < G_N_ELEMENTS (bindings); i++)
    {
      if (!meta_keybindings_set_custom_handler (bindings[i], on_switch, self, NULL))
        g_warning ("Could not take over the %s keybinding", bindings[i]);
    }

  return self;
}

void
shellplus_switcher_free (ShellPlusSwitcher *self)
{
  for (guint i = 0; i < G_N_ELEMENTS (bindings); i++)
    meta_keybindings_set_custom_handler (bindings[i], NULL, NULL, NULL);

  if (self->active)
    teardown (self);
  g_signal_handlers_disconnect_by_data (self->grab_actor, self);
  clutter_actor_destroy (self->grab_actor);
  g_ptr_array_unref (self->items);
  g_ptr_array_unref (self->watched);
  g_free (self);
}
