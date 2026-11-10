#include "shellplus-panel-constraint.h"

#include <meta/window.h>

/* Gap between the bottom of the top bar and a notification banner. */
#define BANNER_MARGIN 6
/* Gap between the text cursor and the input method pop-up. */
#define CURSOR_MARGIN 4

struct _ShellPlusPanelConstraint
{
  GObject parent;

  MetaDisplay *display;
  ShellPlusRole role;
  MtkRectangle anchor;
};

static void constraint_iface_init (MetaExternalConstraintInterface *iface);

G_DEFINE_TYPE_WITH_CODE (ShellPlusPanelConstraint, shellplus_panel_constraint, G_TYPE_OBJECT,
                         G_IMPLEMENT_INTERFACE (META_TYPE_EXTERNAL_CONSTRAINT,
                                                constraint_iface_init))

static gboolean
shellplus_panel_constraint_constrain (MetaExternalConstraint     *constraint,
                                      MetaWindow                 *window,
                                      MetaExternalConstraintInfo *info)
{
  ShellPlusPanelConstraint *self = SHELLPLUS_PANEL_CONSTRAINT (constraint);
  int primary = meta_display_get_primary_monitor (self->display);
  MtkRectangle *rect = info->new_rect;
  MtkRectangle monitor, work_area;

  if (primary < 0)
    return FALSE;

  /* Below the text cursor (above it if there's no room), on its monitor. */
  if (self->role == SHELLPLUS_ROLE_IM_CANDIDATES)
    {
      MtkRectangle *anchor = &self->anchor;
      int index = meta_display_get_monitor_index_for_rect (self->display, anchor);

      meta_display_get_monitor_geometry (self->display, index >= 0 ? index : primary, &monitor);
      rect->x = anchor->x;
      rect->y = anchor->y + anchor->height + CURSOR_MARGIN;
      if (rect->y + rect->height > monitor.y + monitor.height)
        rect->y = anchor->y - CURSOR_MARGIN - rect->height;
      rect->x = CLAMP (rect->x, monitor.x, MAX (monitor.x, monitor.x + monitor.width - rect->width));
      rect->y = CLAMP (rect->y, monitor.y, MAX (monitor.y, monitor.y + monitor.height - rect->height));
      return TRUE;
    }

  meta_display_get_monitor_geometry (self->display, primary, &monitor);

  switch (self->role)
    {
    case SHELLPLUS_ROLE_TOP_BAR:
      rect->x = monitor.x;
      rect->y = monitor.y;
      rect->width = monitor.width;
      rect->height = CLAMP (rect->height, 1, monitor.height / 4);
      break;

    case SHELLPLUS_ROLE_BANNER:
      /* The work area starts below the top bar's strut. */
      meta_window_get_work_area_for_monitor (window, primary, &work_area);
      rect->width = MIN (rect->width, monitor.width);
      rect->x = monitor.x + (monitor.width - rect->width) / 2;
      rect->y = work_area.y + BANNER_MARGIN;
      break;

    case SHELLPLUS_ROLE_OSD:
      rect->width = MIN (rect->width, monitor.width);
      rect->x = monitor.x + (monitor.width - rect->width) / 2;
      rect->y = monitor.y + monitor.height - monitor.height / 8 - rect->height;
      break;

    case SHELLPLUS_ROLE_SYSTEM_DIALOG:
    case SHELLPLUS_ROLE_OVERVIEW:
    case SHELLPLUS_ROLE_SCREENSHOT:
    case SHELLPLUS_ROLE_LOCK:
      /* The panel draws the backdrop and lays out the contents itself. */
      *rect = monitor;
      break;

    case SHELLPLUS_ROLE_SWITCHER:
      rect->width = MIN (rect->width, monitor.width);
      rect->height = MIN (rect->height, monitor.height);
      rect->x = monitor.x + (monitor.width - rect->width) / 2;
      rect->y = monitor.y + (monitor.height - rect->height) / 2;
      break;

    case SHELLPLUS_ROLE_IM_CANDIDATES:
    case SHELLPLUS_ROLE_NONE:
      return FALSE;
    }

  return TRUE;
}

static void
constraint_iface_init (MetaExternalConstraintInterface *iface)
{
  iface->constrain = shellplus_panel_constraint_constrain;
}

static void
shellplus_panel_constraint_class_init (ShellPlusPanelConstraintClass *klass)
{
}

static void
shellplus_panel_constraint_init (ShellPlusPanelConstraint *self)
{
}

MetaExternalConstraint *
shellplus_panel_constraint_new (MetaDisplay   *display,
                                ShellPlusRole  role)
{
  ShellPlusPanelConstraint *self = g_object_new (SHELLPLUS_TYPE_PANEL_CONSTRAINT, NULL);

  self->display = display;
  self->role = role;
  return META_EXTERNAL_CONSTRAINT (self);
}

void
shellplus_panel_constraint_set_anchor (ShellPlusPanelConstraint *self,
                                       const MtkRectangle       *anchor)
{
  self->anchor = *anchor;
}
