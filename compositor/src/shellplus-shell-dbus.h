#pragma once

#include <meta/display.h>

#include "shellplus-dbus.h"

G_BEGIN_DECLS

typedef struct _ShellPlusShellDBus ShellPlusShellDBus;

/* The parts of GNOME Shell's org.gnome.Shell interface that GNOME's own
 * services rely on: gnome-settings-daemon grabs media keys through
 * GrabAccelerator(s) and shows volume/brightness pop-ups through ShowOSD.
 * OSD requests are forwarded to the panel. */
ShellPlusShellDBus *shellplus_shell_dbus_new (MetaDisplay   *display,
                                              ShellPlusDBus *panel_dbus);

void shellplus_shell_dbus_free (ShellPlusShellDBus *self);

G_END_DECLS
