#include <stdlib.h>
#include <meta/meta-context.h>

#include "shellplus-plugin.h"

int
main (int    argc,
      char **argv)
{
  g_autoptr (MetaContext) context = NULL;
  g_autoptr (GError) error = NULL;

  context = meta_create_context ("GNOME Shell Plus");

  if (!meta_context_configure (context, &argc, &argv, &error))
    {
      g_printerr ("Failed to configure: %s\n", error->message);
      return EXIT_FAILURE;
    }

  meta_context_set_plugin_gtype (context, SHELLPLUS_TYPE_PLUGIN);
  /* Read keybindings from the same schemas GNOME Shell uses. */
  meta_context_set_gnome_wm_keybindings (context, "Mutter,GNOME Shell");

  if (!meta_context_setup (context, &error))
    {
      g_printerr ("Failed to setup: %s\n", error->message);
      return EXIT_FAILURE;
    }

  if (!meta_context_start (context, &error))
    {
      g_printerr ("Failed to start: %s\n", error->message);
      return EXIT_FAILURE;
    }

  meta_context_notify_ready (context);

  if (!meta_context_run_main_loop (context, &error))
    {
      g_printerr ("Exited with error: %s\n", error->message);
      meta_context_destroy (g_steal_pointer (&context));
      return EXIT_FAILURE;
    }

  meta_context_destroy (g_steal_pointer (&context));
  return EXIT_SUCCESS;
}
