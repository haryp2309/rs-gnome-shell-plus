#define G_LOG_DOMAIN "shellplus"

#include "shellplus-background.h"

#include <string.h>

#include <gdesktop-enums.h>
#include <meta/meta-backend.h>
#include <meta/meta-background.h>
#include <meta/meta-background-actor.h>
#include <meta/meta-background-content.h>
#include <meta/meta-background-group.h>
#include <meta/meta-context.h>
#include <meta/meta-monitor-manager.h>

struct _ShellPlusBackground
{
  MetaDisplay *display;
  ClutterActor *group;
  MetaMonitorManager *monitor_manager;
  GSettings *background_settings;
  GSettings *interface_settings;
  gulong monitors_changed_id;
};

/* GNOME's default wallpapers are often XML slideshows (e.g. day/night).
 * Until slideshows are supported we show the first image they reference. */
static GFile *
resolve_xml_wallpaper (GFile *xml_file)
{
  g_autofree char *contents = NULL;
  const char *p;

  if (!g_file_load_contents (xml_file, NULL, &contents, NULL, NULL, NULL))
    return NULL;

  for (p = strchr (contents, '>'); p; p = strchr (p + 1, '>'))
    {
      const char *start = p + 1;
      const char *end;

      while (g_ascii_isspace (*start))
        start++;
      if (*start != '/')
        continue;

      end = strchr (start, '<');
      if (!end)
        break;

      {
        g_autofree char *path = g_strstrip (g_strndup (start, end - start));

        if (!g_str_has_suffix (path, ".xml"))
          return g_file_new_for_path (path);
      }
    }

  return NULL;
}

static GFile *
get_wallpaper_file (ShellPlusBackground *self)
{
  gboolean dark = g_settings_get_enum (self->interface_settings, "color-scheme") ==
                  G_DESKTOP_COLOR_SCHEME_PREFER_DARK;
  g_autofree char *uri = NULL;
  g_autoptr (GFile) file = NULL;

  if (dark)
    uri = g_settings_get_string (self->background_settings, "picture-uri-dark");
  if (!uri || !*uri)
    {
      g_free (uri);
      uri = g_settings_get_string (self->background_settings, "picture-uri");
    }
  if (!uri || !*uri)
    return NULL;

  file = g_file_new_for_commandline_arg (uri);
  if (g_str_has_suffix (uri, ".xml"))
    return resolve_xml_wallpaper (file);

  return g_steal_pointer (&file);
}

static MetaBackground *
create_background (ShellPlusBackground *self)
{
  MetaBackground *background = meta_background_new (self->display);
  GDesktopBackgroundStyle style =
    g_settings_get_enum (self->background_settings, "picture-options");
  GDesktopBackgroundShading shading =
    g_settings_get_enum (self->background_settings, "color-shading-type");
  g_autofree char *primary_str =
    g_settings_get_string (self->background_settings, "primary-color");
  g_autofree char *secondary_str =
    g_settings_get_string (self->background_settings, "secondary-color");
  CoglColor primary, secondary;

  if (!cogl_color_from_string (&primary, primary_str))
    cogl_color_init_from_4f (&primary, 0.14, 0.14, 0.14, 1.0);
  if (!cogl_color_from_string (&secondary, secondary_str))
    secondary = primary;

  if (shading == G_DESKTOP_BACKGROUND_SHADING_SOLID)
    meta_background_set_color (background, &primary);
  else
    meta_background_set_gradient (background, shading, &primary, &secondary);

  if (style != G_DESKTOP_BACKGROUND_STYLE_NONE)
    {
      g_autoptr (GFile) file = get_wallpaper_file (self);

      if (file)
        meta_background_set_file (background, file, style);
    }

  return background;
}

static void
rebuild (ShellPlusBackground *self)
{
  g_autoptr (MetaBackground) background = create_background (self);
  int n_monitors = meta_display_get_n_monitors (self->display);
  int i;

  clutter_actor_destroy_all_children (self->group);

  for (i = 0; i < n_monitors; i++)
    {
      ClutterActor *actor = meta_background_actor_new (self->display, i);
      MetaBackgroundContent *content =
        META_BACKGROUND_CONTENT (clutter_actor_get_content (actor));
      MtkRectangle rect;

      meta_display_get_monitor_geometry (self->display, i, &rect);
      clutter_actor_set_position (actor, rect.x, rect.y);
      clutter_actor_set_size (actor, rect.width, rect.height);
      meta_background_content_set_background (content, background);
      clutter_actor_add_child (self->group, actor);
    }
}

static void
on_settings_changed (GSettings  *settings,
                     const char *key,
                     gpointer    user_data)
{
  ShellPlusBackground *self = user_data;

  if (settings == self->interface_settings && g_strcmp0 (key, "color-scheme") != 0)
    return;

  rebuild (self);
}

static void
on_monitors_changed (MetaMonitorManager *monitor_manager,
                     gpointer            user_data)
{
  rebuild (user_data);
}

ShellPlusBackground *
shellplus_background_new (MetaDisplay  *display,
                          ClutterActor *window_group)
{
  ShellPlusBackground *self = g_new0 (ShellPlusBackground, 1);
  MetaBackend *backend = meta_context_get_backend (meta_display_get_context (display));

  self->display = display;
  self->group = meta_background_group_new ();
  clutter_actor_insert_child_below (window_group, self->group, NULL);

  self->background_settings = g_settings_new ("org.gnome.desktop.background");
  self->interface_settings = g_settings_new ("org.gnome.desktop.interface");
  g_signal_connect (self->background_settings, "changed",
                    G_CALLBACK (on_settings_changed), self);
  g_signal_connect (self->interface_settings, "changed",
                    G_CALLBACK (on_settings_changed), self);

  self->monitor_manager = meta_backend_get_monitor_manager (backend);
  self->monitors_changed_id =
    g_signal_connect (self->monitor_manager, "monitors-changed",
                      G_CALLBACK (on_monitors_changed), self);

  rebuild (self);
  return self;
}

void
shellplus_background_free (ShellPlusBackground *self)
{
  g_clear_signal_handler (&self->monitors_changed_id, self->monitor_manager);
  g_clear_object (&self->background_settings);
  g_clear_object (&self->interface_settings);
  clutter_actor_destroy (self->group);
  g_free (self);
}
