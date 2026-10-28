#pragma once

#include <meta/display.h>
#include <meta/meta-external-constraint.h>

G_BEGIN_DECLS

/* The panel process's windows, told apart by their titles. */
typedef enum
{
  SHELLPLUS_ROLE_NONE,
  SHELLPLUS_ROLE_TOP_BAR,  /* full width, top of the primary monitor */
  SHELLPLUS_ROLE_BANNER,   /* notification banners: centered below the bar */
  SHELLPLUS_ROLE_OSD,      /* volume/brightness pop-ups: centered, near the bottom */
  SHELLPLUS_ROLE_SYSTEM_DIALOG, /* modal prompts: cover the whole primary monitor */
  SHELLPLUS_ROLE_SWITCHER, /* Alt-Tab pop-up: centered */
  SHELLPLUS_ROLE_OVERVIEW, /* covers the primary monitor, under the top bar */
} ShellPlusRole;

#define SHELLPLUS_TYPE_PANEL_CONSTRAINT (shellplus_panel_constraint_get_type ())
G_DECLARE_FINAL_TYPE (ShellPlusPanelConstraint, shellplus_panel_constraint,
                      SHELLPLUS, PANEL_CONSTRAINT, GObject)

/* Places a panel-owned window on the primary monitor according to its role.
 * The window keeps whatever size the client asks for (the top bar is
 * stretched to the monitor's width). */
MetaExternalConstraint *shellplus_panel_constraint_new (MetaDisplay   *display,
                                                        ShellPlusRole  role);

G_END_DECLS
