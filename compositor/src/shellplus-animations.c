#define G_LOG_DOMAIN "shellplus"

#include "shellplus-animations.h"

#include <clutter/clutter.h>
#include <meta/compositor.h>
#include <meta/display.h>
#include <meta/meta-window-actor.h>
#include <meta/meta-workspace-manager.h>
#include <meta/window.h>
#include <meta/workspace.h>

/* Durations (ms) and easing, after GNOME Shell's windowManager.js and
 * workspaceAnimation.js. */
#define MAP_TIME              150
#define DIALOG_MAP_TIME       100
#define DESTROY_TIME          150
#define DIALOG_DESTROY_TIME   100
#define MINIMIZE_TIME         400
#define SWITCH_WORKSPACE_TIME 250
#define MAP_SCALE             0.9
#define DESTROY_SCALE         0.8
/* If a transition never reports back (e.g. its actor lost its frame clock),
 * finish the effect anyway this long after it should have ended. */
#define SAFETY_MARGIN_MS      1000

typedef enum
{
  EFFECT_MAP,
  EFFECT_DESTROY,
  EFFECT_MINIMIZE,
  EFFECT_UNMINIMIZE,
} EffectKind;

static const char * const effect_names[] = { "map", "destroy", "minimize", "unminimize" };

/* The properties window effects animate; removed when an effect is killed. */
static const char * const effect_properties[] = { "opacity", "scale-x", "scale-y" };

typedef struct _ShellPlusAnimations ShellPlusAnimations;

typedef struct
{
  ShellPlusAnimations *self;
  MetaWindowActor *actor;
  EffectKind kind;
  gulong completed_id;
  gulong destroy_id;
  guint timeout_id;
} Effect;

typedef struct
{
  ClutterActor *actor;  /* weak pointer */
  float from;
  float to;
} SwitchEntry;

struct _ShellPlusAnimations
{
  MetaPlugin *plugin;
  ShellPlusAnimationsSkipFunc skip_func;
  gpointer skip_data;
  GSettings *interface_settings;
  double slow_factor;

  GHashTable *effects;  /* MetaWindowActor -> Effect */

  ClutterTimeline *switch_timeline;
  GArray *switch_entries;  /* SwitchEntry */
};

static GQuark animations_quark;

static ShellPlusAnimations *
get_animations (MetaPlugin *plugin)
{
  return animations_quark ? g_object_get_qdata (G_OBJECT (plugin), animations_quark) : NULL;
}

static void
animations_free (gpointer data)
{
  ShellPlusAnimations *self = data;

  /* The plugin is going away; nothing is left to complete effects for. */
  if (self->switch_timeline)
    {
      g_signal_handlers_disconnect_by_data (self->switch_timeline, self);
      clutter_timeline_stop (self->switch_timeline);
      g_clear_object (&self->switch_timeline);
    }
  g_clear_pointer (&self->switch_entries, g_array_unref);
  g_clear_pointer (&self->effects, g_hash_table_unref);
  g_clear_object (&self->interface_settings);
  g_free (self);
}

static gboolean
animations_enabled (ShellPlusAnimations *self)
{
  return g_settings_get_boolean (self->interface_settings, "enable-animations");
}

static guint
scaled_duration (ShellPlusAnimations *self,
                 guint                ms)
{
  return (guint) (ms * self->slow_factor);
}

static const char *
window_title (MetaWindowActor *actor)
{
  MetaWindow *window = meta_window_actor_get_meta_window (actor);
  const char *title = window ? meta_window_get_title (window) : NULL;

  return title ? title : "(untitled)";
}

static gboolean
is_modal_dialog (MetaWindow *window)
{
  return meta_window_get_window_type (window) == META_WINDOW_MODAL_DIALOG ||
         meta_window_is_attached_dialog (window);
}

/* GNOME Shell animates normal windows and dialogs; menus, tooltips, docks,
 * notifications etc. appear and disappear at once. */
