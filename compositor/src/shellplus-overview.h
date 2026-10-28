#pragma once

#include <meta/display.h>
#include <meta/window.h>

G_BEGIN_DECLS

typedef struct _ShellPlusOverview ShellPlusOverview;

/* Returns TRUE for windows the overview must leave out (the panel's own). */
typedef gboolean (*ShellPlusOverviewFilter) (MetaWindow *window,
                                             gpointer    user_data);

/* The compositor's half of the overview, which the panel draws. Exports
 * org.gnome.ShellPlus.Overview next to the panel interface: the window
 * list, window images (shared memory), window actions, and requests to
 * show the overview (Super, Super+A, …). See shellplus-overview.c. */
ShellPlusOverview *shellplus_overview_new (MetaDisplay             *display,
                                           ShellPlusOverviewFilter  filter,
                                           gpointer                 user_data);

/* Asks the panel to show/hide the overview: "toggle", "windows" or "apps". */
void shellplus_overview_request (ShellPlusOverview *self,
                                 const char        *mode);

/* TRUE while the overview is up and should keep the keyboard focus (e.g.
 * across workspace switches), until the panel activates a window. */
gboolean shellplus_overview_holds_focus (ShellPlusOverview *self);

void shellplus_overview_free (ShellPlusOverview *self);

G_END_DECLS
