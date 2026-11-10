#define G_LOG_DOMAIN "shellplus"

#include "shellplus-input-method.h"

#include <mtk/mtk.h>

/* See shellplus-input-method.h. Follows GNOME Shell's js/misc/inputMethod.js
 * (the ClutterInputMethod side), js/misc/ibusManager.js (daemon, engines,
 * panel service) and js/ui/ibusCandidatePopup.js (what the pop-up shows).
 *
 * The pop-up's contents go to the panel over a private interface on the
 * compositor's bus name:
 *
 *   PopupChanged (a{sv})  visible b, preedit s, preedit-cursor i,
 *                         auxiliary s, candidates a(ss) (label, text) of
 *                         the current page, selected i (on the page, -1:
 *                         none), horizontal b, page i, pages i,
 *                         cursor (iiii) in stage coordinates
 *   GetPopup () → a{sv}
 *   CandidateClicked (u index on the page)
 *   PageUp (), PageDown ()
 */

#define DBUS_PATH  "/org/gnome/ShellPlus/InputMethod"
#define DBUS_IFACE "org.gnome.ShellPlus.InputMethod"

/* Like GNOME Shell's ibusManager.js. */
#define SET_ENGINE_TIMEOUT_MS 4000
#define FALLBACK_ENGINE       "xkb:us::eng"
#define MAX_CANDIDATES        16

static const char *default_labels[MAX_CANDIDATES] = {
  "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "a", "b", "c", "d", "e", "f",
};

static const char introspection_xml[] =
  "<node>"
  "  <interface name='" DBUS_IFACE "'>"
  "    <method name='GetPopup'>"
  "      <arg type='a{sv}' name='popup' direction='out'/>"
  "    </method>"
  "    <method name='CandidateClicked'>"
  "      <arg type='u' name='index' direction='in'/>"
  "    </method>"
  "    <method name='PageUp'/>"
  "    <method name='PageDown'/>"
  "    <signal name='PopupChanged'>"
  "      <arg type='a{sv}' name='popup'/>"
  "    </signal>"
  "  </interface>"
  "</node>";

struct _ShellPlusInputMethod
{
  ClutterInputMethod parent;

  MetaDisplay *display;
  ShellPlusInputMethodNotify engines_changed;
  ShellPlusInputMethodNotify popup_changed;
  gpointer user_data;

  IBusBus *bus;
  GCancellable *cancellable;       /* everything tied to one daemon connection */
  IBusInputContext *context;
  IBusPanelService *panel_service;
  GHashTable *engines;             /* name → IBusEngineDesc */
  char *engine;                    /* wanted engine, NULL for XKB sources */

  gboolean has_focus;
  guint purpose;                   /* IBusInputPurpose */
  guint hints;                     /* IBusInputHints */
  gboolean password;
  gboolean has_surrounding;

  /* Preedit shown in the client. */
  char *preedit;
  guint preedit_pos;
  gboolean preedit_visible;
  guint preedit_mode;

  graphene_rect_t cursor;
  gboolean has_cursor;

  /* The pop-up (from the IBus panel service). */
  char *popup_preedit;
  guint popup_preedit_cursor;
  gboolean popup_preedit_visible;
  char *aux;
  gboolean aux_visible;
  IBusLookupTable *table;
  gboolean table_visible;
  gboolean popup_visible;
  guint popup_idle_id;

  GDBusConnection *connection;
  GDBusNodeInfo *node_info;
  guint registration_id;
};

G_DEFINE_FINAL_TYPE (ShellPlusInputMethod, shellplus_input_method, CLUTTER_TYPE_INPUT_METHOD)

static void connect_to_daemon (ShellPlusInputMethod *self);
static void queue_popup_update (ShellPlusInputMethod *self);

/* ------------------------------------------------------------------------ */
/* The pop-up                                                               */

static gboolean
popup_should_show (ShellPlusInputMethod *self)
{
  return self->has_focus && self->engine &&
         ((self->popup_preedit_visible && self->popup_preedit && *self->popup_preedit) ||
          (self->aux_visible && self->aux && *self->aux) ||
          (self->table_visible && self->table &&
           ibus_lookup_table_get_number_of_candidates (self->table) > 0));
}

