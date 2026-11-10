#pragma once

#include <clutter/clutter.h>
#include <ibus.h>
#include <meta/display.h>

G_BEGIN_DECLS

/* IBus input methods, the way GNOME Shell does them on Wayland: the
 * compositor is Mutter's ClutterInputMethod (text-input-v3 clients reach
 * it), owns one IBus input context and forwards key events to it. It is
 * also the IBus panel: candidates, auxiliary text and preedit go to the
 * panel process, which draws the pop-up (panel/src/input_method.rs). */

#define SHELLPLUS_TYPE_INPUT_METHOD (shellplus_input_method_get_type ())
G_DECLARE_FINAL_TYPE (ShellPlusInputMethod, shellplus_input_method,
                      SHELLPLUS, INPUT_METHOD, ClutterInputMethod)

typedef void (*ShellPlusInputMethodNotify) (gpointer user_data);

/* Starts ibus-daemon (unless one is running) and connects to it. */
ShellPlusInputMethod *shellplus_input_method_new (MetaDisplay *display);

/* engines_changed: the list of IBus engines changed (the daemon came or
 * went). popup_changed: the pop-up was shown, hidden, or the text cursor
 * moved; the panel's pop-up window has to be placed again. */
void shellplus_input_method_set_callbacks (ShellPlusInputMethod       *self,
                                           ShellPlusInputMethodNotify  engines_changed,
                                           ShellPlusInputMethodNotify  popup_changed,
                                           gpointer                    user_data);

/* The engine called @name, or NULL when IBus isn't running or doesn't know it. */
IBusEngineDesc *shellplus_input_method_get_engine (ShellPlusInputMethod *self,
                                                   const char           *name);

/* Makes @name the global IBus engine and sends key events through it.
 * NULL: an XKB source is active; keys go straight to the client. */
void shellplus_input_method_set_engine (ShellPlusInputMethod *self,
                                        const char           *name);

/* The text cursor of the focused client in stage coordinates; FALSE if
 * there is none. */
gboolean shellplus_input_method_get_cursor_rect (ShellPlusInputMethod *self,
                                                 MtkRectangle         *rect);

/* Disconnects from IBus and the session bus. */
void shellplus_input_method_shutdown (ShellPlusInputMethod *self);

G_END_DECLS
