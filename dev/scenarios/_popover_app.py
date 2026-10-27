import gi
gi.require_version("Gtk", "4.0"); gi.require_version("Adw", "1")
from gi.repository import Adw, Gtk
def on_activate(app):
    w = Adw.ApplicationWindow(application=app)
    view = Adw.ToolbarView(); bar = Adw.HeaderBar()
    mb = Gtk.MenuButton(icon_name="open-menu-symbolic")
    mb.set_popover(Gtk.Popover(child=Gtk.Label(label="I am a popover", margin_top=40, margin_bottom=40, margin_start=40, margin_end=40)))
    bar.pack_start(mb); view.add_top_bar(bar); view.set_content(Gtk.Label(label="app"))
    w.set_content(view); w.maximize(); w.present()
app = Adw.Application(application_id="org.test.Pop"); app.connect("activate", on_activate); app.run([])