static GVariant *
build_popup (ShellPlusInputMethod *self)
{
  GVariantBuilder builder, candidates;
  gboolean visible = popup_should_show (self);
  int selected = -1, page = 0, pages = 0;
  gboolean horizontal = FALSE;

  g_variant_builder_init (&builder, G_VARIANT_TYPE_VARDICT);
  g_variant_builder_init (&candidates, G_VARIANT_TYPE ("a(ss)"));

  if (visible && self->table_visible && self->table)
    {
      IBusLookupTable *table = self->table;
      guint n = ibus_lookup_table_get_number_of_candidates (table);
      guint page_size = MAX (ibus_lookup_table_get_page_size (table), 1);
      guint cursor = ibus_lookup_table_get_cursor_pos (table);
      guint start, end;

      page_size = MIN (page_size, MAX_CANDIDATES);
      pages = (n + page_size - 1) / page_size;
      page = cursor / page_size;
      start = page * page_size;
      end = MIN (start + page_size, n);
      for (guint i = start; i < end; i++)
        {
          IBusText *label = ibus_lookup_table_get_label (table, i - start);
          IBusText *text = ibus_lookup_table_get_candidate (table, i);
          const char *label_text = label ? ibus_text_get_text (label) : NULL;

          g_variant_builder_add (&candidates, "(ss)",
                                 label_text && *label_text ? label_text : default_labels[i - start],
                                 text ? ibus_text_get_text (text) : "");
        }
      if (ibus_lookup_table_is_cursor_visible (table))
        selected = cursor - start;
      horizontal = ibus_lookup_table_get_orientation (table) == IBUS_ORIENTATION_HORIZONTAL;
    }

  g_variant_builder_add (&builder, "{sv}", "visible", g_variant_new_boolean (visible));
  g_variant_builder_add (&builder, "{sv}", "preedit",
                         g_variant_new_string (visible && self->popup_preedit_visible &&
                                               self->popup_preedit ? self->popup_preedit : ""));
  g_variant_builder_add (&builder, "{sv}", "preedit-cursor",
                         g_variant_new_int32 (self->popup_preedit_cursor));
  g_variant_builder_add (&builder, "{sv}", "auxiliary",
                         g_variant_new_string (visible && self->aux_visible && self->aux ? self->aux : ""));
  g_variant_builder_add (&builder, "{sv}", "candidates", g_variant_builder_end (&candidates));
  g_variant_builder_add (&builder, "{sv}", "selected", g_variant_new_int32 (selected));
  g_variant_builder_add (&builder, "{sv}", "horizontal", g_variant_new_boolean (horizontal));
  g_variant_builder_add (&builder, "{sv}", "page", g_variant_new_int32 (page));
  g_variant_builder_add (&builder, "{sv}", "pages", g_variant_new_int32 (pages));
  g_variant_builder_add (&builder, "{sv}", "cursor",
                         g_variant_new ("(iiii)",
                                        (int) roundf (self->cursor.origin.x),
                                        (int) roundf (self->cursor.origin.y),
                                        (int) roundf (self->cursor.size.width),
                                        (int) roundf (self->cursor.size.height)));
  return g_variant_builder_end (&builder);
}

static gboolean
popup_update_cb (gpointer user_data)
{
  ShellPlusInputMethod *self = user_data;
  gboolean was_visible = self->popup_visible;

  self->popup_idle_id = 0;
  self->popup_visible = popup_should_show (self);

  /* Place the window before the panel resizes or maps it. */
  if (self->popup_changed && (self->popup_visible || was_visible))
    self->popup_changed (self->user_data);

  if (self->connection && self->registration_id && (self->popup_visible || was_visible))
    g_dbus_connection_emit_signal (self->connection, NULL, DBUS_PATH, DBUS_IFACE,
                                   "PopupChanged",
                                   g_variant_new ("(@a{sv})", build_popup (self)), NULL);
  return G_SOURCE_REMOVE;
}

/* Coalesces the bursts of panel service calls a key press causes. */
static void
queue_popup_update (ShellPlusInputMethod *self)
{
  if (!self->popup_idle_id)
    self->popup_idle_id = g_idle_add (popup_update_cb, self);
}

static void
clear_popup (ShellPlusInputMethod *self)
{
  g_clear_pointer (&self->popup_preedit, g_free);
  g_clear_pointer (&self->aux, g_free);
  g_clear_object (&self->table);
  self->popup_preedit_visible = self->aux_visible = self->table_visible = FALSE;
  queue_popup_update (self);
}

static void
on_update_preedit (IBusPanelService     *panel,
                   IBusText             *text,
                   guint                 cursor,
                   gboolean              visible,
                   ShellPlusInputMethod *self)
{
  g_free (self->popup_preedit);
  self->popup_preedit = g_strdup (text ? ibus_text_get_text (text) : "");
  self->popup_preedit_cursor = cursor;
  self->popup_preedit_visible = visible;
  queue_popup_update (self);
}

static void
on_update_auxiliary (IBusPanelService     *panel,
                     IBusText             *text,
                     gboolean              visible,
                     ShellPlusInputMethod *self)
{
  g_free (self->aux);
  self->aux = g_strdup (text ? ibus_text_get_text (text) : "");
  self->aux_visible = visible;
  queue_popup_update (self);
}

static void
on_update_lookup_table (IBusPanelService     *panel,
                        IBusLookupTable      *table,
                        gboolean              visible,
                        ShellPlusInputMethod *self)
{
  g_set_object (&self->table, table);
  self->table_visible = visible;
  queue_popup_update (self);
}

#define DEFINE_VISIBILITY_HANDLER(name, field, value) \
  static void \
  name (IBusPanelService *panel, ShellPlusInputMethod *self) \
  { \
    self->field = value; \
    queue_popup_update (self); \
  }