static gboolean
should_animate (ShellPlusAnimations *self,
                MetaWindowActor     *actor)
{
  MetaWindow *window = meta_window_actor_get_meta_window (actor);

  if (!window || !animations_enabled (self))
    return FALSE;

  if (self->skip_func && self->skip_func (window, self->skip_data))
    return FALSE;

  switch (meta_window_get_window_type (window))
    {
    case META_WINDOW_NORMAL:
    case META_WINDOW_DIALOG:
    case META_WINDOW_MODAL_DIALOG:
      break;
    default:
      return FALSE;
    }

  return clutter_actor_get_stage (CLUTTER_ACTOR (actor)) != NULL;
}

static void
complete (MetaPlugin      *plugin,
          MetaWindowActor *actor,
          EffectKind       kind)
{
  switch (kind)
    {
    case EFFECT_MAP:
      meta_plugin_map_completed (plugin, actor);
      break;
    case EFFECT_DESTROY:
      meta_plugin_destroy_completed (plugin, actor);
      break;
    case EFFECT_MINIMIZE:
      meta_plugin_minimize_completed (plugin, actor);
      break;
    case EFFECT_UNMINIMIZE:
      meta_plugin_unminimize_completed (plugin, actor);
      break;
    }
}

static void
complete_unanimated (MetaPlugin      *plugin,
                     MetaWindowActor *actor,
                     EffectKind       kind)
{
  g_debug ("animation: %s not animated '%s'", effect_names[kind], window_title (actor));
  complete (plugin, actor, kind);
}

static void
log_actor_state (MetaWindowActor *actor,
                 EffectKind       kind,
                 const char      *what)
{
  ClutterActor *a = CLUTTER_ACTOR (actor);
  float sx, sy, tx, ty, x, y, w, h;
  double scale_x, scale_y;

  clutter_actor_get_scale (a, &scale_x, &scale_y);
  clutter_actor_get_translation (a, &tx, &ty, NULL);
  clutter_actor_get_pivot_point (a, &sx, &sy);
  clutter_actor_get_position (a, &x, &y);
  clutter_actor_get_size (a, &w, &h);

  g_debug ("animation: %s %s '%s' scale=%.2f,%.2f opacity=%u translation=%.0f,%.0f "
           "pivot=%.2f,%.2f rect=%.0f,%.0f,%.0fx%.0f",
           effect_names[kind], what, window_title (actor), scale_x, scale_y,
           clutter_actor_get_opacity (a), tx, ty, sx, sy, x, y, w, h);
}

static void
reset_actor (ClutterActor *actor)
{
  clutter_actor_set_scale (actor, 1.0, 1.0);
  clutter_actor_set_opacity (actor, 255);
  clutter_actor_set_pivot_point (actor, 0.0f, 0.0f);
}

/* Ends @effect (naturally or because it was interrupted), restores the
 * actor and tells Mutter. Safe to call exactly once per effect. */
static void
finish_effect (Effect   *effect,
               gboolean  interrupted)
{
  ShellPlusAnimations *self = effect->self;
  MetaWindowActor *actor = effect->actor;
  EffectKind kind = effect->kind;
  guint i;

  g_hash_table_steal (self->effects, actor);
  g_clear_signal_handler (&effect->completed_id, actor);
  g_clear_signal_handler (&effect->destroy_id, actor);
  g_clear_handle_id (&effect->timeout_id, g_source_remove);
  g_free (effect);

  for (i = 0; i < G_N_ELEMENTS (effect_properties); i++)
    clutter_actor_remove_transition (CLUTTER_ACTOR (actor), effect_properties[i]);

  /* A destroyed actor is about to go away; leave it as it is. */
  if (kind != EFFECT_DESTROY)
    reset_actor (CLUTTER_ACTOR (actor));

  log_actor_state (actor, kind, interrupted ? "interrupted" : "completed");
  complete (self->plugin, actor, kind);
}

static void
on_effect_transitions_completed (ClutterActor *actor,
                                 Effect       *effect)
{
  finish_effect (effect, FALSE);
}

static gboolean
on_effect_timeout (gpointer user_data)
{
  Effect *effect = user_data;

  effect->timeout_id = 0;
  g_warning ("%s animation of '%s' did not finish; completing it",
             effect_names[effect->kind], window_title (effect->actor));
  finish_effect (effect, TRUE);
  return G_SOURCE_REMOVE;
}

/* The actor is being destroyed under us (without a destroy effect); it is
 * too late to report back to Mutter, just forget the effect. */
