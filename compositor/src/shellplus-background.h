#pragma once

#include <clutter/clutter.h>
#include <meta/display.h>

G_BEGIN_DECLS

typedef struct _ShellPlusBackground ShellPlusBackground;

/* Draws the desktop wallpaper on every monitor, following
 * org.gnome.desktop.background (and picture-uri-dark when the
 * org.gnome.desktop.interface color-scheme prefers dark). */
ShellPlusBackground *shellplus_background_new (MetaDisplay  *display,
                                               ClutterActor *window_group);

void shellplus_background_free (ShellPlusBackground *self);

G_END_DECLS
