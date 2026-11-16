#define G_LOG_DOMAIN "shellplus"

#include "shellplus-session.h"

#include <errno.h>
#include <gio/gio.h>
#include <stddef.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#define SESSION_MANAGER_NAME  "org.gnome.SessionManager"
#define SESSION_MANAGER_PATH  "/org/gnome/SessionManager"
#define SESSION_MANAGER_IFACE "org.gnome.SessionManager"
/* GSM_MANAGER_LOGOUT_MODE_NO_CONFIRMATION */
#define LOGOUT_MODE_NO_CONFIRMATION 1

static char *notify_socket;
static gboolean notified;

void
shellplus_session_init (void)
{
  const char *socket_path = g_getenv ("NOTIFY_SOCKET");

  if (socket_path && *socket_path)
    notify_socket = g_strdup (socket_path);
  g_unsetenv ("NOTIFY_SOCKET");
}

/* The sd_notify(3) datagram protocol: one AF_UNIX datagram to the path in
 * NOTIFY_SOCKET ('@' for the abstract namespace). Saves linking libsystemd. */
static gboolean
send_notify (const char *path,
             const char *message)
{
  struct sockaddr_un addr = { .sun_family = AF_UNIX };
  size_t path_len = strlen (path);
  socklen_t addr_len;
  ssize_t sent;
  int fd;

  if ((path[0] != '/' && path[0] != '@') || path_len >= sizeof addr.sun_path)
    {
      g_warning ("Unsupported NOTIFY_SOCKET '%s'", path);
      return FALSE;
    }

  memcpy (addr.sun_path, path, path_len);
  if (path[0] == '@')
    addr.sun_path[0] = '\0';
  addr_len = offsetof (struct sockaddr_un, sun_path) + path_len;
  if (path[0] == '/')
    addr_len++;  /* include the terminating NUL */

  fd = socket (AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
  if (fd < 0)
    {
      g_warning ("Could not create notify socket: %s", g_strerror (errno));
      return FALSE;
    }
  sent = sendto (fd, message, strlen (message), MSG_NOSIGNAL,
                 (struct sockaddr *) &addr, addr_len);
  if (sent < 0)
    g_warning ("Could not notify systemd: %s", g_strerror (errno));
  close (fd);
  return sent >= 0;
}

void
shellplus_session_notify_ready (void)
{
  if (notified)
    return;
  notified = TRUE;

  if (!notify_socket)
    return;

  if (send_notify (notify_socket, "READY=1\nSTATUS=Running"))
    g_debug ("Notified systemd that the session is ready");
}

static void
on_logout_reply (GObject      *source,
                 GAsyncResult *result,
                 gpointer      user_data)
{
  g_autoptr (MetaContext) context = user_data;
  g_autoptr (GVariant) reply = NULL;
  g_autoptr (GError) error = NULL;

  reply = g_dbus_connection_call_finish (G_DBUS_CONNECTION (source), result, &error);
  if (reply)
    return;

  g_warning ("gnome-session refused to log out (%s); quitting instead", error->message);
  meta_context_terminate (context);
}

static gboolean
has_session_manager (GDBusConnection *bus)
{
  g_autoptr (GVariant) reply = NULL;
  gboolean has_owner = FALSE;

  reply = g_dbus_connection_call_sync (bus, "org.freedesktop.DBus", "/org/freedesktop/DBus",
                                       "org.freedesktop.DBus", "NameHasOwner",
                                       g_variant_new ("(s)", SESSION_MANAGER_NAME),
                                       G_VARIANT_TYPE ("(b)"), G_DBUS_CALL_FLAGS_NONE,
                                       1000, NULL, NULL);
  if (reply)
    g_variant_get (reply, "(b)", &has_owner);
  return has_owner;
}

void
shellplus_session_log_out (MetaContext *context)
{
  g_autoptr (GDBusConnection) bus = g_bus_get_sync (G_BUS_TYPE_SESSION, NULL, NULL);

  if (!bus || !has_session_manager (bus))
    {
      meta_context_terminate (context);
      return;
    }

  g_debug ("Logging out through gnome-session");
  g_dbus_connection_call (bus, SESSION_MANAGER_NAME, SESSION_MANAGER_PATH,
                          SESSION_MANAGER_IFACE, "Logout",
                          g_variant_new ("(u)", LOGOUT_MODE_NO_CONFIRMATION),
                          NULL, G_DBUS_CALL_FLAGS_NONE, -1, NULL,
                          on_logout_reply, g_object_ref (context));
}