static void
on_effect_actor_destroyed (ClutterActor *actor,
                           Effect       *effect)
{
  g_debug ("animation: %s '%s' dropped, actor destroyed",
           effect_names[effect->kind], window_title (effect->actor));
  g_hash_table_steal (effect->self->effects, actor);
  g_clear_signal_handler (&effect->completed_id, actor);
  g_clear_signal_handler (&effect->destroy_id, actor);
  g_clear_handle_id (&effect->timeout_id, g_source_remove);
  g_free (effect);
}

static void
kill_effect (ShellPlusAnimations *self,
             MetaWindowActor     *actor)
{
  Effect *effect = g_hash_table_lookup (self->effects, actor);

  if (effect)
    finish_effect (effect, TRUE);
}

typedef struct
{
  double scale_x, scale_y;
  guint8 opacity;
} ActorState;

/* Moves @actor to @from at once, then eases it to @to, scaling around
 * (@pivot_x, @pivot_y). The matching
 * meta_plugin_*_completed() is called when the transitions end, or when the
 * effect is killed. */
static void
run_effect (ShellPlusAnimations *self,
            MetaWindowActor     *actor,
            EffectKind           kind,
            float                pivot_x,
            float                pivot_y,
            const ActorState    *from,
            const ActorState    *to,
            guint                duration,
            ClutterAnimationMode mode)
{
  ClutterActor *a = CLUTTER_ACTOR (actor);
  Effect *effect;

  kill_effect (self, actor);

  clutter_actor_set_pivot_point (a, pivot_x, pivot_y);
  clutter_actor_set_scale (a, from->scale_x, from->scale_y);
  clutter_actor_set_opacity (a, from->opacity);
  if (kind == EFFECT_MAP || kind == EFFECT_UNMINIMIZE)
    clutter_actor_show (a);

  duration = scaled_duration (self, duration);

  clutter_actor_save_easing_state (a);
  clutter_actor_set_easing_duration (a, duration);
  clutter_actor_set_easing_mode (a, mode);
  clutter_actor_set_scale (a, to->scale_x, to->scale_y);
  clutter_actor_set_opacity (a, to->opacity);
  clutter_actor_restore_easing_state (a);

  if (!clutter_actor_get_transition (a, "opacity") &&
      !clutter_actor_get_transition (a, "scale-x") &&
      !clutter_actor_get_transition (a, "scale-y"))
    {
      /* Nothing to animate (e.g. the actor isn't mapped). */
      reset_actor (a);
      log_actor_state (actor, kind, "skipped");
      complete (self->plugin, actor, kind);
      return;
    }

  effect = g_new0 (Effect, 1);
  effect->self = self;
  effect->actor = actor;
  effect->kind = kind;
  effect->completed_id =
    g_signal_connect (a, "transitions-completed",
                      G_CALLBACK (on_effect_transitions_completed), effect);
  effect->destroy_id =
    g_signal_connect (a, "destroy", G_CALLBACK (on_effect_actor_destroyed), effect);
  effect->timeout_id = g_timeout_add (duration + SAFETY_MARGIN_MS, on_effect_timeout, effect);
  g_hash_table_insert (self->effects, actor, effect);

  log_actor_state (actor, kind, "started");
}

/* ------------------------------------------------------------------------ */
/* Map / destroy                                                            */

void
shellplus_animations_map (MetaPlugin      *plugin,
                          MetaWindowActor *actor)
{
  ShellPlusAnimations *self = get_animations (plugin);
  ActorState from, to = { 1.0, 1.0, 255 };
  float pivot_y;
  guint duration;

  if (!self || !should_animate (self, actor))
    {
      complete_unanimated (plugin, actor, EFFECT_MAP);
      return;
    }

  from = to;
  from.opacity = 0;
  if (is_modal_dialog (meta_window_actor_get_meta_window (actor)))
    {
      /* Unfolds downwards from its top edge, like GNOME's attached dialogs. */
      pivot_y = 0.0f;
      from.scale_y = 0.0;
      duration = DIALOG_MAP_TIME;
    }
  else
    {
      pivot_y = 0.5f;
      from.scale_x = from.scale_y = MAP_SCALE;
      duration = MAP_TIME;
    }

  run_effect (self, actor, EFFECT_MAP, 0.5f, pivot_y, &from, &to, duration,
              CLUTTER_EASE_OUT_QUAD);
}

