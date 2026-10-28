"""Dash, app grid and app search (panel/src/app_grid/).

Installs fake apps into ~/.local/share/applications (with generated icons),
one of them NoDisplay, two in an app folder, one with a "new-window"
action, plus enough fillers for a second page. Every Exec writes a marker
file, so launches are verifiable. The widgets are hosted in the test-only
window that PanelDebug.ShowAppsTest opens (app_grid/test_host.rs), whose
`test-events` label counts the callbacks.
"""
import colorsys
import os
import shutil
import struct
import subprocess
import sys
import time
import zlib

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

s = Session(sys.argv[1])
failures = []

WORK = "/tmp/appgrid"
MARKS = f"{WORK}/marks"
APPS = os.path.expanduser("~/.local/share/applications")
ICONS = os.path.expanduser("~/.local/share/icons/hicolor/scalable/apps")
PNG_ICONS = os.path.expanduser("~/.local/share/icons/hicolor/96x96/apps")
shutil.rmtree(WORK, ignore_errors=True)
for d in (MARKS, APPS, ICONS):
    os.makedirs(d, exist_ok=True)
MARK = f"{WORK}/mark.sh"
with open(MARK, "w") as f:
    f.write('#!/bin/sh\necho "$2" >> "%s/$1"\n' % MARKS)
os.chmod(MARK, 0o755)


def check(condition, what):
    print(("ok   " if condition else "FAIL ") + what, flush=True)
    if not condition:
        failures.append(what)


def wait_for(predicate, timeout=5.0):
    end = time.time() + timeout
    while time.time() < end:
        if predicate():
            return True
        time.sleep(0.1)
    return False


def names():
    return s.panel("ListNames", settle=0)[0]


def inspect(name):
    try:
        return s.panel("Inspect", name, settle=0)[0]
    except Exception:  # noqa: BLE001 - no such widget
        return None


def text(name):
    state = inspect(name)
    return state.get("text") if state else None


def marks(app_id):
    try:
        with open(f"{MARKS}/{app_id}") as f:
            return f.read().split()
    except FileNotFoundError:
        return []


def events():
    """The test host's callback counters, as a dict."""
    out = {}
    for part in (text("test-events") or "").split():
        key, _, value = part.partition("=")
        out[key] = value
    return out


def dash_order():
    """The test host's dash items in order. The overview has its own dash
    with the same widget names; the debug interface sees the test window
    first, so stop at the first repeated name."""
    out = []
    for n in names():
        if n.startswith("dash-app-") or n == "dash-separator":
            if n in out:
                break
            out.append(n)
    return out


def set_text(name, value, settle=0.6):
    s.panel("SetText", name, value, settle=settle)


ICON_SVG = """<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1">
<stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/></linearGradient></defs>
<rect x="8" y="12" width="112" height="108" rx="24" fill="#00000030"/>
<rect x="8" y="8" width="112" height="108" rx="24" fill="url(#g)"/>
<text x="64" y="84" font-family="sans-serif" font-size="58" font-weight="bold"
 text-anchor="middle" fill="#ffffff">{glyph}</text></svg>"""


def png_icon(path, hue, size=96):
    """A rounded-square gradient PNG with a white disc."""
    def rgb(lightness):
        return [int(c * 255) for c in colorsys.hls_to_rgb(hue, lightness, 0.65)]
    radius, rows = 20, []
    for y in range(size):
        row = bytearray(b"\0")
        top, bottom = rgb(0.62), rgb(0.42)
        base = [int(t + (b - t) * y / size) for t, b in zip(top, bottom)]
        for x in range(size):
            cx = min(max(x, radius), size - 1 - radius)
            cy = min(max(y, radius), size - 1 - radius)
            inside = (x - cx) ** 2 + (y - cy) ** 2 <= radius ** 2
            disc = (x - size / 2) ** 2 + (y - size / 2) ** 2 <= (size / 5) ** 2
            row += bytes([255, 255, 255, 255] if disc else base + [255] if inside else [0, 0, 0, 0])
        rows.append(bytes(row))

    def chunk(kind, data):
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data) & 0xffffffff))
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(b"".join(rows))) + chunk(b"IEND", b""))


def icon(name, hue, glyph):
    # PNG by default: in the container, loading many SVG icons at once
    # (gdk-pixbuf → glycin, unsandboxed) occasionally crashes the panel
    # with a double free; APPGRID_SVG_ICONS=1 uses lettered SVGs instead.
    if not os.environ.get("APPGRID_SVG_ICONS"):
        os.makedirs(PNG_ICONS, exist_ok=True)
        return png_icon(f"{PNG_ICONS}/{name}.png", hue)

    def color(lightness):
        r, g, b = colorsys.hls_to_rgb(hue, lightness, 0.65)
        return "#%02x%02x%02x" % (int(r * 255), int(g * 255), int(b * 255))
    with open(f"{ICONS}/{name}.svg", "w") as f:
        f.write(ICON_SVG.format(top=color(0.62), bottom=color(0.42), glyph=glyph))


