"""Runs inside the private session bus that hack/shot.sh sets up.

Starts a headless GNOME Shell, sets the virtual monitor's scale, launches one
application on its Wayland display, waits for it to settle and saves a
screenshot through org.gnome.Shell.Screenshot. Never run this directly on a
real desktop session: hack/shot.sh gives it a scratch HOME, its own
XDG_RUNTIME_DIR and its own D-Bus session, inside a systemd scope.
"""

import argparse
import os
import signal
import subprocess
import sys
import time

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

SHELL_NAME = "org.gnome.Shell"
DISPLAY_CONFIG = ("org.gnome.Mutter.DisplayConfig", "/org/gnome/Mutter/DisplayConfig")
SCREENSHOT = ("org.gnome.Shell.Screenshot", "/org/gnome/Shell/Screenshot")


def log(message):
    print(f"[shot] {message}", file=sys.stderr, flush=True)


def wait_for_name(bus, name, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        reply = bus.call_sync(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            GLib.Variant("(s)", (name,)),
            GLib.VariantType("(b)"),
            Gio.DBusCallFlags.NONE,
            -1,
            None,
        )
        if reply.unpack()[0]:
            return True
        time.sleep(0.2)
    return False


def call(bus, target, interface, method, args, reply_type=None, timeout_ms=30000):
    name, path = target
    return bus.call_sync(
        name,
        path,
        interface,
        method,
        args,
        GLib.VariantType(reply_type) if reply_type else None,
        Gio.DBusCallFlags.NONE,
        timeout_ms,
        None,
    )


def apply_scale(bus, scale):
    """Put every monitor at the requested integer scale (temporary config)."""
    state = call(
        bus,
        DISPLAY_CONFIG,
        "org.gnome.Mutter.DisplayConfig",
        "GetCurrentState",
        None,
        "(ua((ssss)a(siiddada{sv})a{sv})a(iiduba(ssss)a{sv})a{sv})",
    ).unpack()
    serial, monitors, _logical, _props = state
    logical = []
    x = 0
    for (connector, _vendor, _product, _serial), modes, _mprops in monitors:
        current = next((m for m in modes if m[6].get("is-current")), modes[0])
        mode_id, width, _height, _refresh, _pref, scales, _modeprops = current
        if scale not in scales:
            log(f"scale {scale} not offered for {connector} ({width}px wide); offered {list(scales)}")
            return False
        logical.append((x, 0, float(scale), 0, x == 0, [(connector, mode_id, {})]))
        x += int(width / scale)
    call(
        bus,
        DISPLAY_CONFIG,
        "org.gnome.Mutter.DisplayConfig",
        "ApplyMonitorsConfig",
        GLib.Variant("(uua(iiduba(ssa{sv}))a{sv})", (serial, 1, logical, {})),
    )
    return True


def hide_overview(bus):
    """GNOME Shell opens the overview at startup; close it so windows show as they are."""
    try:
        bus.call_sync(
            SHELL_NAME,
            "/org/gnome/Shell",
            "org.freedesktop.DBus.Properties",
            "Set",
            GLib.Variant("(ssv)", ("org.gnome.Shell", "OverviewActive", GLib.Variant("b", False))),
            None,
            Gio.DBusCallFlags.NONE,
            5000,
            None,
        )
    except GLib.Error as error:
        log(f"could not hide the overview: {error.message}")


class Pointer:
    """Pointer input through org.gnome.Mutter.RemoteDesktop (no screen cast
    needed): relative motion is 1:1 in logical pixels, so the pointer is
    homed against the top-left corner before every move."""

    BTN_LEFT = 0x110
    BTN_RIGHT = 0x111

    def __init__(self, bus):
        self.bus = bus
        reply = call(
            bus,
            ("org.gnome.Mutter.RemoteDesktop", "/org/gnome/Mutter/RemoteDesktop"),
            "org.gnome.Mutter.RemoteDesktop",
            "CreateSession",
            None,
            "(o)",
        ).unpack()
        self.session = ("org.gnome.Mutter.RemoteDesktop", reply[0])
        call(bus, self.session, "org.gnome.Mutter.RemoteDesktop.Session", "Start", None)

    def move_to(self, x, y):
        for dx, dy in ((-10000.0, -10000.0), (float(x), float(y))):
            call(
                self.bus,
                self.session,
                "org.gnome.Mutter.RemoteDesktop.Session",
                "NotifyPointerMotionRelative",
                GLib.Variant("(dd)", (dx, dy)),
            )
            time.sleep(0.05)

    def button(self, button, pressed):
        call(
            self.bus,
            self.session,
            "org.gnome.Mutter.RemoteDesktop.Session",
            "NotifyPointerButton",
            GLib.Variant("(ib)", (button, pressed)),
        )

    def keys(self, codes):
        """Press evdev keycodes in order and release them in reverse (a chord)."""
        for code in codes:
            call(
                self.bus,
                self.session,
                "org.gnome.Mutter.RemoteDesktop.Session",
                "NotifyKeyboardKeycode",
                GLib.Variant("(ub)", (code, True)),
            )
            time.sleep(0.03)
        for code in reversed(codes):
            call(
                self.bus,
                self.session,
                "org.gnome.Mutter.RemoteDesktop.Session",
                "NotifyKeyboardKeycode",
                GLib.Variant("(ub)", (code, False)),
            )
            time.sleep(0.03)

    def click(self, x, y, button=BTN_LEFT):
        self.move_to(x, y)
        time.sleep(0.15)
        self.button(button, True)
        time.sleep(0.05)
        self.button(button, False)


def run_steps(bus, steps):
    """`click:X,Y`, `rclick:X,Y`, `move:X,Y` (logical pixels), `wait:SECONDS`,
    `key:CODE[+CODE...]` (evdev keycodes pressed as one chord)."""
    pointer = None
    for step in steps:
        kind, _, arg = step.partition(":")
        if kind == "wait":
            time.sleep(float(arg))
            continue
        if pointer is None:
            pointer = Pointer(bus)
        if kind == "key":
            pointer.keys([int(code) for code in arg.split("+")])
            time.sleep(0.3)
            continue
        x, y = (float(v) for v in arg.split(","))
        if kind == "click":
            pointer.click(x, y)
        elif kind == "rclick":
            pointer.click(x, y, Pointer.BTN_RIGHT)
        elif kind == "move":
            pointer.move_to(x, y)
        else:
            log(f"unknown step {step}")
        time.sleep(0.3)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--size", default="1420x880")
    parser.add_argument("--scale", type=int, default=2)
    parser.add_argument("--wait", type=float, default=6.0)
    parser.add_argument("--display", default="mx-shot")
    parser.add_argument("--dark", default="0")
    parser.add_argument("--step", action="append", default=[])
    parser.add_argument("--out", required=True)
    parser.add_argument("--log", required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = [part for part in args.command if part != "--"] if args.command[:1] == ["--"] else args.command
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        log("no command given")
        return 2

    width, height = (int(v) for v in args.size.lower().split("x"))
    physical = f"{width * args.scale}x{height * args.scale}"
    shell_log = open(args.log + ".shell", "a")
    shell = subprocess.Popen(
        [
            "gnome-shell",
            "--headless",
            # The plain GNOME mode: the distribution's session mode would load
            # its dock and desktop-icons extensions into the screenshot.
            "--mode=user",
            "--wayland",
            "--no-x11",
            "--virtual-monitor",
            physical,
            "--wayland-display",
            args.display,
        ],
        stdout=shell_log,
        stderr=subprocess.STDOUT,
        start_new_session=False,
    )
    app = None
    try:
        bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        if not wait_for_name(bus, SCREENSHOT[0], 60) or not wait_for_name(bus, DISPLAY_CONFIG[0], 60):
            log("gnome-shell did not come up; see " + args.log + ".shell")
            return 1
        # The screenshot API only answers callers on its allowlist; owning the
        # name of the GNOME Screenshot app puts this process on it.
        Gio.bus_own_name_on_connection(bus, "org.gnome.Screenshot", Gio.BusNameOwnerFlags.NONE, None, None)
        if args.scale != 1 and not apply_scale(bus, args.scale):
            return 1
        if args.dark == "1":
            # Settings live in the scratch HOME's dconf database.
            subprocess.run(
                ["gsettings", "set", "org.gnome.desktop.interface", "color-scheme", "prefer-dark"],
                check=False,
            )
        time.sleep(1.0)

        env = dict(os.environ)
        env["WAYLAND_DISPLAY"] = args.display
        env.pop("DISPLAY", None)
        app_log = open(args.log, "w")
        log("launching: " + " ".join(command))
        app = subprocess.Popen(command, env=env, stdout=app_log, stderr=subprocess.STDOUT)

        hide_overview(bus)
        deadline = time.monotonic() + args.wait
        while time.monotonic() < deadline:
            if app.poll() is not None:
                log(f"application exited early with {app.returncode}; see {args.log}")
                return 1
            time.sleep(0.1)

        hide_overview(bus)
        if args.step:
            run_steps(bus, args.step)
        time.sleep(0.5)
        out = os.path.abspath(args.out)
        reply = call(
            bus,
            SCREENSHOT,
            "org.gnome.Shell.Screenshot",
            "Screenshot",
            GLib.Variant("(bbs)", (False, False, out)),
            "(bs)",
        ).unpack()
        if not reply[0]:
            log("screenshot failed")
            return 1
        log("saved " + reply[1])
        return 0
    finally:
        for proc in (app, shell):
            if proc and proc.poll() is None:
                proc.send_signal(signal.SIGTERM)
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()


if __name__ == "__main__":
    sys.exit(main())
