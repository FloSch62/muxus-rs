"""A headless GNOME Shell that end-to-end scenarios drive over a Unix socket.

    python3 -I hack/e2e/desktop.py serve --socket S --size 1420x880 --display mx-e2e

`serve` runs inside the private D-Bus session and systemd scope that
hack/e2e/run.py sets up (never on a real desktop session). It starts
`gnome-shell --headless`, then answers one JSON request per line on the
socket:

    {"op": "launch", "cmd": [...], "env": {...}, "log": "path"} -> {"pid": N}
    {"op": "alive", "pid": N}                -> {"alive": bool}
    {"op": "signal", "pid": N, "sig": 15}
    {"op": "click"|"rclick"|"dclick"|"move", "x": X, "y": Y}   (logical px)
    {"op": "drag", "x": X, "y": Y, "x2": X2, "y2": Y2}
    {"op": "scroll", "x": X, "y": Y, "dy": N}                  (wheel steps)
    {"op": "key", "keys": "ctrl+shift+t"}   (X keysym names, one chord)
    {"op": "type", "text": "..."}
    {"op": "shot", "path": "out.png"}
    {"op": "windows"}                        -> {"windows": [{title, focus, w, h, app}]}
    {"op": "setting", "schema": s, "key": k, "value": v}
    {"op": "quit"}

The client half (`Desktop`) is what scenarios import.
"""

import argparse
import json
import os
import signal
import socket
import subprocess
import sys
import threading
import time

KEYSYMS = {
    "return": 0xFF0D, "enter": 0xFF0D, "escape": 0xFF1B, "esc": 0xFF1B, "tab": 0xFF09,
    "backspace": 0xFF08, "delete": 0xFFFF, "space": 0x20, "home": 0xFF50, "end": 0xFF57,
    "left": 0xFF51, "up": 0xFF52, "right": 0xFF53, "down": 0xFF54, "pageup": 0xFF55,
    "pagedown": 0xFF56, "insert": 0xFF63, "ctrl": 0xFFE3, "control": 0xFFE3, "shift": 0xFFE1,
    "alt": 0xFFE9, "super": 0xFFEB, "plus": 0x2B, "minus": 0x2D, "equal": 0x3D,
    "comma": 0x2C, "period": 0x2E, "slash": 0x2F, "backslash": 0x5C,
}
for _n in range(1, 13):
    KEYSYMS[f"f{_n}"] = 0xFFBE + _n - 1


def keysym(name):
    lower = name.lower()
    if lower in KEYSYMS:
        return KEYSYMS[lower]
    if len(name) == 1:
        return char_keysym(name)
    raise ValueError(f"unknown key {name}")


def char_keysym(ch):
    code = ord(ch)
    if ch == "\n":
        return 0xFF0D
    if ch == "\t":
        return 0xFF09
    if 0x20 <= code <= 0x7E or 0xA0 <= code <= 0xFF:
        return code
    return 0x01000000 | code


# --------------------------------------------------------------------------- server


