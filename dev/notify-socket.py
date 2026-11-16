"""Stands in for systemd's notify socket (sd_notify(3)) in the dev session.

    python3 notify-socket.py <socket-path> <log-file>

Logs every datagram as "<monotonic seconds> <message>" so scenarios can
check what the compositor reported, e.g. READY=1 under gnome-session.
"""
import os
import socket
import sys
import time

path, log = sys.argv[1], sys.argv[2]
if os.path.exists(path):
    os.unlink(path)
sock = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
sock.bind(path)
with open(log, "a", buffering=1) as out:
    out.write(f"{time.monotonic():.3f} LISTENING\n")
    while True:
        data = sock.recv(4096).decode(errors="replace")
        out.write(f"{time.monotonic():.3f} {data!r}\n")