DEFINE_VISIBILITY_HANDLER (on_show_preedit, popup_preedit_visible, TRUE)
DEFINE_VISIBILITY_HANDLER (on_hide_preedit, popup_preedit_visible, FALSE)
DEFINE_VISIBILITY_HANDLER (on_show_auxiliary, aux_visible, TRUE)
DEFINE_VISIBILITY_HANDLER (on_hide_auxiliary, aux_visible, FALSE)
DEFINE_VISIBILITY_HANDLER (on_show_lookup_table, table_visible, TRUE)
DEFINE_VISIBILITY_HANDLER (on_hide_lookup_table, table_visible, FALSE)

static void
on_panel_focus_out (IBusPanelService     *panel,
                    const char           *path,
                    ShellPlusInputMethod *self)
{
  clear_popup (self);
}

/* ------------------------------------------------------------------------ */
/* The private D-Bus interface                                              */

static void
handle_method_call (GDBusConnection       *connection,
                    const char            *sender,
                    const char            *object_path,
                    const char            *interface_name,
                    const char            *method_name,
                    GVariant              *parameters,
                    GDBusMethodInvocation *invocation,
                    gpointer               user_data)
{
  ShellPlusInputMethod *self = user_data;

  if (g_strcmp0 (method_name, "GetPopup") == 0)
    {
      g_dbus_method_invocation_return_value (invocation,
                                             g_variant_new ("(@a{sv})", build_popup (self)));
      return;
    }

  if (g_strcmp0 (method_name, "CandidateClicked") == 0)
    {
      guint index;

      g_variant_get (parameters, "(u)", &index);
      /* Like GNOME Shell: the index on the page, the primary button. */
      if (self->panel_service && index < MAX_CANDIDATES)
        ibus_panel_service_candidate_clicked (self->panel_service, index, 1, 0);
    }
  else if (g_strcmp0 (method_name, "PageUp") == 0)
    {
      if (self->panel_service)
        ibus_panel_service_page_up (self->panel_service);
    }
  else if (g_strcmp0 (method_name, "PageDown") == 0)
    {
      if (self->panel_service)
        ibus_panel_service_page_down (self->panel_service);
    }
  else
    {
      g_dbus_method_invocation_return_error (invocation, G_DBUS_ERROR,
                                             G_DBUS_ERROR_UNKNOWN_METHOD,
                                             "Unknown method %s", method_name);
      return;
    }
  g_dbus_method_invocation_return_value (invocation, NULL);
}

static const GDBusInterfaceVTable interface_vtable = {
  handle_method_call,
};

/* The panel calls the compositor's well-known name; org.gnome.ShellPlus is
 * owned by the shared session bus connection this object is exported on. */
static void
export_interface (ShellPlusInputMethod *self)
{
  g_autoptr (GError) error = NULL;

  self->connection = g_bus_get_sync (G_BUS_TYPE_SESSION, NULL, &error);
  if (!self->connection)
    {
      g_warning ("No session bus for the input method pop-up: %s", error->message);
      return;
    }
  self->node_info = g_dbus_node_info_new_for_xml (introspection_xml, NULL);
  self->registration_id =
    g_dbus_connection_register_object (self->connection, DBUS_PATH,
                                       self->node_info->interfaces[0],
                                       &interface_vtable, self, NULL, &error);
  if (!self->registration_id)
    g_warning ("Failed to export %s: %s", DBUS_IFACE, error->message);
}

/* ------------------------------------------------------------------------ */
/* The IBus input context                                                   */

static void
update_capabilities (ShellPlusInputMethod *self)
{
  guint caps = IBUS_CAP_PREEDIT_TEXT | IBUS_CAP_FOCUS;

  if (self->has_surrounding)
    caps |= IBUS_CAP_SURROUNDING_TEXT;
  if (self->context)
    ibus_input_context_set_capabilities (self->context, caps);
}

static void
on_commit_text (IBusInputContext     *context,
                IBusText             *text,
                ShellPlusInputMethod *self)
{
  clutter_input_method_commit (CLUTTER_INPUT_METHOD (self), ibus_text_get_text (text));
}

static void
on_delete_surrounding_text (IBusInputContext     *context,
                            int                   offset,
                            guint                 n_chars,
                            ShellPlusInputMethod *self)
{
  if (!self->has_surrounding)
    return;
  clutter_input_method_delete_surrounding (CLUTTER_INPUT_METHOD (self), offset, n_chars);
}

static void
on_update_preedit_text (IBusInputContext     *context,
                        IBusText             *text,
                        guint                 pos,
                        gboolean              visible,
                        guint                 mode,
                        ShellPlusInputMethod *self)
{
  const char *str = text ? ibus_text_get_text (text) : NULL;

  if (str && !*str)
    str = NULL;

  /* IBus's preedit modes (clear, commit) are Clutter's. */
  if (visible)
    clutter_input_method_set_preedit_text (CLUTTER_INPUT_METHOD (self), str, pos, pos, mode);
  else if (self->preedit_visible)
    clutter_input_method_set_preedit_text (CLUTTER_INPUT_METHOD (self), NULL, pos, pos, mode);

  g_free (self->preedit);
  self->preedit = g_strdup (str);
  self->preedit_pos = pos;
  self->preedit_visible = visible;
  self->preedit_mode = mode;
}

