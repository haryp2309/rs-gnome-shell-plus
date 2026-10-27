#pragma once

#include <meta/meta-plugin.h>

G_BEGIN_DECLS

#define SHELLPLUS_TYPE_PLUGIN (shellplus_plugin_get_type ())
G_DECLARE_FINAL_TYPE (ShellPlusPlugin, shellplus_plugin, SHELLPLUS, PLUGIN, MetaPlugin)

G_END_DECLS
