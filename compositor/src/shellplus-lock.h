#pragma once

#include <sys/types.h>
#include <meta/display.h>
#include <meta/keybindings.h>
#include <meta/window.h>

#include "shellplus-shell-dbus.h"

G_BEGIN_DECLS

typedef struct _ShellPlusLock ShellPlusLock;

typedef struct
{
  /* The shield went up or down, or the screen got locked or unlocked. */
  void (*changed)       (gpointer user_data);
  /* The pid of the panel process the compositor spawned, or 0 if it isn't
   * running. Only that process may unlock. */
  pid_t (*get_panel_pid) (gpointer user_data);
} ShellPlusLockHandlers;

/* The screen shield and lock state. The compositor owns it; the panel
 * only draws the lock UI (a window titled "Lock Screen") and asks to
 * unlock once the user authenticated. Exports org.gnome.ScreenSaver and
 * org.gnome.ShellPlus.Lock, follows logind (Lock, PrepareForSleep) and
 * locks when idle. See shellplus-lock.c. */
ShellPlusLock *shellplus_lock_new (MetaDisplay                 *display,
                                   ShellPlusShellDBus          *shell_dbus,
                                   const ShellPlusLockHandlers *handlers,
                                   gpointer                     user_data);

/* Locked: only the lock window is shown and only the panel can unlock. */
gboolean shellplus_lock_is_locked (ShellPlusLock *self);

/* The shield is up (locked, or blanking before the lock). */
gboolean shellplus_lock_is_active (ShellPlusLock *self);

void shellplus_lock_lock (ShellPlusLock *self);

/* A panel window titled "Lock Screen" appeared / was mapped. */
void shellplus_lock_setup_window  (ShellPlusLock *self,
                                   MetaWindow    *window);
void shellplus_lock_window_mapped (ShellPlusLock *self,
                                   MetaWindow    *window);

/* For MetaPluginClass.keybinding_filter: TRUE (filter out) for every
 * binding that mustn't run while the shield is up. */
gboolean shellplus_lock_filter_keybinding (ShellPlusLock  *self,
                                           MetaKeyBinding *binding);

/* Call when the focus window changed. Returns TRUE while the lock screen
 * owns the keyboard focus (the caller must then leave the focus alone). */
gboolean shellplus_lock_sync_focus (ShellPlusLock *self);

void shellplus_lock_free (ShellPlusLock *self);

G_END_DECLS
