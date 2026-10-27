"""A plain window with a given app id and title, for the switcher
scenario. Whenever it becomes the active (focused) window it writes its
title to FOCUS_FILE.
(one line per change, appended, so several windows can share the file).

    python3 switcher-window.py APP_ID TITLE FOCUS_FILE
"""
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gtk  # noqa: E402

app_id, title, focus_file = sys.argv[1:4]


def on_active(window, _pspec):
    if window.is_active():
        with open(focus_file, "a") as f:
            f.write(title + "\n")


def on_activate(app):
    window = Adw.ApplicationWindow(application=app, title=title,
                                   default_width=560, default_height=380)
    view = Adw.ToolbarView()
    view.add_top_bar(Adw.HeaderBar())
    view.set_content(Gtk.Label(label=title, css_classes=["title-1"]))
    window.set_content(view)
    window.connect("notify::is-active", on_active)
    window.present()


app = Adw.Application(application_id=app_id)
app.connect("activate", on_activate)
app.run([])
