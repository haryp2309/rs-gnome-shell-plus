#define G_LOG_DOMAIN "shellplus"

#include "shellplus-input-sources.h"

#include <meta/meta-backend.h>
#include <meta/meta-context.h>
#include <xkbcommon/xkbregistry.h>

/* Keyboard layouts, the way GNOME Shell does them: the sources come from
 * org.gnome.desktop.input-sources, the most recently used one is first in
 * mru-sources and is the active one, and Super+Space (the
 * switch-input-source keybinding) switches to the previously used source.
 * Only XKB sources are supported; IBus input methods are skipped. */

/* XKB keymaps hold at most 4 layout groups. */
#define MAX_GROUPS 4

typedef struct
{
  char *id;            /* "de+nodeadkeys" */
  char *layout;        /* "de" */
  char *variant;       /* "nodeadkeys" or "" */
  char *short_name;    /* "de" */
  char *display_name;  /* "German (no dead keys)" */
} Source;

struct _ShellPlusInputSources
{
  MetaDisplay *display;
  MetaBackend *backend;
  ShellPlusInputSourcesChanged changed;
  gpointer user_data;

  GSettings *settings;
  GSettings *keybindings;
  GHashTable *briefs;        /* xkb id → short name */
  GHashTable *descriptions;  /* xkb id → display name */
  GCancellable *cancellable;

  GPtrArray *sources;        /* Source */
  int current;
};

static void
source_free (Source *source)
{
  g_free (source->id);
  g_free (source->layout);
  g_free (source->variant);
  g_free (source->short_name);
  g_free (source->display_name);
  g_free (source);
}

static void
load_registry (ShellPlusInputSources *self)
{
  struct rxkb_context *ctx = rxkb_context_new (RXKB_CONTEXT_NO_FLAGS);
  struct rxkb_layout *layout;

  self->briefs = g_hash_table_new_full (g_str_hash, g_str_equal, g_free, g_free);
  self->descriptions = g_hash_table_new_full (g_str_hash, g_str_equal, g_free, g_free);

  if (!ctx || !rxkb_context_parse_default_ruleset (ctx))
    {
      g_warning ("Could not read the XKB layout registry; showing layout ids");
      if (ctx)
        rxkb_context_unref (ctx);
      return;
    }

  for (layout = rxkb_layout_first (ctx); layout; layout = rxkb_layout_next (layout))
    {
      const char *variant = rxkb_layout_get_variant (layout);
      const char *brief = rxkb_layout_get_brief (layout);
      const char *description = rxkb_layout_get_description (layout);
      char *id = variant && *variant
        ? g_strdup_printf ("%s+%s", rxkb_layout_get_name (layout), variant)
        : g_strdup (rxkb_layout_get_name (layout));

      if (brief)
        g_hash_table_insert (self->briefs, g_strdup (id), g_strdup (brief));
      if (description)
        g_hash_table_insert (self->descriptions, g_strdup (id), g_strdup (description));
      g_free (id);
    }
  rxkb_context_unref (ctx);
}

static Source *
source_new (ShellPlusInputSources *self,
            const char            *id)
{
  Source *source = g_new0 (Source, 1);
  g_auto (GStrv) parts = g_strsplit (id, "+", 2);
  const char *brief;
  const char *description;

  source->id = g_strdup (id);
  source->layout = g_strdup (parts[0]);
  source->variant = g_strdup (parts[1] ? parts[1] : "");

  /* Variants usually share their layout's short name. */
  brief = g_hash_table_lookup (self->briefs, id);
  if (!brief)
    brief = g_hash_table_lookup (self->briefs, source->layout);
  source->short_name = g_strdup (brief ? brief : source->layout);

  description = g_hash_table_lookup (self->descriptions, id);
  source->display_name = g_strdup (description ? description : id);
  return source;
}