static void
animations_destroy (MetaPlugin      *plugin,
                    MetaWindowActor *actor)
{
  ShellPlusAnimations *self = get_animations (plugin);
  ActorState from, to;
  float pivot_y;
  guint duration;

  /* Start from wherever a running effect left the window (Mutter normally
   * kills it first, which finishes it, like GNOME Shell). */
  clutter_actor_get_scale (CLUTTER_ACTOR (actor), &from.scale_x, &from.scale_y);
  from.opacity = clutter_actor_get_opacity (CLUTTER_ACTOR (actor));
  if (self)
    kill_effect (self, actor);

  if (!self || !should_animate (self, actor) ||
      !clutter_actor_is_mapped (CLUTTER_ACTOR (actor)))
    {
      complete_unanimated (plugin, actor, EFFECT_DESTROY);
      return;
    }

  to = from;
  to.opacity = 0;
  if (is_modal_dialog (meta_window_actor_get_meta_window (actor)))
    {
      pivot_y = 0.0f;
      to.scale_y = 0.0;
      duration = DIALOG_DESTROY_TIME;
    }
  else
    {
      pivot_y = 0.5f;
      to.scale_x = to.scale_y = DESTROY_SCALE;
      duration = DESTROY_TIME;
    }

  run_effect (self, actor, EFFECT_DESTROY, 0.5f, pivot_y, &from, &to, duration,
              CLUTTER_EASE_OUT_QUAD);
}

/* ------------------------------------------------------------------------ */
/* Minimize / unminimize                                                    */

/* Without a dock icon to aim at, GNOME Shell sends minimized windows to the
 * bottom center of their monitor. Scaling around that point (as the pivot,
 * which may lie outside the actor) moves the window there in a straight
 * line, so no translation is needed. Returns FALSE if there's no monitor. */
static gboolean
get_minimize_pivot (MetaWindowActor *actor,
                    float           *pivot_x,
                    float           *pivot_y)
{
  MetaWindow *window = meta_window_actor_get_meta_window (actor);
  MetaDisplay *display = meta_window_get_display (window);
  int monitor = meta_window_get_monitor (window);
  MtkRectangle geometry;
  float x, y, width, height;

  if (monitor < 0 || monitor >= meta_display_get_n_monitors (display))
    return FALSE;

  meta_display_get_monitor_geometry (display, monitor, &geometry);
  clutter_actor_get_position (CLUTTER_ACTOR (actor), &x, &y);
  clutter_actor_get_size (CLUTTER_ACTOR (actor), &width, &height);
  if (width < 1 || height < 1)
    return FALSE;

  *pivot_x = (geometry.x + geometry.width / 2.0f - x) / width;
  *pivot_y = (geometry.y + geometry.height - y) / height;
  return TRUE;
}

static void
animations_minimize (MetaPlugin      *plugin,
                     MetaWindowActor *actor)
{
  ShellPlusAnimations *self = get_animations (plugin);
  ActorState from = { 1.0, 1.0, 255 }, to = { 0.0, 0.0, 0 };
  float pivot_x, pivot_y;

  if (self)
    kill_effect (self, actor);

  if (!self || !should_animate (self, actor) ||
      !clutter_actor_is_mapped (CLUTTER_ACTOR (actor)) ||
      !get_minimize_pivot (actor, &pivot_x, &pivot_y))
    {
      complete_unanimated (plugin, actor, EFFECT_MINIMIZE);
      return;
    }

  run_effect (self, actor, EFFECT_MINIMIZE, pivot_x, pivot_y, &from, &to,
              MINIMIZE_TIME, CLUTTER_EASE_OUT_EXPO);
}

