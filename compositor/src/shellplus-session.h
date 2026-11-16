#pragma once

#include <meta/meta-context.h>

G_BEGIN_DECLS

/* Running under gnome-session (data/gnome-shell-plus-gnome-session.desktop)
 * the compositor is a systemd user service with Type=notify, like
 * org.gnome.Shell@.service: it tells systemd it's ready once the panel is
 * on screen, and logs out through org.gnome.SessionManager. Under the
 * plain session script none of this applies and nothing changes. */

/* Takes NOTIFY_SOCKET out of the environment so the panel, Xwayland and
 * apps don't inherit it. Call first thing in main(). */
void shellplus_session_init (void);

/* Sends READY=1 to systemd's notify socket, once. No-op without one. */
void shellplus_session_notify_ready (void);

/* Asks gnome-session to log out (no confirmation: the user already
 * confirmed in the panel). Without gnome-session, or if the call fails,
 * terminates the compositor like before. */
void shellplus_session_log_out (MetaContext *context);

G_END_DECLS
