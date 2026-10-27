"""The whole quick settings menu and each submenu, against mocked services.

Section-specific behaviour is covered by the qs_* scenarios; this one is the
visual overview.
"""
import sys

sys.path.insert(0, "/src/dev")
from driver import Session  # noqa: E402

s = Session(sys.argv[1])
s.shot("00-panel")
s.panel("Activate", "qs-button")
s.shot("01-menu")

for arrow in ["qs-wifi-arrow", "qs-bluetooth-arrow", "qs-power-mode-arrow",
              "qs-volume-arrow", "qs-power"]:
    try:
        s.panel("Activate", arrow)
    except Exception as err:  # section not available in this build
        print("skipped", arrow, err)
        continue
    s.shot(f"02-{arrow}")
    s.panel("Activate", arrow)  # close it again

s.panel("Activate", "qs-dark-style")
s.shot("03-dark")