/* Like GNOME Shell, number sources that share a short name: en₁, en₂. */
static void
disambiguate_short_names (GPtrArray *sources)
{
  static const char *subscripts[] = { "₁", "₂", "₃", "₄", "₅", "₆", "₇", "₈", "₉" };
  g_autoptr (GHashTable) counts = g_hash_table_new (g_str_hash, g_str_equal);
  g_autoptr (GHashTable) seen = g_hash_table_new (g_str_hash, g_str_equal);
  g_autoptr (GPtrArray) originals = g_ptr_array_new_with_free_func (g_free);

  for (guint i = 0; i < sources->len; i++)
    {
      Source *source = g_ptr_array_index (sources, i);
      char *key = g_strdup (source->short_name);

      g_ptr_array_add (originals, key);
      g_hash_table_insert (counts, key,
                           GINT_TO_POINTER (GPOINTER_TO_INT (g_hash_table_lookup (counts, key)) + 1));
    }

  for (guint i = 0; i < sources->len; i++)
    {
      Source *source = g_ptr_array_index (sources, i);
      const char *key = g_ptr_array_index (originals, i);
      int n;

      if (GPOINTER_TO_INT (g_hash_table_lookup (counts, key)) < 2)
        continue;

      n = GPOINTER_TO_INT (g_hash_table_lookup (seen, key));
      g_hash_table_insert (seen, (gpointer) key, GINT_TO_POINTER (n + 1));
      if (n < (int) G_N_ELEMENTS (subscripts))
        {
          char *numbered = g_strconcat (key, subscripts[n], NULL);

          g_free (source->short_name);
          source->short_name = numbered;
        }
    }
}

static int
find_source (ShellPlusInputSources *self,
             const char            *type,
             const char            *id)
{
  if (g_strcmp0 (type, "xkb") != 0)
    return -1;

  for (guint i = 0; i < self->sources->len; i++)
    {
      Source *source = g_ptr_array_index (self->sources, i);

      if (g_strcmp0 (source->id, id) == 0)
        return i;
    }
  return -1;
}

/* Source indices, most recently used first; never-used ones last in order. */
static GArray *
mru_order (ShellPlusInputSources *self)
{
  g_autoptr (GVariant) mru = g_settings_get_value (self->settings, "mru-sources");
  g_autofree gboolean *added = g_new0 (gboolean, self->sources->len);
  GArray *order = g_array_new (FALSE, FALSE, sizeof (int));
  GVariantIter iter;
  const char *type, *id;

  g_variant_iter_init (&iter, mru);
  while (g_variant_iter_next (&iter, "(&s&s)", &type, &id))
    {
      int index = find_source (self, type, id);

      if (index >= 0 && !added[index])
        {
          added[index] = TRUE;
          g_array_append_val (order, index);
        }
    }
  for (int i = 0; i < (int) self->sources->len; i++)
    {
      if (!added[i])
        g_array_append_val (order, i);
    }
  return order;
}

