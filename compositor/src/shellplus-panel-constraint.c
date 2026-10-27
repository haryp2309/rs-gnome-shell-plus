#include "shellplus-panel-constraint.h"

struct _ShellPlusPanelConstraint
{
  GObject parent;

  MetaDisplay *display;
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
  MtkRectangle monitor;

  if (primary < 0)
    return FALSE;

  meta_display_get_monitor_geometry (self->display, primary, &monitor);

  info->new_rect->x = monitor.x;
  info->new_rect->y = monitor.y;
  info->new_rect->width = monitor.width;
  info->new_rect->height = CLAMP (info->new_rect->height, 1, monitor.height / 4);

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
shellplus_panel_constraint_new (MetaDisplay *display)
{
  ShellPlusPanelConstraint *self = g_object_new (SHELLPLUS_TYPE_PANEL_CONSTRAINT, NULL);

  self->display = display;
  return META_EXTERNAL_CONSTRAINT (self);
}
