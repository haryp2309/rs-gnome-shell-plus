#pragma once

#include <meta/display.h>

G_BEGIN_DECLS

typedef struct _ShellPlusScreenshot ShellPlusScreenshot;

/* Returns the panel process's pid, or 0 if it isn't running. */
typedef pid_t (*ShellPlusScreenshotPanelPid) (gpointer user_data);

/* Screen captures: GNOME Shell's org.gnome.Shell.Screenshot API (for
 * xdg-desktop-portal-gnome, gnome-screenshot, …), the private
 * org.gnome.ShellPlus.Screenshot interface the panel's screenshot UI uses,
 * and the Print key bindings. See shellplus-screenshot.c. */
ShellPlusScreenshot *shellplus_screenshot_new (MetaDisplay                 *display,
                                               ShellPlusScreenshotPanelPid  panel_pid,
                                               gpointer                     user_data);

/* Asks the panel to open the screenshot UI: "" (selection), "screen" or
 * "window". */
void shellplus_screenshot_show_ui (ShellPlusScreenshot *self,
                                   const char          *mode);

/* Refuses all captures (while the screen is locked). */
void shellplus_screenshot_set_blocked (ShellPlusScreenshot *self,
                                       gboolean             blocked);

void shellplus_screenshot_free (ShellPlusScreenshot *self);

G_END_DECLS
