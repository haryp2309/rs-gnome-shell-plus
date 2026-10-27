#pragma once

#include <meta/display.h>

G_BEGIN_DECLS

typedef struct _ShellPlusInputSources ShellPlusInputSources;

typedef void (*ShellPlusInputSourcesChanged) (gpointer user_data);

ShellPlusInputSources *shellplus_input_sources_new (MetaDisplay                  *display,
                                                    ShellPlusInputSourcesChanged  changed,
                                                    gpointer                      user_data);

/* a(sss): id, short name, display name of each usable source, in order. */
GVariant *shellplus_input_sources_get_sources (ShellPlusInputSources *self);

/* Index into get_sources(), or -1 when there are none. */
int shellplus_input_sources_get_current (ShellPlusInputSources *self);

void shellplus_input_sources_activate (ShellPlusInputSources *self,
                                       int                    index);

void shellplus_input_sources_free (ShellPlusInputSources *self);

G_END_DECLS