def app(app_id, name, hue, extra="", actions=None):
    icon(app_id, hue, name[0])
    lines = ["[Desktop Entry]", "Type=Application", f"Name={name}", f"Icon={app_id}",
             f"Exec={MARK} {app_id} launch"]
    if actions:
        lines.append("Actions=" + ";".join(actions) + ";")
    lines.append(extra.strip())
    for action, label in (actions or {}).items():
        lines += ["", f"[Desktop Action {action}]", f"Name={label}", f"Exec={MARK} {app_id} {action}"]
    with open(f"{APPS}/{app_id}.desktop", "w") as f:
        f.write("\n".join(lines) + "\n")


# --- fake apps ---------------------------------------------------------------

app("org.gnome.TextEditor", "Text Editor", 0.08, """
GenericName=Text Editor
Keywords=text;editor;plaintext;write;
Comment=Edit text files""", actions={"new-window": "New Window"})
app("org.gnome.Nautilus", "Files", 0.58, "Keywords=folder;manager;explore;disk;filesystem;")
app("org.gnome.Settings", "Settings", 0.0, "Keywords=preferences;settings;\nComment=Change system settings")
app("org.example.Calculator", "Calculator", 0.75,
    "Keywords=calculation;arithmetic;scientific;\nStartupWMClass=example-calculator")
app("org.example.Notes", "Notes", 0.15, "Keywords=text;memo;\nComment=Jot down notes")
app("org.example.Context", "Context Viewer", 0.45)
app("org.example.Hidden", "Hidden Helper", 0.3, "NoDisplay=true")
app("org.example.Clock", "Clock", 0.62, "Categories=Utility;")
app("org.example.Weather", "Weather", 0.52)
for n in range(1, 31):
    app(f"org.example.Filler{n:02d}", f"Filler {n:02d}", (n * 0.13) % 1.0)

subprocess.run(["gsettings", "set", "org.gnome.desktop.app-folders", "folder-children", "['TestFolder']"],
               check=True)
FOLDER = "org.gnome.desktop.app-folders.folder:/org/gnome/desktop/app-folders/folders/TestFolder/"
subprocess.run(["gsettings", "set", FOLDER, "name", "Utilities"], check=True)
subprocess.run(["gsettings", "set", FOLDER, "apps", "['org.example.Weather.desktop']"], check=True)
subprocess.run(["gsettings", "set", FOLDER, "categories", "['Utility']"], check=True)
time.sleep(3)  # GLib's desktop file index and GTK's icon theme notice changes

# --- dash -----------------------------------------------------------------------

s.panel("ShowAppsTest", settle=3)
all_names = names()
favs = [n for n in dash_order() if n.startswith("dash-app-")]
print("dash:", favs, flush=True)
check(favs == ["dash-app-org.gnome.Nautilus", "dash-app-org.gnome.TextEditor", "dash-app-org.gnome.Settings"],
      "dash shows the installed default favourites in order (no org.gnome.shell schema)")
check("dash-show-apps" in all_names, "dash has the Show Apps button")
check(not inspect("dash-dot-org.gnome.TextEditor")["visible"], "no running dot before set_running")
s.shot("01-dash-and-grid")

set_text("test-running-entry", "org.gnome.TextEditor,example-calculator,some.window.App")
all_names = names()
check("dash-app-org.example.Calculator" in all_names, "running app (by StartupWMClass) is added to the dash")
check("dash-app-some.window.App" in all_names, "running app without a desktop file is added too")
check(inspect("dash-dot-org.example.Calculator")["visible"], "running non-favourite has its dot")
check(inspect("dash-dot-org.gnome.TextEditor")["visible"], "running favourite has its dot")
check(not inspect("dash-dot-org.gnome.Nautilus")["visible"], "not-running favourite has no dot")
check("dash-separator" in all_names, "separator between favourites and running apps")
order = dash_order()
check(order.index("dash-separator") == 3 and order[4] == "dash-app-org.example.Calculator",
      "running apps come after the favourites")
s.shot("02-dash-running")

s.panel("Activate", "dash-show-apps")
check(events().get("show-apps") == "1", "Show Apps fires the callback")

s.panel("Activate", "dash-app-org.gnome.TextEditor")
check(events().get("raised") == "org.gnome.TextEditor", "clicking a running app raises it via set_activate_running")
check(marks("org.gnome.TextEditor") == [], "...instead of launching it")
check(events().get("launched") == "1", "connect_launched fires after raising")

s.panel("Activate", "dash-app-org.gnome.Nautilus")
check(wait_for(lambda: marks("org.gnome.Nautilus") == ["launch"]), "clicking a favourite launches it")

# App menu
s.panel("Activate", "dash-menu-org.gnome.TextEditor", settle=1.0)
all_names = names()
check("dash-menu-org.gnome.TextEditor-new-window" in all_names, "app menu has New Window (desktop action)")
check("dash-menu-org.gnome.TextEditor-favorite" in all_names, "app menu has the favourite item")
check(text("dash-menu-org.gnome.TextEditor-favorite-label") == "Remove from Favorites",
      "favourite item says Remove from Favorites for a favourite")
