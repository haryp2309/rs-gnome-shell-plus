#pragma once

#include <gio/gio.h>

G_BEGIN_DECLS

#define SHELLPLUS_DBUS_NAME "org.gnome.ShellPlus"
#define SHELLPLUS_DBUS_PATH "/org/gnome/ShellPlus"
#define SHELLPLUS_DBUS_IFACE "org.gnome.ShellPlus.Panel"

typedef struct _ShellPlusDBus ShellPlusDBus;

typedef struct
{
  /* Returns a new floating or full reference to an a{sv} dictionary. */
  GVariant * (*get_state)          (gpointer user_data);
  void       (*activate_workspace) (gpointer user_data,
                                    int      index);
  void       (*toggle_overview)    (gpointer user_data);
  void       (*log_out)            (gpointer user_data);
  void       (*set_brightness)     (gpointer user_data,
                                    double   value);
} ShellPlusDBusHandlers;

ShellPlusDBus *shellplus_dbus_new (const ShellPlusDBusHandlers *handlers,
                                   gpointer                     user_data);

void shellplus_dbus_emit_state_changed (ShellPlusDBus *self,
                                        GVariant      *state);

void shellplus_dbus_free (ShellPlusDBus *self);

G_END_DECLS