def serve(args):
    import gi

    gi.require_version("Gio", "2.0")
    from gi.repository import Gio, GLib

    def log(message):
        print(f"[desktop] {message}", file=sys.stderr, flush=True)

    def call(bus, name, path, iface, method, params=None, reply=None, timeout=30000):
        return bus.call_sync(
            name, path, iface, method, params,
            GLib.VariantType(reply) if reply else None,
            Gio.DBusCallFlags.NONE, timeout, None,
        )

    def has_owner(bus, name):
        return call(
            bus, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
            "NameHasOwner", GLib.Variant("(s)", (name,)), "(b)",
        ).unpack()[0]

    width, height = (int(v) for v in args.size.lower().split("x"))
    shell_log = open(args.shell_log, "a") if args.shell_log else subprocess.DEVNULL
    # Settings the scenarios rely on, in the scratch HOME's dconf database:
    # new windows centered (so their place is known), no animations, no
    # screen blanking.
    for schema, key, value in (
        ("org.gnome.mutter", "center-new-windows", "true"),
        ("org.gnome.desktop.interface", "enable-animations", "false"),
        ("org.gnome.desktop.session", "idle-delay", "uint32 0"),
    ):
        subprocess.run(["gsettings", "set", schema, key, value], check=False)
    shell = subprocess.Popen(
        ["gnome-shell", "--headless", "--mode=user", "--wayland", "--no-x11",
         "--virtual-monitor", f"{width}x{height}", "--wayland-display", args.display],
        stdout=shell_log, stderr=subprocess.STDOUT,
    )
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    deadline = time.monotonic() + 60
    while not (has_owner(bus, "org.gnome.Shell.Screenshot") and has_owner(bus, "org.gnome.Mutter.RemoteDesktop")):
        if time.monotonic() > deadline or shell.poll() is not None:
            log("gnome-shell did not come up")
            return 1
        time.sleep(0.2)
    # Services the session bus starts on demand (the desktop portal and its file
    # chooser) need the display too.
    subprocess.run(
        ["dbus-update-activation-environment", f"WAYLAND_DISPLAY={args.display}",
         "XDG_CURRENT_DESKTOP=GNOME", "XDG_SESSION_TYPE=wayland"],
        check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    # The screenshot and introspection APIs only answer allowlisted names.
    for name in ("org.gnome.Screenshot", "org.freedesktop.impl.portal.desktop.gnome"):
        Gio.bus_own_name_on_connection(bus, name, Gio.BusNameOwnerFlags.NONE, None, None)
    time.sleep(1.0)
    try:
        bus.call_sync(
            "org.gnome.Shell", "/org/gnome/Shell", "org.freedesktop.DBus.Properties", "Set",
            GLib.Variant("(ssv)", ("org.gnome.Shell", "OverviewActive", GLib.Variant("b", False))),
            None, Gio.DBusCallFlags.NONE, 5000, None,
        )
    except GLib.Error as error:
        log(f"overview: {error.message}")

    rd = call(bus, "org.gnome.Mutter.RemoteDesktop", "/org/gnome/Mutter/RemoteDesktop",
              "org.gnome.Mutter.RemoteDesktop", "CreateSession", None, "(o)").unpack()[0]
    call(bus, "org.gnome.Mutter.RemoteDesktop", rd, "org.gnome.Mutter.RemoteDesktop.Session", "Start")

    def rdcall(method, params):
        call(bus, "org.gnome.Mutter.RemoteDesktop", rd, "org.gnome.Mutter.RemoteDesktop.Session",
             method, params)

    def move(x, y):
        rdcall("NotifyPointerMotionRelative", GLib.Variant("(dd)", (-20000.0, -20000.0)))
        time.sleep(0.02)
        rdcall("NotifyPointerMotionRelative", GLib.Variant("(dd)", (float(x), float(y))))
        time.sleep(0.05)

    def button(code, pressed):
        rdcall("NotifyPointerButton", GLib.Variant("(ib)", (code, pressed)))

    def keyval(sym, pressed):
        rdcall("NotifyKeyboardKeysym", GLib.Variant("(ub)", (sym, pressed)))

    def click(x, y, code=0x110, count=1):
        move(x, y)
        time.sleep(0.1)
        for _ in range(count):
            button(code, True)
            time.sleep(0.04)
            button(code, False)
            time.sleep(0.06)

    apps = {}

    def handle(req):
        op = req.get("op")
        if op == "launch":
            env = dict(os.environ)
            env["WAYLAND_DISPLAY"] = args.display
            env.pop("DISPLAY", None)
            env.update(req.get("env") or {})
            out = open(req["log"], "a") if req.get("log") else subprocess.DEVNULL
            proc = subprocess.Popen(req["cmd"], env=env, stdout=out, stderr=subprocess.STDOUT,
                                    cwd=req.get("cwd"))
            apps[proc.pid] = proc
            return {"pid": proc.pid}
        if op == "alive":
            proc = apps.get(req["pid"])
            return {"alive": bool(proc and proc.poll() is None),
                    "code": proc.poll() if proc else None}
        if op == "signal":
            proc = apps.get(req["pid"])
            if proc and proc.poll() is None:
                proc.send_signal(req.get("sig", 15))
            return {}
        if op == "wait_exit":
            proc = apps.get(req["pid"])
            if not proc:
                return {"code": None}
            try:
                return {"code": proc.wait(timeout=req.get("timeout", 10))}
            except subprocess.TimeoutExpired:
                return {"code": None}
        if op in ("click", "rclick", "mclick", "dclick"):
            code = {"click": 0x110, "dclick": 0x110, "rclick": 0x111, "mclick": 0x112}[op]
            click(req["x"], req["y"], code, 2 if op == "dclick" else 1)
            return {}
        if op == "move":
            move(req["x"], req["y"])
            return {}
        if op == "drag":
            move(req["x"], req["y"])
            button(0x110, True)
            time.sleep(0.1)
            steps = req.get("steps", 12)
            dx = (req["x2"] - req["x"]) / steps
            dy = (req["y2"] - req["y"]) / steps
            for _ in range(steps):
                rdcall("NotifyPointerMotionRelative", GLib.Variant("(dd)", (dx, dy)))
                time.sleep(0.03)
            time.sleep(0.1)
            button(0x110, False)
            return {}
        if op == "scroll":
            move(req["x"], req["y"])
            steps = int(req.get("dy", 1))
            for _ in range(abs(steps)):
                rdcall("NotifyPointerAxisDiscrete", GLib.Variant("(ui)", (0, 1 if steps > 0 else -1)))
                time.sleep(0.03)
            return {}
        if op == "key":
            syms = [keysym(part) for part in req["keys"].split("+")]
            for sym in syms:
                keyval(sym, True)
                time.sleep(0.02)
            for sym in reversed(syms):
                keyval(sym, False)
                time.sleep(0.02)
            return {}
        if op == "type":
            for ch in req["text"]:
                sym = char_keysym(ch)
                keyval(sym, True)
                keyval(sym, False)
                time.sleep(req.get("delay", 0.015))
            return {}
        if op == "shot":
            reply = call(bus, "org.gnome.Shell.Screenshot", "/org/gnome/Shell/Screenshot",
                         "org.gnome.Shell.Screenshot", "Screenshot",
                         GLib.Variant("(bbs)", (False, False, os.path.abspath(req["path"]))),
                         "(bs)").unpack()
            return {"ok": bool(reply[0]), "path": reply[1]}
        if op == "windows":
            reply = call(bus, "org.gnome.Shell", "/org/gnome/Shell/Introspect",
                         "org.gnome.Shell.Introspect", "GetWindows", None,
                         "(a{ta{sv}})").unpack()[0]
            windows = []
            for wid, props in reply.items():
                windows.append({
                    "id": wid, "title": props.get("title", ""), "app": props.get("app-id", ""),
                    "focus": bool(props.get("has-focus", False)),
                    "hidden": bool(props.get("is-hidden", False)),
                    "w": props.get("width", 0), "h": props.get("height", 0),
                })
            return {"windows": windows}
        if op == "setting":
            subprocess.run(["gsettings", "set", req["schema"], req["key"], req["value"]], check=False)
            return {}
        raise ValueError(f"unknown op {op}")

    if os.path.exists(args.socket):
        os.unlink(args.socket)
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(args.socket)
    server.listen(4)
    log("ready")
    stop = threading.Event()
    lock = threading.Lock()

    def client(conn):
        with conn, conn.makefile("rw") as stream:
            for line in stream:
                try:
                    req = json.loads(line)
                    if req.get("op") == "quit":
                        stop.set()
                        stream.write(json.dumps({"ok": True}) + "\n")
                        stream.flush()
                        return
                    with lock:
                        reply = handle(req)
                    reply.setdefault("ok", True)
                except Exception as error:  # noqa: BLE001 - reported to the caller
                    reply = {"ok": False, "error": f"{type(error).__name__}: {error}"}
                stream.write(json.dumps(reply) + "\n")
                stream.flush()

    server.settimeout(0.5)
    try:
        while not stop.is_set() and shell.poll() is None:
            try:
                conn, _ = server.accept()
            except socket.timeout:
                continue
            threading.Thread(target=client, args=(conn,), daemon=True).start()
    finally:
        for proc in apps.values():
            if proc.poll() is None:
                proc.terminate()
        for proc in apps.values():
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        shell.terminate()
        try:
            shell.wait(timeout=5)
        except subprocess.TimeoutExpired:
            shell.kill()
    return 0


# --------------------------------------------------------------------------- client


class Desktop:
    """Client of a running `serve`."""

    def __init__(self, path, timeout=60):
        deadline = time.monotonic() + timeout
        while True:
            try:
                self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.sock.connect(path)
                break
            except OSError:
                self.sock.close()
                if time.monotonic() > deadline:
                    raise
                time.sleep(0.3)
        self.stream = self.sock.makefile("rw")

    def request(self, op, **fields):
        fields["op"] = op
        self.stream.write(json.dumps(fields) + "\n")
        self.stream.flush()
        reply = json.loads(self.stream.readline())
        if not reply.get("ok", False):
            raise RuntimeError(f"{op}: {reply.get('error')}")
        return reply

    def click(self, x, y):
        self.request("click", x=x, y=y)

    def dclick(self, x, y):
        self.request("dclick", x=x, y=y)

    def rclick(self, x, y):
        self.request("rclick", x=x, y=y)

    def move(self, x, y):
        self.request("move", x=x, y=y)

    def key(self, keys):
        self.request("key", keys=keys)

    def type(self, text):
        self.request("type", text=text)

    def shot(self, path):
        return self.request("shot", path=path)["path"]

    def windows(self):
        return self.request("windows")["windows"]


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("serve")
    s.add_argument("--socket", required=True)
    s.add_argument("--size", default="1420x880")
    s.add_argument("--display", default="mx-e2e")
    s.add_argument("--shell-log")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    return serve(args)


if __name__ == "__main__":
    sys.exit(main())
