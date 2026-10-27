#pragma once

#include <meta/display.h>
#include <meta/keybindings.h>
#include <meta/window.h>

#include "shellplus-dbus.h"

G_BEGIN_DECLS

typedef struct _ShellPlusSwitcher ShellPlusSwitcher;

/* Returns TRUE for windows that must never show up in the switcher (the
 * panel process's own windows). */
typedef gboolean (*ShellPlusSwitcherFilter) (MetaWindow *window,
                                             gpointer    user_data);

/* Takes over Mutter's switch-applications, switch-windows and switch-group
 * keybindings (and their -backward variants) and implements GNOME Shell's
 * Alt-Tab behaviour. The pop-up itself is drawn by the panel, which gets
 * the SwitcherShow / SwitcherSelect / SwitcherHide signals. */
ShellPlusSwitcher *shellplus_switcher_new (MetaDisplay             *display,
                                           ShellPlusDBus           *dbus,
                                           ShellPlusSwitcherFilter  filter,
                                           gpointer                 user_data);

/* For MetaPluginClass.keybinding_filter: while the switcher is up, only
 * its own bindings run (like GNOME Shell's POPUP action mode), so that e.g.
 * Alt+Escape cancels the switcher instead of running cycle-windows. */
gboolean shellplus_switcher_filter_keybinding (ShellPlusSwitcher *self,
                                               MetaKeyBinding    *binding);

void shellplus_switcher_free (ShellPlusSwitcher *self);

G_END_DECLS
