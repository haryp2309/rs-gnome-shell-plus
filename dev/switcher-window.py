"""A plain window with a given app id and title, for the switcher
scenario. Whenever it becomes the active (focused) window it writes its
title to FOCUS_FILE.
(one line per change, appended, so several windows can share the file).

    python3 switcher-window.py APP_ID TITLE FOCUS_FILE [COLOR [COLOR2]]

COLOR fills the window's content (for checking previews by color). Running
it again with the same APP_ID opens another window in the first process,
titled "TITLE 2" (and so on) and filled with COLOR2 if given.
"""
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gdk, Gtk  # noqa: E402

app_id, title, focus_file = sys.argv[1:4]
colors = sys.argv[4:6]
count = [0]


def on_active(window, _pspec):
    if window.is_active():
        with open(focus_file, "a") as f:
            f.write(window.get_title() + "\n")


def on_activate(app):
    count[0] += 1
    n = count[0]
    name = title if n == 1 else f"{title} {n}"
    window = Adw.ApplicationWindow(application=app, title=name,
                                   default_width=560, default_height=380)
    view = Adw.ToolbarView()
    view.add_top_bar(Adw.HeaderBar())
    label = Gtk.Label(label=name, css_classes=["title-1"], vexpand=True)
    view.set_content(label)
    color = colors[min(n, len(colors)) - 1] if colors else None
    if color:
        css = Gtk.CssProvider()
        css.load_from_string(f".fill-{n} {{ background: {color}; color: black; }}")
        Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), css, 800)
        label.add_css_class(f"fill-{n}")
    window.set_content(view)
    window.connect("notify::is-active", on_active)
    window.present()


app = Adw.Application(application_id=app_id)
app.connect("activate", on_activate)
app.run([])