static void
on_keymap_set (GObject      *source_object,
               GAsyncResult *result,
               gpointer      user_data)
{
  g_autoptr (GError) error = NULL;

  if (!meta_backend_set_keymap_finish (META_BACKEND (source_object), result, &error) &&
      !g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
    g_warning ("Failed to set the keyboard layout: %s", error->message);
}

static void
apply_keymap (ShellPlusInputSources *self)
{
  g_autoptr (MetaKeymapDescription) description = NULL;
  g_autoptr (GStrvBuilder) layouts = g_strv_builder_new ();
  g_autoptr (GStrvBuilder) variants = g_strv_builder_new ();
  g_autoptr (GStrvBuilder) display_names = g_strv_builder_new ();
  g_autoptr (GStrvBuilder) short_names = g_strv_builder_new ();
  g_auto (GStrv) layout_list = NULL, variant_list = NULL, options_list = NULL;
  g_auto (GStrv) display_list = NULL, short_list = NULL;
  g_autofree char *layout = NULL, *variant = NULL, *options = NULL, *model = NULL;
  /* With more than 4 sources, load the group of 4 holding the current one. */
  int first = (self->current / MAX_GROUPS) * MAX_GROUPS;

  for (int i = first; i < (int) self->sources->len && i < first + MAX_GROUPS; i++)
    {
      Source *source = g_ptr_array_index (self->sources, i);

      g_strv_builder_add (layouts, source->layout);
      g_strv_builder_add (variants, source->variant);
      g_strv_builder_add (display_names, source->display_name);
      g_strv_builder_add (short_names, source->short_name);
    }

  layout_list = g_strv_builder_end (layouts);
  variant_list = g_strv_builder_end (variants);
  display_list = g_strv_builder_end (display_names);
  short_list = g_strv_builder_end (short_names);
  options_list = g_settings_get_strv (self->settings, "xkb-options");
  layout = g_strjoinv (",", layout_list);
  variant = g_strjoinv (",", variant_list);
  options = g_strjoinv (",", options_list);
  model = g_settings_get_string (self->settings, "xkb-model");

  description = meta_keymap_description_new_from_rules (*model ? model : NULL,
                                                        layout, variant, options,
                                                        display_list, short_list);
  meta_backend_set_keymap_async (self->backend, description, self->current - first,
                                 self->cancellable, on_keymap_set, NULL);
}

static void
notify_changed (ShellPlusInputSources *self)
{
  if (self->changed)
    self->changed (self->user_data);
}

static void
reload (ShellPlusInputSources *self)
{
  g_autoptr (GVariant) configured = g_settings_get_value (self->settings, "sources");
  g_autoptr (GArray) order = NULL;
  GVariantIter iter;
  const char *type, *id;

  g_ptr_array_set_size (self->sources, 0);
  g_variant_iter_init (&iter, configured);
  while (g_variant_iter_next (&iter, "(&s&s)", &type, &id))
    {
      if (g_strcmp0 (type, "xkb") == 0)
        g_ptr_array_add (self->sources, source_new (self, id));
      else
        g_debug ("Skipping unsupported %s input source %s", type, id);
    }
  if (self->sources->len == 0)
    g_ptr_array_add (self->sources, source_new (self, "us"));
  disambiguate_short_names (self->sources);

  order = mru_order (self);
  self->current = g_array_index (order, int, 0);
  apply_keymap (self);
  notify_changed (self);
}

static void
on_mru_changed (ShellPlusInputSources *self)
{
  g_autoptr (GArray) order = mru_order (self);
  int current = g_array_index (order, int, 0);

  if (current == self->current)
    return;

  self->current = current;
  apply_keymap (self);
  notify_changed (self);
}

void
shellplus_input_sources_activate (ShellPlusInputSources *self,
                                  int                    index)
{
  g_autoptr (GVariant) mru = NULL;
  GVariantBuilder builder;
  GVariantIter iter;
  const char *type, *id;
  Source *source;

  if (index < 0 || index >= (int) self->sources->len)
    return;

  /* Move the source to the front of mru-sources, keeping everything else
   * (including sources we don't support) in order. on_mru_changed() then
   * applies it. */
  source = g_ptr_array_index (self->sources, index);
  mru = g_settings_get_value (self->settings, "mru-sources");
  g_variant_builder_init (&builder, G_VARIANT_TYPE ("a(ss)"));
  g_variant_builder_add (&builder, "(ss)", "xkb", source->id);
  g_variant_iter_init (&iter, mru);
  while (g_variant_iter_next (&iter, "(&s&s)", &type, &id))
    {
      if (g_strcmp0 (type, "xkb") != 0 || g_strcmp0 (id, source->id) != 0)
        g_variant_builder_add (&builder, "(ss)", type, id);
    }
  g_settings_set_value (self->settings, "mru-sources", g_variant_builder_end (&builder));
}

static void
switch_source (ShellPlusInputSources *self,
               gboolean               backward)
{
  g_autoptr (GArray) order = mru_order (self);

  if (order->len < 2)
    return;

  /* A tap of Super+Space goes back to the previously used source, like
   * GNOME Shell's switcher without holding Super. */
  shellplus_input_sources_activate (self,
                                    g_array_index (order, int,
                                                   backward ? order->len - 1 : 1));
}

static void
on_switch_forward (MetaDisplay        *display,
                   MetaWindow         *window,
                   const ClutterEvent *event,
                   MetaKeyBinding     *binding,
                   gpointer            user_data)
{
  switch_source (user_data, FALSE);
}

static void
on_switch_backward (MetaDisplay        *display,
                    MetaWindow         *window,
                    const ClutterEvent *event,
                    MetaKeyBinding     *binding,
                    gpointer            user_data)
{
  switch_source (user_data, TRUE);
}

ShellPlusInputSources *
shellplus_input_sources_new (MetaDisplay                  *display,
                             ShellPlusInputSourcesChanged  changed,
                             gpointer                      user_data)
{
  ShellPlusInputSources *self = g_new0 (ShellPlusInputSources, 1);

  self->display = display;
  self->backend = meta_context_get_backend (meta_display_get_context (display));
  self->settings = g_settings_new ("org.gnome.desktop.input-sources");
  self->keybindings = g_settings_new ("org.gnome.desktop.wm.keybindings");
  self->cancellable = g_cancellable_new ();
  self->sources = g_ptr_array_new_with_free_func ((GDestroyNotify) source_free);
  load_registry (self);

  g_signal_connect_swapped (self->settings, "changed::sources",
                            G_CALLBACK (reload), self);
  g_signal_connect_swapped (self->settings, "changed::xkb-options",
                            G_CALLBACK (apply_keymap), self);
  g_signal_connect_swapped (self->settings, "changed::xkb-model",
                            G_CALLBACK (apply_keymap), self);
  g_signal_connect_swapped (self->settings, "changed::mru-sources",
                            G_CALLBACK (on_mru_changed), self);

  meta_display_add_keybinding (display, "switch-input-source", self->keybindings,
                               META_KEY_BINDING_NONE, on_switch_forward, self, NULL);
  meta_display_add_keybinding (display, "switch-input-source-backward", self->keybindings,
                               META_KEY_BINDING_IS_REVERSED, on_switch_backward, self, NULL);

  /* Don't notify from the constructor; the caller isn't set up yet. */
  reload (self);
  self->changed = changed;
  self->user_data = user_data;
  return self;
}

GVariant *
shellplus_input_sources_get_sources (ShellPlusInputSources *self)
{
  GVariantBuilder builder;

  g_variant_builder_init (&builder, G_VARIANT_TYPE ("a(sss)"));
  for (guint i = 0; i < self->sources->len; i++)
    {
      Source *source = g_ptr_array_index (self->sources, i);

      g_variant_builder_add (&builder, "(sss)", source->id,
                             source->short_name, source->display_name);
    }
  return g_variant_builder_end (&builder);
}

int
shellplus_input_sources_get_current (ShellPlusInputSources *self)
{
  return self->sources->len ? self->current : -1;
}

void
shellplus_input_sources_free (ShellPlusInputSources *self)
{
  meta_display_remove_keybinding (self->display, "switch-input-source");
  meta_display_remove_keybinding (self->display, "switch-input-source-backward");
  g_cancellable_cancel (self->cancellable);
  g_clear_object (&self->cancellable);
  g_signal_handlers_disconnect_by_data (self->settings, self);
  g_clear_object (&self->settings);
  g_clear_object (&self->keybindings);
  g_clear_pointer (&self->briefs, g_hash_table_unref);
  g_clear_pointer (&self->descriptions, g_hash_table_unref);
  g_clear_pointer (&self->sources, g_ptr_array_unref);
  g_free (self);
}
