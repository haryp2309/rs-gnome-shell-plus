"""Quick settings sound section: output/input sliders, device menus and
indicators, checked against pactl."""
import subprocess
import sys
import time

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

failures = []


def pactl(*args):
    return subprocess.run(["pactl", *args], capture_output=True, text=True).stdout.strip()


def check(what, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + what, detail)
    if not ok:
        failures.append(what)


def volume_percent(kind="sink", device="@DEFAULT_SINK@"):
    # "Volume: front-left: 19661 /  30% / ..." -> 30
    out = pactl(f"get-{kind}-volume", device)
    return int(out.split("/")[1].strip().rstrip("%"))


s = Session(sys.argv[1])
names = set(s.panel("ListNames")[0])
check("output slider exists", {"qs-volume", "qs-volume-arrow", "qs-volume-icon"} <= names)

s.panel("Activate", "qs-button")
s.shot("01-menu")

# Output device menu.
s.panel("Activate", "qs-volume-arrow")
s.shot("02-output-menu")

# Slider -> sink volume.
s.panel("SetValue", "qs-volume", 0.3)
check("slider sets volume", volume_percent() == 30, pactl("get-sink-volume", "@DEFAULT_SINK@"))

# Menu item -> default sink.
s.panel("Activate", "qs-sink-headphones")
check("menu switches default sink", pactl("get-default-sink") == "headphones", pactl("get-default-sink"))
s.shot("03-headphones-selected")
s.panel("SetValue", "qs-volume", 0.8)
check("slider follows new default", volume_percent(device="headphones") == 80)
s.panel("Activate", "qs-sink-speakers")
check("switch back to speakers", pactl("get-default-sink") == "speakers")
s.panel("Activate", "qs-volume-arrow")  # close the device menu

# External changes -> slider and panel icon.
pactl("set-sink-volume", "speakers", "10%")
time.sleep(0.5)
s.shot("04-external-low")
pactl("set-sink-mute", "speakers", "1")
time.sleep(0.5)
s.shot("05-external-muted")

# Icon button toggles mute.
s.panel("Activate", "qs-volume-icon")
check("icon unmutes", pactl("get-sink-mute", "speakers") == "Mute: no", pactl("get-sink-mute", "speakers"))
s.panel("Activate", "qs-volume-icon")
check("icon mutes", pactl("get-sink-mute", "speakers") == "Mute: yes")
s.panel("SetValue", "qs-volume", 0.9)
check("dragging unmutes", pactl("get-sink-mute", "speakers") == "Mute: no")

# Recording -> mic slider and privacy indicator.
s.shot("06-not-recording")
rec = subprocess.Popen(["parecord", "/dev/null"])
time.sleep(1.5)
s.shot("07-recording")
s.panel("SetValue", "qs-mic", 0.45)
check("mic slider sets source volume",
      volume_percent("source", "@DEFAULT_SOURCE@") == 45, pactl("get-source-volume", "@DEFAULT_SOURCE@"))
s.panel("Activate", "qs-mic-arrow")
s.shot("08-input-menu")
rec.terminate()
rec.wait()
time.sleep(1)
s.shot("09-recording-stopped")

# Volume above 100%.
s.gsettings("org.gnome.desktop.sound", "allow-volume-above-100-percent", "true")
pactl("set-sink-volume", "speakers", "130%")
time.sleep(0.5)
s.shot("10-overamplified")

s.panel("CloseMenus")
s.shot("11-closed")

# Sound server restart: the section hides, then reconnects by itself.
subprocess.run(["pkill", "-x", "pipewire-pulse"])
time.sleep(1)
s.shot("12-server-gone")
server = subprocess.Popen(["pipewire-pulse"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(4)
s.panel("Activate", "qs-button")
s.shot("13-reconnected")
s.panel("SetValue", "qs-volume", 0.5)
check("works after reconnect", volume_percent() == 50, pactl("get-sink-volume", "@DEFAULT_SINK@"))

print("failures:", failures or "none")
