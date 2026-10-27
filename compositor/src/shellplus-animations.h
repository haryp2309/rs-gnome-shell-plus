#pragma once

#include <meta/meta-plugin.h>

G_BEGIN_DECLS

/* Returns TRUE for windows that must never be animated (the panel's bar,
 * banners and OSD). */
typedef gboolean (*ShellPlusAnimationsSkipFunc) (MetaWindow *window,
                                                 gpointer    user_data);

/* Window animations modelled on GNOME Shell's windowManager.js: map,
 * destroy, minimize/unminimize and the workspace switch slide.
 *
 * Call shellplus_animations_install() from the plugin's class_init: it fills
 * in the destroy, minimize, unminimize, switch_workspace and kill_* vfuncs.
 * Call shellplus_animations_attach() from the plugin's start vfunc, and
 * shellplus_animations_map() from its map vfunc (which completes the map,
 * immediately or once the animation ends).
 *
 * Animations are skipped when org.gnome.desktop.interface enable-animations
 * is off. SHELLPLUS_SLOW_ANIMATIONS=<factor> in the environment slows them
 * down, for testing. */
void shellplus_animations_install (MetaPluginClass *plugin_class);

void shellplus_animations_attach  (MetaPlugin                  *plugin,
                                   ShellPlusAnimationsSkipFunc  skip_func,
                                   gpointer                     user_data);

void shellplus_animations_map     (MetaPlugin      *plugin,
                                   MetaWindowActor *actor);

G_END_DECLS
