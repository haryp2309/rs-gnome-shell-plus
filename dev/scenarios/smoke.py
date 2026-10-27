"""Translucent panel over the wallpaper, then solid with a maximized window."""
import sys

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

s = Session(sys.argv[1])
print("translucent:", s.state())
s.shot("translucent")
s.open_maximized_window()
print("solid:", s.state())
s.shot("solid")