static void
on_show_preedit_text (IBusInputContext     *context,
                      ShellPlusInputMethod *self)
{
  self->preedit_visible = TRUE;
  clutter_input_method_set_preedit_text (CLUTTER_INPUT_METHOD (self), self->preedit,
                                         self->preedit_pos, self->preedit_pos,
                                         self->preedit_mode);
}

static void
on_hide_preedit_text (IBusInputContext     *context,
                      ShellPlusInputMethod *self)
{
  self->preedit_visible = FALSE;
  clutter_input_method_set_preedit_text (CLUTTER_INPUT_METHOD (self), NULL,
                                         self->preedit_pos, self->preedit_pos,
                                         self->preedit_mode);
}

static void
on_forward_key_event (IBusInputContext     *context,
                      guint                 keyval,
                      guint                 keycode,
                      guint                 state,
                      ShellPlusInputMethod *self)
{
  gboolean press = (state & IBUS_RELEASE_MASK) == 0;

  state &= ~IBUS_RELEASE_MASK;
  clutter_input_method_forward_key (CLUTTER_INPUT_METHOD (self), keyval, keycode + 8,
                                    state & CLUTTER_MODIFIER_MASK,
                                    g_get_monotonic_time (), press);
}

static void
on_require_surrounding_text (IBusInputContext     *context,
                             ShellPlusInputMethod *self)
{
  clutter_input_method_request_surrounding (CLUTTER_INPUT_METHOD (self));
}

static void
clear_context (ShellPlusInputMethod *self)
{
  if (self->context)
    g_signal_handlers_disconnect_by_data (self->context, self);
  g_clear_object (&self->context);
  g_clear_pointer (&self->preedit, g_free);
  self->preedit_visible = FALSE;
}