s.shot("03-dash-menu")
s.panel("Activate", "dash-menu-org.gnome.TextEditor-new-window")
check(wait_for(lambda: marks("org.gnome.TextEditor") == ["new-window"]), "New Window runs the new-window action")
s.panel("CloseMenus")

s.panel("Activate", "dash-menu-org.example.Calculator", settle=1.0)
check("dash-menu-org.example.Calculator-new-window" not in names(), "no New Window without the action")
s.panel("Activate", "dash-menu-org.example.Calculator-favorite", settle=1.0)
order = dash_order()
print("dash after add:", order, flush=True)
check(order[:4] == ["dash-app-org.gnome.Nautilus", "dash-app-org.gnome.TextEditor", "dash-app-org.gnome.Settings",
                    "dash-app-org.example.Calculator"], "Add to Favorites makes it a favourite")
keyfile = os.path.expanduser("~/.config/gnome-shell-plus/apps.ini")
check(os.path.exists(keyfile) and "org.example.Calculator.desktop" in open(keyfile).read(),
      "favourites are saved to our key file")
s.panel("Activate", "dash-menu-org.example.Calculator", settle=1.0)
s.panel("Activate", "dash-menu-org.example.Calculator-favorite", settle=1.0)
order = dash_order()
check(order.index("dash-app-org.example.Calculator") > order.index("dash-separator"),
      "Remove from Favorites puts it back with the running apps")
s.panel("CloseMenus")

# --- grid -----------------------------------------------------------------------

all_names = names()
check("grid-app-org.example.Notes" in all_names, "grid shows the apps")
check("grid-app-org.example.Hidden" not in all_names, "grid hides the NoDisplay app")
check("grid-folder-TestFolder" in all_names, "grid shows the folder")
check("grid-app-org.example.Clock" not in all_names and "grid-app-org.example.Weather" not in all_names,
      "folder apps (listed and by category) aren't on the top level")
pages = sorted(n for n in all_names if n.startswith("grid-page-") and not n.startswith("grid-page-dot"))
print("pages:", pages, flush=True)
check(len(pages) >= 2, "many apps make more than one page")
check("active" in inspect("grid-page-dot-0")["classes"], "first page dot is active")

s.panel("Activate", "grid-folder-TestFolder", settle=1.0)
all_names = names()
check("grid-folder-dialog" in all_names, "folder opens")
check(text("grid-folder-title") == "Utilities", "folder popup shows its name")
check({"grid-app-org.example.Clock", "grid-app-org.example.Weather"} <= set(all_names),
      "folder popup shows its apps")
s.shot("04-folder")
s.click(80, 400, settle=1.0)  # the dimmed backdrop
check("grid-folder-dialog" not in names(), "clicking outside closes the folder")

s.panel("Activate", "grid-page-dot-1", settle=1.2)
check("active" in inspect("grid-page-dot-1")["classes"]
      and "active" not in inspect("grid-page-dot-0")["classes"], "page dot changes the page")
s.shot("05-page-2")
set_text("test-search-entry", "a")
set_text("test-search-entry", "")  # the test host calls reset() when the query is cleared
check(wait_for(lambda: "active" in inspect("grid-page-dot-0")["classes"]), "reset() goes back to page 1")

launched = int(events().get("launched", "0"))
s.panel("Activate", "grid-app-org.example.Notes")
check(wait_for(lambda: marks("org.example.Notes") == ["launch"]), "clicking a grid icon launches the app")
check(int(events().get("launched", "0")) == launched + 1, "connect_launched fires for the grid")

# --- search ----------------------------------------------------------------------

set_text("test-search-entry", "text", settle=1.0)
results = [text(f"search-result-{n}-label") for n in range(4)]
print("search 'text':", results, flush=True)
check(results[0] == "Text Editor", "'text' finds Text Editor first")
check("Notes" in results and "Context Viewer" in results
      and results.index("Notes") < results.index("Context Viewer"),
      "keyword match (Notes) ranks above a mid-word name match (Context Viewer)")
s.shot("06-search")

set_text("test-running-entry", "")  # so activate_first launches rather than raises
set_text("test-search-entry", "arith", settle=1.0)
check(text("search-result-0-label") == "Calculator", "keyword prefix match finds Calculator")
check(inspect("search-no-results")["visible"] is False, "no 'No Results' with results")
s.panel("Activate", "test-activate-first")
check(events().get("first") == "true", "activate_first returns true")
check(wait_for(lambda: marks("org.example.Calculator") == ["launch"]), "activate_first launches the top result")

set_text("test-search-entry", "filler 07", settle=1.0)
check(text("search-result-0-label") == "Filler 07", "multi-word query matches all terms")

set_text("test-search-entry", "xyzzyqwv", settle=1.0)
state = inspect("search-no-results")
check(state and state["visible"] and state["text"] == "No Results", "'No Results' for nonsense")
check(not any(n.startswith("search-result-") for n in names()), "no result icons for nonsense")
s.panel("Activate", "test-activate-first")
check(events().get("first") == "false", "activate_first returns false without results")
s.shot("07-no-results")

print("events:", events(), flush=True)
print("FAILURES:", failures if failures else "none", flush=True)
