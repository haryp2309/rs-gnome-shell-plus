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
 * - In the applications mode, like GNOME's AppSwitcherPopup, Down or
 *   switch-group (Alt+`) shows the selected app's windows as thumbnails
 *   under it and moves into them (Left/Right or Alt+` / Alt+~ walk them, Up
 *   goes back to the apps); resting on an app with several windows for
 *   THUMBNAIL_POPUP_MS shows them without moving into them.
 *
 * The panel draws the pop-up from the D-Bus signals below; see
 * panel/src/switcher.rs. It fetches window previews by id through
 * org.gnome.ShellPlus.Overview GetPreview (shellplus-overview.c).
 *
 *   SwitcherShow (s mode, a(ssia(ts)) items, i selected)
 *       mode "applications" or "windows"; per item: app id, title (of the
 *       most recent window), number of windows, and its windows (id, title)
 *       most recently used first
 *   SwitcherSelect (i item, i window)
 *       window: index into the item's windows of the selected thumbnail,
 *       -1 if the selection is on the item itself
 *   SwitcherThumbnails (i item)
 *       show the item's window thumbnails (-1: hide them)
 *   SwitcherHide () */

#define POPUP_DELAY_MS 150
/* GNOME Shell's THUMBNAIL_POPUP_TIME. */
#define THUMBNAIL_POPUP_MS 500
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
  int thumbnails;     /* item whose thumbnails are up, or -1 */
  int window;         /* selected thumbnail, or -1 (the item is selected) */
  guint thumbnail_id; /* THUMBNAIL_POPUP_MS timeout */
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
emit_select (ShellPlusSwitcher *self)
{
  if (self->shown)
    shellplus_dbus_emit (self->dbus, "SwitcherSelect",
                         g_variant_new ("(ii)", self->selected, self->window));
}

static void
emit_thumbnails (ShellPlusSwitcher *self)
{
  if (self->shown)
    shellplus_dbus_emit (self->dbus, "SwitcherThumbnails",
                         g_variant_new ("(i)", self->thumbnails));
}

