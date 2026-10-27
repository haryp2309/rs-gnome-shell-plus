#!/usr/bin/python3
"""Stand-in for polkit's polkit-agent-helper-1 in the test session (the
panel runs it because dev/session.sh sets SHELLPLUS_POLKIT_HELPER).

Speaks the helper's protocol as of polkit 127:

    polkit-agent-helper-1 <user>          setuid mode: cookie on stdin
    polkit-agent-helper-1 --socket-activated
                                          socket mode: user, then cookie, on stdin

then a PAM-like conversation on stdout/stdin: it asks for a password
(`PAM_PROMPT_ECHO_OFF Password: `), and accepts "hunter2", telling the
(mock) polkitd with AuthenticationAgentResponse2 before printing SUCCESS.
"locked" gets an escaped multi-line PAM_ERROR_MSG before FAILURE; anything
else just FAILURE. Every run is logged as a JSON line to
$XDG_RUNTIME_DIR/fake-polkit-helper.log for the scenario to check.
"""
import json
import os
import pwd
import signal
import sys
import time

PASSWORD = "hunter2"


def escape(text):
    """Like g_strescape(), which the real helper applies to PAM messages."""
    out = []
    for byte in text.encode():
        char = chr(byte)
        if char in "\\\"":
            out.append("\\" + char)
        elif char == "\n":
            out.append("\\n")
        elif char == "\t":
            out.append("\\t")
        elif byte < 0x20 or byte >= 0x7f:
            out.append("\\%03o" % byte)
        else:
            out.append(char)
    return "".join(out)


def say(line):
    sys.stdout.write(line + "\n")
    sys.stdout.flush()


def log(entry):
    path = os.path.join(os.environ.get("XDG_RUNTIME_DIR", "/tmp"), "fake-polkit-helper.log")
    with open(path, "a") as f:
        f.write(json.dumps(entry) + "\n")


def main():
    args = sys.argv[1:]
    if args == ["--socket-activated"]:
        mode = "socket"
        user = sys.stdin.readline().rstrip("\n")
    elif len(args) == 1:
        mode = "setuid"
        user = args[0]
    else:
        print("polkit-agent-helper-1: wrong number of arguments", file=sys.stderr)
        return 1
    entry = {"mode": mode, "argv": args, "user": user, "cookie": None, "result": None}

    def terminated(*_):
        entry["result"] = "terminated"
        sys.exit(0)

    signal.signal(signal.SIGTERM, terminated)
    cookie = entry["cookie"] = sys.stdin.readline().rstrip("\n")
    try:
        try:
            target = pwd.getpwnam(user)
        except KeyError:
            entry["result"] = "no such user"
            say("FAILURE")
            return 0

        say("PAM_PROMPT_ECHO_OFF " + escape("Password: "))
        answer = sys.stdin.readline()
        if not answer:
            entry["result"] = "eof"
            return 1
        answer = answer.rstrip("\n")
        entry["answer"] = answer

        if answer == PASSWORD:
            import dbus

            authority = dbus.Interface(
                dbus.SystemBus().get_object("org.freedesktop.PolicyKit1", "/org/freedesktop/PolicyKit1/Authority"),
                "org.freedesktop.PolicyKit1.Authority")
            identity = dbus.Struct(("unix-user", dbus.Dictionary({"uid": dbus.UInt32(target.pw_uid)},
                                                                  signature="sv")), signature="sa{sv}")
            authority.AuthenticationAgentResponse2(dbus.UInt32(os.getuid()), cookie, identity)
            entry["result"] = "success"
            say("SUCCESS")
        else:
            time.sleep(0.3)  # pam_unix's failure delay, shortened
            if answer == "locked":
                say("PAM_ERROR_MSG " + escape("Your account is locked.\nAsk an administrator."))
            entry["result"] = "failure"
            say("FAILURE")
        return 0
    finally:
        log(entry)


if __name__ == "__main__":
    sys.exit(main())
