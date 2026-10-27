"""A maximized libadwaita window, used by smoke-test.sh to exercise the
solid panel mode."""
import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gtk  # noqa: E402


def on_activate(app):
    window = Adw.ApplicationWindow(application=app, title="Maximized")
    view = Adw.ToolbarView()
    view.add_top_bar(Adw.HeaderBar())
    view.set_content(Gtk.Label(label="A maximized window"))
    window.set_content(view)
    window.maximize()
    window.present()


app = Adw.Application(application_id="org.gnome.ShellPlus.SmokeTest")
app.connect("activate", on_activate)
app.run([])