static void
emit_show (ShellPlusSwitcher *self)
{
  GVariantBuilder builder;

  g_variant_builder_init (&builder, G_VARIANT_TYPE ("a(ssia(ts))"));
  for (guint i = 0; i < self->items->len; i++)
    {
      Item *item = g_ptr_array_index (self->items, i);

      g_variant_builder_open (&builder, G_VARIANT_TYPE ("(ssia(ts))"));
      g_variant_builder_add (&builder, "s", item->app_id);
      g_variant_builder_add (&builder, "s", item->title);
      g_variant_builder_add (&builder, "i", (int) item->windows->len);
      g_variant_builder_open (&builder, G_VARIANT_TYPE ("a(ts)"));
      for (guint j = 0; j < item->windows->len; j++)
        {
          MetaWindow *window = g_ptr_array_index (item->windows, j);
          const char *title = meta_window_get_title (window);

          g_variant_builder_add (&builder, "(ts)", meta_window_get_id (window),
                                 title ? title : "");
        }
      g_variant_builder_close (&builder);
      g_variant_builder_close (&builder);
    }

  shellplus_dbus_emit (self->dbus, "SwitcherShow",
                       g_variant_new ("(sa(ssia(ts))i)",
                                      self->mode == MODE_APPLICATIONS ? "applications" : "windows",
                                      &builder, self->selected));
  self->shown = TRUE;

  if (self->thumbnails >= 0)
    {
      emit_thumbnails (self);
      emit_select (self);
    }
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
  g_clear_handle_id (&self->thumbnail_id, g_source_remove);
  self->thumbnails = -1;
  self->window = -1;
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
      int index = self->window >= 0 && self->window < (int) item->windows->len ? self->window : 0;

      window = g_ptr_array_index (item->windows, index);
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

static int
wrap (int index,
      int n)
{
  return n > 0 ? ((index % n) + n) % n : 0;
}

static gboolean
thumbnail_popup_cb (gpointer user_data)
{
  ShellPlusSwitcher *self = user_data;

  self->thumbnail_id = 0;
  self->thumbnails = self->selected;
  emit_thumbnails (self);
  return G_SOURCE_REMOVE;
}

/* Like AppSwitcherPopup._select(): selects item @index and, if @window is
 * not -1, that window's thumbnail. */
static void
select_item_window (ShellPlusSwitcher *self,
                    int                index,
                    int                window)
{
  int n = self->items->len;
  Item *item;

  if (n == 0)
    return;

  index = wrap (index, n);
  item = g_ptr_array_index (self->items, index);

  if (self->thumbnails >= 0 && (index != self->thumbnails || window < 0))
    {
      self->thumbnails = -1;
      emit_thumbnails (self);
    }
  g_clear_handle_id (&self->thumbnail_id, g_source_remove);

  self->selected = index;
  self->window = window >= 0 ? wrap (window, item->windows->len) : -1;

  if (self->window >= 0)
    {
      if (self->thumbnails < 0)
        {
          self->thumbnails = index;
          emit_thumbnails (self);
        }
    }
  else if (self->mode == MODE_APPLICATIONS && item->windows->len > 1)
    {
      self->thumbnail_id = g_timeout_add (THUMBNAIL_POPUP_MS, thumbnail_popup_cb, self);
    }

  emit_select (self);
}

static void
select_index (ShellPlusSwitcher *self,
              int                index)
{
  select_item_window (self, index, -1);
}

/* Moves through the selected app's windows (switch-group, or Left/Right
 * in the thumbnails). Starts at the first one if none is selected yet. */
static void
select_window (ShellPlusSwitcher *self,
               int                delta)
{
  Item *item;
  int n, window;

  if (self->selected < 0 || self->selected >= (int) self->items->len)
    return;
  item = g_ptr_array_index (self->items, self->selected);
  n = item->windows->len;

  if (self->window < 0)
    window = delta >= 0 ? 0 : n - 1;
  else
    window = wrap (self->window + delta, n);
  select_item_window (self, self->selected, window);
}

static void
initial_selection (ShellPlusSwitcher *self,
                   gboolean           backward)
{
  int n = self->items->len;

  select_index (self, backward ? n - 1 : (n > 1 ? 1 : 0));
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
  guint keysym = clutter_event_get_key_symbol (event);

  if (!self->active)
    return CLUTTER_EVENT_PROPAGATE;

  if (self->mode == MODE_APPLICATIONS)
    {
      gboolean in_thumbnails = self->window >= 0;

      switch (keysym)
        {
        case CLUTTER_KEY_Down:
        case CLUTTER_KEY_KP_Down:
          if (!in_thumbnails)
            select_item_window (self, self->selected, 0);
          return CLUTTER_EVENT_STOP;
        case CLUTTER_KEY_Up:
        case CLUTTER_KEY_KP_Up:
          if (in_thumbnails)
            {
              select_index (self, self->selected);
              /* Back on the app, the thumbnails don't pop up again. */
              g_clear_handle_id (&self->thumbnail_id, g_source_remove);
            }
          return CLUTTER_EVENT_STOP;
        case CLUTTER_KEY_Left:
        case CLUTTER_KEY_KP_Left:
          if (in_thumbnails)
            {
              select_window (self, -1);
              return CLUTTER_EVENT_STOP;
            }
          break;
        case CLUTTER_KEY_Right:
        case CLUTTER_KEY_KP_Right:
          if (in_thumbnails)
            {
              select_window (self, 1);
              return CLUTTER_EVENT_STOP;
            }
          break;
        default:
          break;
        }
    }

  switch (keysym)
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
      /* In the applications mode, switch-group walks the selected app's
       * windows; anything else moves between the items. */
      if (self->mode == MODE_APPLICATIONS && g_str_has_prefix (name, "switch-group"))
        select_window (self, backward ? -1 : 1);
      else
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

void
shellplus_switcher_cancel (ShellPlusSwitcher *self)
{
  if (self->active)
    cancel (self);
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
  self->thumbnails = -1;
  self->window = -1;
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
