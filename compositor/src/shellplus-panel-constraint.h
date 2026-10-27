#pragma once

#include <meta/display.h>
#include <meta/meta-external-constraint.h>

G_BEGIN_DECLS

#define SHELLPLUS_TYPE_PANEL_CONSTRAINT (shellplus_panel_constraint_get_type ())
G_DECLARE_FINAL_TYPE (ShellPlusPanelConstraint, shellplus_panel_constraint,
                      SHELLPLUS, PANEL_CONSTRAINT, GObject)

/* Pins a window to the top edge of the primary monitor, spanning its full
 * width. The window keeps whatever height the client asks for. */
MetaExternalConstraint *shellplus_panel_constraint_new (MetaDisplay *display);

G_END_DECLS
