#pragma once

#include <meta/display.h>

#include "shellplus-dbus.h"

G_BEGIN_DECLS

typedef struct _ShellPlusShellDBus ShellPlusShellDBus;

/* GNOME Shell's Shell.ActionMode flags, which GrabAccelerator callers use
 * to say in which modes their keys work. */
#define SHELLPLUS_ACTION_MODE_NORMAL        (1 << 0)
#define SHELLPLUS_ACTION_MODE_LOCK_SCREEN   (1 << 2)
#define SHELLPLUS_ACTION_MODE_UNLOCK_SCREEN (1 << 3)

/* The parts of GNOME Shell's org.gnome.Shell interface that GNOME's own
 * services rely on: gnome-settings-daemon grabs media keys through
 * GrabAccelerator(s) and shows volume/brightness pop-ups through ShowOSD.
 * OSD requests are forwarded to the panel. */
ShellPlusShellDBus *shellplus_shell_dbus_new (MetaDisplay   *display,
                                              ShellPlusDBus *panel_dbus);

/* The mode reported with AcceleratorActivated (NORMAL by default). */
void shellplus_shell_dbus_set_action_mode (ShellPlusShellDBus *self,
                                           guint               action_mode);

/* TRUE if binding_name is a grabbed accelerator (Mutter's
 * "external-grab-<action>") whose grab allows one of action_mode's flags. */
gboolean shellplus_shell_dbus_binding_allowed (ShellPlusShellDBus *self,
                                               const char         *binding_name,
                                               guint               action_mode);

void shellplus_shell_dbus_free (ShellPlusShellDBus *self);

G_END_DECLS