static void
on_context_created (GObject      *source,
                    GAsyncResult *result,
                    gpointer      user_data)
{
  ShellPlusInputMethod *self = user_data;
  g_autoptr (GError) error = NULL;
  IBusInputContext *context;

  context = ibus_bus_create_input_context_async_finish (IBUS_BUS (source), result, &error);
  if (!context)
    {
      if (!g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
        g_warning ("Could not create an IBus input context: %s", error->message);
      return;
    }

  self->context = context;
  ibus_input_context_set_client_commit_preedit (context, TRUE);
  g_signal_connect (context, "commit-text", G_CALLBACK (on_commit_text), self);
  g_signal_connect (context, "delete-surrounding-text",
                    G_CALLBACK (on_delete_surrounding_text), self);
  g_signal_connect (context, "update-preedit-text-with-mode",
                    G_CALLBACK (on_update_preedit_text), self);
  g_signal_connect (context, "show-preedit-text", G_CALLBACK (on_show_preedit_text), self);
  g_signal_connect (context, "hide-preedit-text", G_CALLBACK (on_hide_preedit_text), self);
  g_signal_connect (context, "forward-key-event", G_CALLBACK (on_forward_key_event), self);
  if (g_signal_lookup ("require-surrounding-text", IBUS_TYPE_INPUT_CONTEXT))
    g_signal_connect (context, "require-surrounding-text",
                      G_CALLBACK (on_require_surrounding_text), self);
  update_capabilities (self);
  ibus_input_context_set_content_type (context, self->purpose, self->hints);

  if (self->has_focus)
    {
      ibus_input_context_focus_in (context);
      if (self->has_cursor)
        ibus_input_context_set_cursor_location (context,
                                                self->cursor.origin.x, self->cursor.origin.y,
                                                self->cursor.size.width, self->cursor.size.height);
    }
}

/* ------------------------------------------------------------------------ */
/* Engines and the daemon                                                   */

static void
on_engine_set (GObject      *source,
               GAsyncResult *result,
               gpointer      user_data)
{
  g_autoptr (GError) error = NULL;
  g_autofree char *engine = user_data;

  if (ibus_bus_set_global_engine_async_finish (IBUS_BUS (source), result, &error))
    {
      g_debug ("IBus engine is now %s", engine);
      return;
    }
  if (g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
    return;

  g_warning ("Could not set the IBus engine %s: %s", engine, error->message);
  if (g_strcmp0 (engine, FALLBACK_ENGINE) != 0)
    ibus_bus_set_global_engine_async (IBUS_BUS (source), FALLBACK_ENGINE,
                                      SET_ENGINE_TIMEOUT_MS, NULL, on_engine_set,
                                      g_strdup (FALLBACK_ENGINE));
}

/* Like GNOME Shell, XKB sources use IBus's plain XKB engine, so that the
 * previous engine is turned off (and its preedit dropped). */
static void
apply_engine (ShellPlusInputMethod *self)
{
  const char *engine = self->engine ? self->engine : FALLBACK_ENGINE;

  if (!self->bus || !ibus_bus_is_connected (self->bus) || !self->engines ||
      g_hash_table_size (self->engines) == 0)
    return;

  ibus_bus_set_global_engine_async (self->bus, engine, SET_ENGINE_TIMEOUT_MS,
                                    self->cancellable, on_engine_set, g_strdup (engine));
}

static void
on_engines_listed (GObject      *source,
                   GAsyncResult *result,
                   gpointer      user_data)
{
  ShellPlusInputMethod *self = user_data;
  g_autoptr (GError) error = NULL;
  GList *engines, *l;

  engines = ibus_bus_list_engines_async_finish (IBUS_BUS (source), result, &error);
  if (error)
    {
      if (!g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
        g_warning ("Could not list IBus engines: %s", error->message);
      return;
    }

  g_hash_table_remove_all (self->engines);
  for (l = engines; l; l = l->next)
    g_hash_table_insert (self->engines,
                         g_strdup (ibus_engine_desc_get_name (l->data)), l->data);
  g_list_free (engines);
  g_debug ("IBus has %u engines", g_hash_table_size (self->engines));

  if (self->engines_changed)
    self->engines_changed (self->user_data);
  apply_engine (self);
}

static void
on_panel_name_requested (GObject      *source,
                         GAsyncResult *result,
                         gpointer      user_data)
{
  ShellPlusInputMethod *self = user_data;
  g_autoptr (GError) error = NULL;
  IBusPanelService *panel;

  if (!ibus_bus_request_name_async_finish (IBUS_BUS (source), result, &error))
    {
      if (!g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
        g_warning ("Could not become the IBus panel: %s", error->message);
      return;
    }

  panel = ibus_panel_service_new (ibus_bus_get_connection (self->bus));
  self->panel_service = panel;
  g_signal_connect (panel, "update-preedit-text", G_CALLBACK (on_update_preedit), self);
  g_signal_connect (panel, "show-preedit-text", G_CALLBACK (on_show_preedit), self);
  g_signal_connect (panel, "hide-preedit-text", G_CALLBACK (on_hide_preedit), self);
  g_signal_connect (panel, "update-auxiliary-text", G_CALLBACK (on_update_auxiliary), self);
  g_signal_connect (panel, "show-auxiliary-text", G_CALLBACK (on_show_auxiliary), self);
  g_signal_connect (panel, "hide-auxiliary-text", G_CALLBACK (on_hide_auxiliary), self);
  g_signal_connect (panel, "update-lookup-table", G_CALLBACK (on_update_lookup_table), self);
  g_signal_connect (panel, "show-lookup-table", G_CALLBACK (on_show_lookup_table), self);
  g_signal_connect (panel, "hide-lookup-table", G_CALLBACK (on_hide_lookup_table), self);
  g_signal_connect (panel, "focus-out", G_CALLBACK (on_panel_focus_out), self);
}

static void
on_connected (IBusBus              *bus,
              ShellPlusInputMethod *self)
{
  g_debug ("Connected to ibus-daemon");
  g_cancellable_cancel (self->cancellable);
  g_clear_object (&self->cancellable);
  self->cancellable = g_cancellable_new ();

  ibus_bus_create_input_context_async (bus, "gnome-shell", -1, self->cancellable,
                                       on_context_created, self);
  ibus_bus_list_engines_async (bus, -1, self->cancellable, on_engines_listed, self);
  ibus_bus_request_name_async (bus, IBUS_SERVICE_PANEL, IBUS_BUS_NAME_FLAG_REPLACE_EXISTING,
                               -1, self->cancellable, on_panel_name_requested, self);
}

static void
on_disconnected (IBusBus              *bus,
                 ShellPlusInputMethod *self)
{
  g_debug ("Disconnected from ibus-daemon");
  g_cancellable_cancel (self->cancellable);
  clear_context (self);
  if (self->panel_service)
    g_signal_handlers_disconnect_by_data (self->panel_service, self);
  g_clear_object (&self->panel_service);
  clear_popup (self);

  if (self->engines && g_hash_table_size (self->engines) > 0)
    {
      g_hash_table_remove_all (self->engines);
      if (self->engines_changed)
        self->engines_changed (self->user_data);
    }
}

static void
on_daemon_exited (GObject      *source,
                  GAsyncResult *result,
                  gpointer      user_data)
{
  g_subprocess_wait_finish (G_SUBPROCESS (source), result, NULL);
  g_object_unref (source);
}

/* Like GNOME Shell when IBus's systemd unit doesn't start it. A second
 * ibus-daemon exits straight away, so this is harmless when one runs. */
static void
spawn_daemon (void)
{
  g_autoptr (GError) error = NULL;
  g_autofree char *path = g_find_program_in_path ("ibus-daemon");
  GSubprocess *daemon;

  if (!path)
    {
      g_message ("ibus-daemon isn't installed; only XKB input sources are available");
      return;
    }

  daemon = g_subprocess_new (G_SUBPROCESS_FLAGS_NONE, &error,
                             path, "--panel", "disable", NULL);
  if (!daemon)
    {
      g_warning ("Failed to launch ibus-daemon: %s", error->message);
      return;
    }
  g_subprocess_wait_async (daemon, NULL, on_daemon_exited, NULL);
}

static void
connect_to_daemon (ShellPlusInputMethod *self)
{
  ibus_init ();
  self->bus = ibus_bus_new_async ();
  g_signal_connect (self->bus, "connected", G_CALLBACK (on_connected), self);
  g_signal_connect (self->bus, "disconnected", G_CALLBACK (on_disconnected), self);
  if (ibus_bus_is_connected (self->bus))
    on_connected (self->bus, self);
}

/* ------------------------------------------------------------------------ */
/* ClutterInputMethod                                                       */

static gboolean
is_active (ShellPlusInputMethod *self)
{
  /* GNOME Shell turns IBus off for password fields, which also keeps the
   * lock screen's password entry plain. */
  return self->context && self->engine && !self->password;
}

static void
shellplus_input_method_focus_in (ClutterInputMethod *im,
                                 ClutterInputFocus  *focus)
{
  ShellPlusInputMethod *self = SHELLPLUS_INPUT_METHOD (im);

  self->has_focus = TRUE;
  if (self->context)
    {
      update_capabilities (self);
      ibus_input_context_focus_in (self->context);
    }
}

static void
shellplus_input_method_focus_out (ClutterInputMethod *im)
{
  ShellPlusInputMethod *self = SHELLPLUS_INPUT_METHOD (im);

  self->has_focus = FALSE;
  self->has_cursor = FALSE;
  if (self->context)
    {
      ibus_input_context_set_content_type (self->context, IBUS_INPUT_PURPOSE_FREE_FORM,
                                           IBUS_INPUT_HINT_NONE);
      ibus_input_context_set_cursor_location (self->context, 0, 0, 0, 0);
      ibus_input_context_reset (self->context);
      ibus_input_context_focus_out (self->context);
    }

  if (self->preedit && self->preedit_visible)
    {
      clutter_input_method_set_preedit_text (im, NULL, 0, 0, self->preedit_mode);
      g_clear_pointer (&self->preedit, g_free);
    }
  clear_popup (self);
}

static void
shellplus_input_method_reset (ClutterInputMethod *im)
{
  ShellPlusInputMethod *self = SHELLPLUS_INPUT_METHOD (im);

  if (self->context)
    ibus_input_context_reset (self->context);
  g_clear_pointer (&self->preedit, g_free);
}

static void
shellplus_input_method_set_cursor_location (ClutterInputMethod    *im,
                                            const graphene_rect_t *rect)
{
  ShellPlusInputMethod *self = SHELLPLUS_INPUT_METHOD (im);

  if (self->has_cursor && graphene_rect_equal (&self->cursor, rect))
    return;

  self->cursor = *rect;
  self->has_cursor = TRUE;
  if (self->context)
    ibus_input_context_set_cursor_location (self->context, rect->origin.x, rect->origin.y,
                                            rect->size.width, rect->size.height);
  if (self->popup_visible)
    queue_popup_update (self);
}

static void
shellplus_input_method_set_surrounding (ClutterInputMethod *im,
                                        const char         *text,
                                        guint               cursor,
                                        guint               anchor)
{
  ShellPlusInputMethod *self = SHELLPLUS_INPUT_METHOD (im);
  gboolean had_surrounding = self->has_surrounding;

  self->has_surrounding = text != NULL;
  if (!self->context || !text)
    return;
  if (!had_surrounding)
    update_capabilities (self);

  /* The context sinks the floating text. */
  ibus_input_context_set_surrounding_text (self->context, ibus_text_new_from_string (text),
                                           cursor, anchor);
}

static void
shellplus_input_method_update_content_hints (ClutterInputMethod           *im,
                                             ClutterInputContentHintFlags  hints)
{
  ShellPlusInputMethod *self = SHELLPLUS_INPUT_METHOD (im);
  guint ibus_hints = IBUS_INPUT_HINT_NONE;

  if (hints & CLUTTER_INPUT_CONTENT_HINT_COMPLETION)
    ibus_hints |= IBUS_INPUT_HINT_WORD_COMPLETION;
  if (hints & CLUTTER_INPUT_CONTENT_HINT_SPELLCHECK)
    ibus_hints |= IBUS_INPUT_HINT_SPELLCHECK;
  if (hints & CLUTTER_INPUT_CONTENT_HINT_AUTO_CAPITALIZATION)
    ibus_hints |= IBUS_INPUT_HINT_UPPERCASE_SENTENCES;
  if (hints & CLUTTER_INPUT_CONTENT_HINT_LOWERCASE)
    ibus_hints |= IBUS_INPUT_HINT_LOWERCASE;
  if (hints & CLUTTER_INPUT_CONTENT_HINT_UPPERCASE)
    ibus_hints |= IBUS_INPUT_HINT_UPPERCASE_CHARS;
  if (hints & CLUTTER_INPUT_CONTENT_HINT_TITLECASE)
    ibus_hints |= IBUS_INPUT_HINT_UPPERCASE_WORDS;
  if (hints & CLUTTER_INPUT_CONTENT_HINT_SENSITIVE_DATA)
    ibus_hints |= IBUS_INPUT_HINT_PRIVATE;
  if (hints & CLUTTER_INPUT_CONTENT_HINT_HIDDEN_TEXT)
    ibus_hints |= IBUS_INPUT_HINT_HIDDEN_TEXT;
  if (hints & CLUTTER_INPUT_CONTENT_HINT_NO_EMOJI)
    ibus_hints |= IBUS_INPUT_HINT_NO_EMOJI;

  self->hints = ibus_hints;
  if (self->context)
    ibus_input_context_set_content_type (self->context, self->purpose, self->hints);
}

static void
shellplus_input_method_update_content_purpose (ClutterInputMethod         *im,
                                               ClutterInputContentPurpose  purpose)
{
  ShellPlusInputMethod *self = SHELLPLUS_INPUT_METHOD (im);
  guint ibus_purpose;

  switch (purpose)
    {
    case CLUTTER_INPUT_CONTENT_PURPOSE_ALPHA:    ibus_purpose = IBUS_INPUT_PURPOSE_ALPHA; break;
    case CLUTTER_INPUT_CONTENT_PURPOSE_DIGITS:   ibus_purpose = IBUS_INPUT_PURPOSE_DIGITS; break;
    case CLUTTER_INPUT_CONTENT_PURPOSE_NUMBER:   ibus_purpose = IBUS_INPUT_PURPOSE_NUMBER; break;
    case CLUTTER_INPUT_CONTENT_PURPOSE_PHONE:    ibus_purpose = IBUS_INPUT_PURPOSE_PHONE; break;
    case CLUTTER_INPUT_CONTENT_PURPOSE_URL:      ibus_purpose = IBUS_INPUT_PURPOSE_URL; break;
    case CLUTTER_INPUT_CONTENT_PURPOSE_EMAIL:    ibus_purpose = IBUS_INPUT_PURPOSE_EMAIL; break;
    case CLUTTER_INPUT_CONTENT_PURPOSE_NAME:     ibus_purpose = IBUS_INPUT_PURPOSE_NAME; break;
    case CLUTTER_INPUT_CONTENT_PURPOSE_PASSWORD: ibus_purpose = IBUS_INPUT_PURPOSE_PASSWORD; break;
    case CLUTTER_INPUT_CONTENT_PURPOSE_TERMINAL: ibus_purpose = IBUS_INPUT_PURPOSE_TERMINAL; break;
    default:                                     ibus_purpose = IBUS_INPUT_PURPOSE_FREE_FORM; break;
    }

  self->purpose = ibus_purpose;
  self->password = purpose == CLUTTER_INPUT_CONTENT_PURPOSE_PASSWORD;
  if (self->context)
    {
      ibus_input_context_set_content_type (self->context, self->purpose, self->hints);
      if (self->password)
        ibus_input_context_reset (self->context);
    }
}

typedef struct
{
  ShellPlusInputMethod *self;
  IBusInputContext *context;
  ClutterEvent *event;
} KeyEventData;

static void
on_key_event_processed (GObject      *source,
                        GAsyncResult *result,
                        gpointer      user_data)
{
  KeyEventData *data = user_data;
  g_autoptr (GError) error = NULL;
  gboolean handled;

  handled = ibus_input_context_process_key_event_async_finish (IBUS_INPUT_CONTEXT (source),
                                                               result, &error);
  if (error && g_error_matches (error, G_IO_ERROR, G_IO_ERROR_CANCELLED))
    goto out;
  if (error)
    g_debug ("IBus failed to process a key: %s", error->message);

  /* The key goes to the client unless IBus took it (also on errors, so no
   * key is lost). A stale context's answer is still delivered: Clutter
   * expects one notification per filtered event. */
  clutter_input_method_notify_key_event (CLUTTER_INPUT_METHOD (data->self), data->event,
                                         handled && !error);

out:
  clutter_event_free (data->event);
  g_object_unref (data->context);
  g_object_unref (data->self);
  g_free (data);
}

static gboolean
shellplus_input_method_filter_key_event (ClutterInputMethod *im,
                                         const ClutterEvent *event)
{
  ShellPlusInputMethod *self = SHELLPLUS_INPUT_METHOD (im);
  ClutterModifierType state;
  KeyEventData *data;

  if (!is_active (self))
    return FALSE;

  state = clutter_event_get_state (event);
  if (state & IBUS_IGNORED_MASK)
    return FALSE;
  if (clutter_event_type (event) == CLUTTER_KEY_RELEASE)
    state |= IBUS_RELEASE_MASK;

  data = g_new0 (KeyEventData, 1);
  data->self = g_object_ref (self);
  data->context = g_object_ref (self->context);
  data->event = clutter_event_copy (event);
  ibus_input_context_process_key_event_async (self->context,
                                              clutter_event_get_key_symbol (event),
                                              clutter_event_get_key_code (event) - 8,
                                              state, -1, NULL,
                                              on_key_event_processed, data);
  return TRUE;
}

static void
shellplus_input_method_set_handled_actions (ClutterInputMethod      *im,
                                            ClutterInputActionFlags  actions)
{
}

static void
shellplus_input_method_dispose (GObject *object)
{
  shellplus_input_method_shutdown (SHELLPLUS_INPUT_METHOD (object));
  G_OBJECT_CLASS (shellplus_input_method_parent_class)->dispose (object);
}

static void
shellplus_input_method_class_init (ShellPlusInputMethodClass *klass)
{
  GObjectClass *object_class = G_OBJECT_CLASS (klass);
  ClutterInputMethodClass *im_class = CLUTTER_INPUT_METHOD_CLASS (klass);

  object_class->dispose = shellplus_input_method_dispose;

  im_class->focus_in = shellplus_input_method_focus_in;
  im_class->focus_out = shellplus_input_method_focus_out;
  im_class->reset = shellplus_input_method_reset;
  im_class->set_cursor_location = shellplus_input_method_set_cursor_location;
  im_class->set_surrounding = shellplus_input_method_set_surrounding;
  im_class->update_content_hints = shellplus_input_method_update_content_hints;
  im_class->update_content_purpose = shellplus_input_method_update_content_purpose;
  im_class->filter_key_event = shellplus_input_method_filter_key_event;
  im_class->set_handled_actions = shellplus_input_method_set_handled_actions;
}

static void
shellplus_input_method_init (ShellPlusInputMethod *self)
{
  self->purpose = IBUS_INPUT_PURPOSE_FREE_FORM;
  self->hints = IBUS_INPUT_HINT_NONE;
  self->engines = g_hash_table_new_full (g_str_hash, g_str_equal, g_free, g_object_unref);
  self->cancellable = g_cancellable_new ();
}

/* ------------------------------------------------------------------------ */
/* Public API                                                               */

ShellPlusInputMethod *
shellplus_input_method_new (MetaDisplay *display)
{
  ShellPlusInputMethod *self = g_object_new (SHELLPLUS_TYPE_INPUT_METHOD, NULL);

  self->display = display;
  export_interface (self);
  if (!g_getenv ("SHELLPLUS_NO_IBUS"))
    {
      spawn_daemon ();
      connect_to_daemon (self);
    }
  return self;
}

void
shellplus_input_method_set_callbacks (ShellPlusInputMethod       *self,
                                      ShellPlusInputMethodNotify  engines_changed,
                                      ShellPlusInputMethodNotify  popup_changed,
                                      gpointer                    user_data)
{
  self->engines_changed = engines_changed;
  self->popup_changed = popup_changed;
  self->user_data = user_data;
}

IBusEngineDesc *
shellplus_input_method_get_engine (ShellPlusInputMethod *self,
                                   const char           *name)
{
  return self->engines ? g_hash_table_lookup (self->engines, name) : NULL;
}

void
shellplus_input_method_set_engine (ShellPlusInputMethod *self,
                                   const char           *name)
{
  if (g_strcmp0 (self->engine, name) == 0)
    return;

  g_free (self->engine);
  self->engine = g_strdup (name);
  if (!name)
    clear_popup (self);
  apply_engine (self);
}

gboolean
shellplus_input_method_get_cursor_rect (ShellPlusInputMethod *self,
                                        MtkRectangle         *rect)
{
  if (!self->has_cursor)
    return FALSE;

  rect->x = (int) roundf (self->cursor.origin.x);
  rect->y = (int) roundf (self->cursor.origin.y);
  rect->width = (int) roundf (self->cursor.size.width);
  rect->height = (int) roundf (self->cursor.size.height);
  return TRUE;
}

void
shellplus_input_method_shutdown (ShellPlusInputMethod *self)
{
  self->engines_changed = NULL;
  self->popup_changed = NULL;
  g_clear_handle_id (&self->popup_idle_id, g_source_remove);
  if (self->cancellable)
    g_cancellable_cancel (self->cancellable);
  g_clear_object (&self->cancellable);
  clear_context (self);
  if (self->panel_service)
    g_signal_handlers_disconnect_by_data (self->panel_service, self);
  g_clear_object (&self->panel_service);
  if (self->bus)
    g_signal_handlers_disconnect_by_data (self->bus, self);
  g_clear_object (&self->bus);
  g_clear_pointer (&self->engines, g_hash_table_unref);
  g_clear_object (&self->table);
  g_clear_pointer (&self->popup_preedit, g_free);
  g_clear_pointer (&self->aux, g_free);
  g_clear_pointer (&self->engine, g_free);
  if (self->registration_id)
    g_dbus_connection_unregister_object (self->connection, self->registration_id);
  self->registration_id = 0;
  g_clear_object (&self->connection);
  g_clear_pointer (&self->node_info, g_dbus_node_info_unref);
}