static void
animations_unminimize (MetaPlugin      *plugin,
                       MetaWindowActor *actor)
{
  ShellPlusAnimations *self = get_animations (plugin);
  ActorState from = { 0.0, 0.0, 0 }, to = { 1.0, 1.0, 255 };
  float pivot_x, pivot_y;

  if (self)
    kill_effect (self, actor);

  if (!self || !should_animate (self, actor) ||
      !get_minimize_pivot (actor, &pivot_x, &pivot_y))
    {
      complete_unanimated (plugin, actor, EFFECT_UNMINIMIZE);
      return;
    }

  run_effect (self, actor, EFFECT_UNMINIMIZE, pivot_x, pivot_y, &from, &to,
              MINIMIZE_TIME, CLUTTER_EASE_OUT_EXPO);
}

static void
animations_kill_window_effects (MetaPlugin      *plugin,
                                MetaWindowActor *actor)
{
  ShellPlusAnimations *self = get_animations (plugin);

  if (self)
    kill_effect (self, actor);
}

/* ------------------------------------------------------------------------ */
/* Workspace switch                                                         */

/* The windows of both workspaces slide sideways, like GNOME Shell's
 * horizontal workspace layout. Mutter keeps the old workspace's windows
 * visible until the switch completes. One timeline on the stage drives all
 * of them: the new workspace's actors may not be shown yet when the switch
 * starts, and a hidden actor's own transitions wouldn't run. */
static void
finish_switch_workspace (ShellPlusAnimations *self,
                         gboolean             interrupted)
{
  guint i;

  if (self->switch_timeline)
    {
      g_signal_handlers_disconnect_by_data (self->switch_timeline, self);
      clutter_timeline_stop (self->switch_timeline);
      g_clear_object (&self->switch_timeline);
    }

  for (i = 0; i < self->switch_entries->len; i++)
    {
      SwitchEntry *entry = &g_array_index (self->switch_entries, SwitchEntry, i);

      if (entry->actor)
        {
          clutter_actor_set_translation (entry->actor, 0.0f, 0.0f, 0.0f);
          g_object_remove_weak_pointer (G_OBJECT (entry->actor), (gpointer *) &entry->actor);
        }
    }
  g_array_set_size (self->switch_entries, 0);

  g_debug ("animation: switch-workspace %s", interrupted ? "interrupted" : "completed");
  meta_plugin_switch_workspace_completed (self->plugin);
}

static void
on_switch_new_frame (ClutterTimeline     *timeline,
                     int                  msecs,
                     ShellPlusAnimations *self)
{
  double progress = clutter_timeline_get_progress (timeline);
  guint i;

  for (i = 0; i < self->switch_entries->len; i++)
    {
      SwitchEntry *entry = &g_array_index (self->switch_entries, SwitchEntry, i);

      if (entry->actor)
        clutter_actor_set_translation (entry->actor,
                                       entry->from + (entry->to - entry->from) * progress,
                                       0.0f, 0.0f);
    }
}

static void
on_switch_stopped (ClutterTimeline     *timeline,
                   gboolean             is_finished,
                   ShellPlusAnimations *self)
{
  finish_switch_workspace (self, !is_finished);
}

