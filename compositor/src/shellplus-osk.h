#pragma once

#include <sys/types.h>
#include <meta/boxes.h>
#include <meta/display.h>
#include <meta/window.h>

#include "shellplus-input-method.h"

G_BEGIN_DECLS

/* The compositor half of the on-screen keyboard (GNOME Shell's
 * js/ui/keyboard.js). The panel draws the keyboard in a window titled
 * "Screen Keyboard" and decides when to show it; this places that window
 * at the bottom of the primary monitor (never focused, so the text field
 * keeps the focus), slides it in, reserves its space with a strut, moves
 * a focused window up if the keyboard would cover its text cursor, tells
 * the panel about the text field (org.gnome.ShellPlus.Osk) and types what
 * the panel sends. See shellplus-osk.c. */

typedef struct _ShellPlusOsk ShellPlusOsk;

typedef struct
{
  /* The pid of the panel process, or 0. Only it may type. */
  pid_t    (*get_panel_pid)  (gpointer user_data);
  /* The screen is locked (nothing may be typed into hidden windows). */
  gboolean (*is_locked)      (gpointer user_data);
  /* The keyboard's strut appeared, went away or changed size. */
  void     (*struts_changed) (gpointer user_data);
} ShellPlusOskHandlers;

ShellPlusOsk *shellplus_osk_new (MetaDisplay                *display,
                                 ShellPlusInputMethod       *input_method,
                                 const ShellPlusOskHandlers *handlers,
                                 gpointer                    user_data);

/* A panel window titled "Screen Keyboard" appeared / was mapped. */
void shellplus_osk_setup_window  (ShellPlusOsk *self,
                                  MetaWindow   *window);
void shellplus_osk_window_mapped (ShellPlusOsk *self,
                                  MetaWindow   *window);

/* The keyboard's space at the bottom of the primary monitor while it is
 * shown; FALSE if it isn't. */
gboolean shellplus_osk_get_strut (ShellPlusOsk *self,
                                  MetaStrut    *strut);

void shellplus_osk_free (ShellPlusOsk *self);

G_END_DECLS
