#pragma once

#include <gio/gio.h>
#include <sys/types.h>

G_BEGIN_DECLS

/* org.gnome.SessionManager.EndSessionDialog for gnome-session.
 *
 * gnome-session opens its log out / restart / power off prompt by calling
 * /org/gnome/SessionManager/EndSessionDialog on whoever owns org.gnome.Shell,
 * which is the compositor. The dialog itself is the panel's
 * (panel/src/end_session.rs, owning org.gnome.ShellPlus.EndSessionDialog):
 * this object forwards Open/Close to it and re-emits its signals, only
 * trusting the panel process the compositor started. */

typedef struct _ShellPlusEndSession ShellPlusEndSession;

typedef pid_t (*ShellPlusEndSessionGetPanelPid) (gpointer user_data);

ShellPlusEndSession *shellplus_end_session_new (ShellPlusEndSessionGetPanelPid get_panel_pid,
                                                gpointer                       user_data);

void shellplus_end_session_free (ShellPlusEndSession *self);

G_END_DECLS