static void
animations_switch_workspace (MetaPlugin          *plugin,
                             int                  from,
                             int                  to,
                             MetaMotionDirection  direction)
{
  ShellPlusAnimations *self = get_animations (plugin);
  MetaDisplay *display = meta_plugin_get_display (plugin);
  MetaCompositor *compositor = meta_display_get_compositor (display);
  MetaWorkspaceManager *wm = meta_display_get_workspace_manager (display);
  MetaWorkspace *from_ws = meta_workspace_manager_get_workspace_by_index (wm, from);
  MetaWorkspace *to_ws = meta_workspace_manager_get_workspace_by_index (wm, to);
  float sign = to > from ? 1.0f : -1.0f;
  GList *actors, *l;
  guint i;

  if (!self || from == to || !from_ws || !to_ws || !animations_enabled (self))
    {
      g_debug ("animation: switch-workspace %d -> %d not animated", from, to);
      meta_plugin_switch_workspace_completed (plugin);
      return;
    }

  if (self->switch_timeline)
    finish_switch_workspace (self, TRUE);

  actors = meta_compositor_get_window_actors (compositor);
  for (l = actors; l; l = l->next)
    {
      MetaWindowActor *actor = l->data;
      MetaWindow *window = meta_window_actor_get_meta_window (actor);
      MtkRectangle monitor;
      SwitchEntry entry;
      float width;

      if (!window || meta_window_actor_is_destroyed (actor) ||
          meta_window_is_on_all_workspaces (window) ||
          !meta_window_showing_on_its_workspace (window) ||
          (self->skip_func && self->skip_func (window, self->skip_data)) ||
          meta_window_get_monitor (window) < 0)
        continue;

      meta_display_get_monitor_geometry (display, meta_window_get_monitor (window), &monitor);
      width = monitor.width;

      if (meta_window_located_on_workspace (window, from_ws))
        entry = (SwitchEntry) { CLUTTER_ACTOR (actor), 0.0f, -sign * width };
      else if (meta_window_located_on_workspace (window, to_ws))
        entry = (SwitchEntry) { CLUTTER_ACTOR (actor), sign * width, 0.0f };
      else
        continue;

      g_array_append_val (self->switch_entries, entry);
      g_debug ("animation: switch-workspace moves '%s'", window_title (actor));
    }

  if (self->switch_entries->len == 0)
    {
      g_debug ("animation: switch-workspace %d -> %d has no windows to move", from, to);
      meta_plugin_switch_workspace_completed (plugin);
      return;
    }

  for (i = 0; i < self->switch_entries->len; i++)
    {
      SwitchEntry *entry = &g_array_index (self->switch_entries, SwitchEntry, i);

      g_object_add_weak_pointer (G_OBJECT (entry->actor), (gpointer *) &entry->actor);
      clutter_actor_set_translation (entry->actor, entry->from, 0.0f, 0.0f);
    }

  self->switch_timeline =
    clutter_timeline_new_for_actor (CLUTTER_ACTOR (meta_compositor_get_stage (compositor)),
                                    scaled_duration (self, SWITCH_WORKSPACE_TIME));
  clutter_timeline_set_progress_mode (self->switch_timeline, CLUTTER_EASE_OUT_CUBIC);
  g_signal_connect (self->switch_timeline, "new-frame",
                    G_CALLBACK (on_switch_new_frame), self);
  g_signal_connect (self->switch_timeline, "stopped",
                    G_CALLBACK (on_switch_stopped), self);
  clutter_timeline_start (self->switch_timeline);

  g_debug ("animation: switch-workspace %d -> %d started (%u windows, %u ms)",
           from, to, self->switch_entries->len,
           scaled_duration (self, SWITCH_WORKSPACE_TIME));
}

static void
animations_kill_switch_workspace (MetaPlugin *plugin)
{
  ShellPlusAnimations *self = get_animations (plugin);

  if (self && self->switch_timeline)
    finish_switch_workspace (self, TRUE);
}

/* ------------------------------------------------------------------------ */

void
shellplus_animations_install (MetaPluginClass *plugin_class)
{
  plugin_class->destroy = animations_destroy;
  plugin_class->minimize = animations_minimize;
  plugin_class->unminimize = animations_unminimize;
  plugin_class->switch_workspace = animations_switch_workspace;
  plugin_class->kill_window_effects = animations_kill_window_effects;
  plugin_class->kill_switch_workspace = animations_kill_switch_workspace;
}

void
shellplus_animations_attach (MetaPlugin                  *plugin,
                             ShellPlusAnimationsSkipFunc  skip_func,
                             gpointer                     user_data)
{
  ShellPlusAnimations *self = g_new0 (ShellPlusAnimations, 1);
  const char *slow = g_getenv ("SHELLPLUS_SLOW_ANIMATIONS");

  self->plugin = plugin;
  self->skip_func = skip_func;
  self->skip_data = user_data;
  self->interface_settings = g_settings_new ("org.gnome.desktop.interface");
  self->effects = g_hash_table_new (NULL, NULL);
  self->switch_entries = g_array_new (FALSE, FALSE, sizeof (SwitchEntry));
  self->slow_factor = slow && *slow ? g_ascii_strtod (slow, NULL) : 1.0;
  if (self->slow_factor <= 0.0)
    self->slow_factor = 1.0;

  if (!animations_quark)
    animations_quark = g_quark_from_static_string ("shellplus-animations");
  g_object_set_qdata_full (G_OBJECT (plugin), animations_quark, self, animations_free);
}
