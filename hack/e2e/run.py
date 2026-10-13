#!/usr/bin/env python3
"""End-to-end runs of the real app against real servers.

    hack/e2e/run.py all   [--binary B] [--work DIR] [--only NAME,...] [--keep]
    hack/e2e/run.py up    [--binary B] [--work DIR]      # servers, desktop, app; stays up
    hack/e2e/run.py ctl   [--work DIR] <op> [args...]    # one desktop request (see desktop.py)
    hack/e2e/run.py test  [--work DIR] [--only NAME,...] # scenarios against a running `up`
    hack/e2e/run.py down  [--work DIR] [--images]

`all` brings everything up, runs every scenario in scenarios.py, prints one
line per scenario and tears everything down again (containers, the headless
desktop's systemd scope, the scratch folders; `--keep` leaves them for a
look). The app runs with a scratch HOME whose ~/.ssh/config names the
containers, and with MUXUS_DATA_DIR in the work folder, so nothing touches the
real data. Screenshots and logs of a run land in <work>/out.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)

from desktop import Desktop  # noqa: E402

DEFAULT_WORK = os.path.join(tempfile.gettempdir(), "muxusrs-e2e")
SIZE = (1420, 880)

SSH_CONFIG = """\
Host e2e-cfgfwd
    HostName 127.0.0.1
    Port 22201
    User alice
    IdentityFile ~/.ssh/id_ed25519
    IdentitiesOnly yes
    LocalForward 18090 127.0.0.1:8080
    DynamicForward 11081

Host e2e-ssh
    HostName 127.0.0.1
    Port 22201
    User alice
    IdentityFile ~/.ssh/id_ed25519
    IdentitiesOnly yes

Host e2e-pass
    HostName 127.0.0.1
    Port 22201
    User alice
    PubkeyAuthentication no

Host e2e-kbd
    HostName 127.0.0.1
    Port 22201
    User kbd
    PubkeyAuthentication no

Host e2e-nofwd
    HostName 127.0.0.1
    Port 22201
    User nofwd
    IdentityFile ~/.ssh/id_ed25519
    IdentitiesOnly yes

Host e2e-jump
    HostName 127.0.0.1
    Port 22202
    User alice
    IdentityFile ~/.ssh/id_ed25519
    IdentitiesOnly yes

Host e2e-inner
    HostName inner
    User alice
    ProxyJump e2e-jump
    IdentityFile ~/.ssh/id_ed25519
    IdentitiesOnly yes

Host e2e-tofu
    HostName 127.0.0.1
    Port 22203
    User alice
    IdentityFile ~/.ssh/id_ed25519
    IdentitiesOnly yes
"""


class Work:
    """The scratch layout of one run."""

    def __init__(self, path):
        self.path = os.path.abspath(path)
        self.home = os.path.join(self.path, "home")
        self.data = os.path.join(self.path, "data")
        self.out = os.path.join(self.path, "out")
        self.state_file = os.path.join(self.path, "state.json")

    def state(self):
        try:
            with open(self.state_file) as f:
                return json.load(f)
        except FileNotFoundError:
            return {}

    def save(self, state):
        with open(self.state_file, "w") as f:
            json.dump(state, f, indent=2)


def sh(*cmd, check=True, **kw):
    return subprocess.run(list(cmd), check=check, **kw)


def prepare_home(work):
    home = work.home
    ssh = os.path.join(home, ".ssh")
    os.makedirs(ssh, mode=0o700, exist_ok=True)
    for sub in (".config", ".local/share", ".cache", ".local/state", "Downloads"):
        os.makedirs(os.path.join(home, sub), exist_ok=True)
    os.makedirs(work.data, mode=0o700, exist_ok=True)
    os.makedirs(work.out, exist_ok=True)
    key = os.path.join(ssh, "id_ed25519")
    if not os.path.exists(key):
        sh("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "muxus-e2e", "-f", key)
    with open(os.path.join(ssh, "config"), "w") as f:
        f.write(SSH_CONFIG)
    # The desktop portal's GTK backend for every interface (file chooser included): the
    # GNOME backend's bus name is the one desktop.py holds to list windows.
    portal = os.path.join(home, ".config", "xdg-desktop-portal")
    os.makedirs(portal, exist_ok=True)
    with open(os.path.join(portal, "portals.conf"), "w") as f:
        f.write("[preferred]\ndefault=gtk;\n")
    # A plain bash prompt: the machine's real shell rc files never load.
    with open(os.path.join(home, ".bashrc"), "w") as f:
        f.write("PS1='e2e$ '\n")
    return key + ".pub"


def known_hosts(work):
    """Trust the servers up front, except the one the host-key scenario asks about."""
    lines = []
    for port in (22201, 22202):
        out = subprocess.run(
            ["ssh-keyscan", "-p", str(port), "-t", "ed25519,ecdsa,rsa", "127.0.0.1"],
            capture_output=True, text=True, check=False,
        ).stdout
        lines.append(out)
    with open(os.path.join(work.home, ".ssh", "known_hosts"), "w") as f:
        f.write("".join(lines))


def desktop_up(work, state):
    runtime = tempfile.mkdtemp(prefix="mx-", dir="/tmp")
    # GNOME Shell starts an ibus-daemon of its own; in these sessions it grows by the
    # gigabyte and took the whole scope down. A stand-in that only waits keeps it out
    # (keys still arrive; only input methods are missing, which no scenario uses).
    stubs = os.path.join(work.path, "bin")
    os.makedirs(stubs, exist_ok=True)
    stub = os.path.join(stubs, "ibus-daemon")
    with open(stub, "w") as f:
        f.write("#!/bin/sh\nexec sleep 2147483647\n")
    os.chmod(stub, 0o755)
    unit = f"muxusrs-e2e-{os.getpid()}"
    sock = os.path.join(runtime, "desktop.sock")
    env_vars = {
        "HOME": work.home,
        "XDG_RUNTIME_DIR": runtime,
        "XDG_CONFIG_HOME": os.path.join(work.home, ".config"),
        "XDG_DATA_HOME": os.path.join(work.home, ".local/share"),
        "XDG_CACHE_HOME": os.path.join(work.home, ".cache"),
        "XDG_STATE_HOME": os.path.join(work.home, ".local/state"),
        "NO_AT_BRIDGE": "1",
        "GTK_A11Y": "none",
        "LANG": "C.UTF-8",
        "PATH": stubs + os.pathsep + os.environ.get("PATH", ""),
    }
    cmd = [
        "systemd-run", "--user", "--scope", "--quiet", f"--unit={unit}",
        "-p", "MemoryMax=6G", "-p", "MemorySwapMax=0", "--",
        "env", "-u", "DBUS_SESSION_BUS_ADDRESS", "-u", "DISPLAY", "-u", "WAYLAND_DISPLAY",
        "-u", "ZDOTDIR", "-u", "SSH_AUTH_SOCK", "-u", "SSH_AGENT_PID",
        *[f"{k}={v}" for k, v in env_vars.items()],
        "dbus-run-session", "--", "python3", "-I", os.path.join(HERE, "desktop.py"), "serve",
        "--socket", sock, "--size", f"{SIZE[0]}x{SIZE[1]}", "--display", "mx-e2e",
        "--shell-log", os.path.join(work.out, "gnome-shell.log"),
    ]
    log = open(os.path.join(work.out, "desktop.log"), "a")
    proc = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    state.update({"unit": unit, "runtime": runtime, "socket": sock, "desktop_pid": proc.pid})
    work.save(state)
    Desktop(sock, timeout=90)
    return sock


def serial_up(work, state):
    """A pty pair (socat) with a console answering on its far end (serial_echo.py)."""
    a = os.path.join(work.path, "ttyA")
    b = os.path.join(work.path, "ttyB")
    for path in (a, b, b + ".lines"):
        if os.path.lexists(path):
            os.unlink(path)
    socat = subprocess.Popen(
        ["socat", f"pty,raw,echo=0,link={a}", f"pty,raw,echo=0,link={b}"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
    )
    deadline = time.time() + 5
    while not (os.path.exists(a) and os.path.exists(b)) and time.time() < deadline:
        time.sleep(0.1)
    echo = subprocess.Popen(
        [sys.executable, "-I", os.path.join(HERE, "serial_echo.py"), b],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
    )
    state.update({"serial": a, "serial_pids": [socat.pid, echo.pid]})


def serial_down(state):
    for pid in state.get("serial_pids", []):
        try:
            os.kill(pid, 15)
        except ProcessLookupError:
            pass


def desktop_down(state):
    unit = state.get("unit")
    if unit:
        subprocess.run(["systemctl", "--user", "stop", f"{unit}.scope"], check=False,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    runtime = state.get("runtime")
    if runtime:
        with open("/proc/mounts") as f:
            for line in f:
                mount = line.split()[1]
                if mount.startswith(runtime + "/"):
                    subprocess.run(["fusermount3", "-u", "-z", mount], check=False,
                                   stderr=subprocess.DEVNULL)
        shutil.rmtree(runtime, ignore_errors=True)


def app_env(work, extra=None):
    env = {
        "MUXUS_DATA_DIR": work.data,
        "MUXUS_THEME": "light",
        "RUST_LOG": os.environ.get("E2E_RUST_LOG", "info"),
        "SHELL": "/bin/bash",
    }
    env.update(extra or {})
    return env


def launch_app(desk, work, binary, args=(), extra_env=None, log_name="app.log"):
    reply = desk.request(
        "launch", cmd=[binary, *args], env=app_env(work, extra_env),
        log=os.path.join(work.out, log_name), cwd=work.home,
    )
    return reply["pid"]


def up(args):
    work = Work(args.work)
    os.makedirs(work.path, exist_ok=True)
    state = work.state()
    if state.get("unit"):
        desktop_down(state)
        serial_down(state)
    pub = prepare_home(work)
    if not args.no_containers:
        sh(os.path.join(HERE, "containers.sh"), "up", pub)
    known_hosts(work)
    state = {"binary": os.path.abspath(args.binary), "work": work.path}
    serial_up(work, state)
    sock = desktop_up(work, state)
    if not args.no_app:
        desk = Desktop(sock)
        state["app_pid"] = launch_app(desk, work, state["binary"])
        work.save(state)
    print(json.dumps(state, indent=2))


def down(args):
    work = Work(args.work)
    desktop_down(work.state())
    serial_down(work.state())
    if os.path.exists(work.state_file):
        os.unlink(work.state_file)
    if not args.keep_containers:
        cmd = [os.path.join(HERE, "containers.sh"), "down"]
        if args.images:
            cmd.append("--images")
        sh(*cmd, check=False)


def ctl(args):
    work = Work(args.work)
    desk = Desktop(work.state()["socket"], timeout=5)
    op, rest = args.op, args.rest
    if op in ("click", "dclick", "rclick", "mclick", "move"):
        print(desk.request(op, x=float(rest[0]), y=float(rest[1])))
    elif op == "drag":
        print(desk.request(op, x=float(rest[0]), y=float(rest[1]), x2=float(rest[2]), y2=float(rest[3])))
    elif op == "scroll":
        print(desk.request(op, x=float(rest[0]), y=float(rest[1]), dy=int(rest[2])))
    elif op == "key":
        for chord in rest:
            desk.key(chord)
    elif op == "type":
        desk.type(" ".join(rest))
    elif op == "shot":
        print(desk.shot(os.path.abspath(rest[0])))
    elif op == "windows":
        for w in desk.windows():
            print(w)
    elif op == "launch":
        print(launch_app(desk, work, work.state()["binary"], rest,
                         log_name=f"app-{int(time.time())}.log"))
    elif op == "relaunch":
        state = work.state()
        if state.get("app_pid"):
            desk.request("signal", pid=state["app_pid"], sig=15)
            desk.request("wait_exit", pid=state["app_pid"], timeout=10)
        state["app_pid"] = launch_app(desk, work, state["binary"], rest)
        work.save(state)
        print(state["app_pid"])
    else:
        print(desk.request(op))


def test(args):
    import scenarios

    work = Work(args.work)
    state = work.state()
    only = set(args.only.split(",")) if args.only else None
    return scenarios.run_all(work, state, only)


def run_all(args):
    work = Work(args.work)
    if os.path.exists(work.path) and not args.reuse:
        shutil.rmtree(work.path, ignore_errors=True)
    args.no_app = True
    status = 1
    try:
        up(args)
        status = test(args)
    finally:
        if not args.keep:
            args.keep_containers = False
            down(args)
    return status


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--work", default=DEFAULT_WORK)
    sub = parser.add_subparsers(dest="cmd", required=True)
    binary_default = os.path.join(ROOT, "target-release", "release", "muxus")
    p_all = sub.add_parser("all")
    p_all.add_argument("--binary", default=binary_default)
    p_all.add_argument("--only")
    p_all.add_argument("--keep", action="store_true")
    p_all.add_argument("--reuse", action="store_true")
    p_all.add_argument("--images", action="store_true")
    p_all.add_argument("--no-containers", action="store_true")
    p_up = sub.add_parser("up")
    p_up.add_argument("--binary", default=binary_default)
    p_up.add_argument("--no-app", action="store_true")
    p_up.add_argument("--no-containers", action="store_true")
    p_ctl = sub.add_parser("ctl")
    p_ctl.add_argument("op")
    p_ctl.add_argument("rest", nargs="*")
    p_test = sub.add_parser("test")
    p_test.add_argument("--only")
    p_down = sub.add_parser("down")
    p_down.add_argument("--images", action="store_true")
    p_down.add_argument("--keep-containers", action="store_true")
    args = parser.parse_args()
    for name in ("work",):
        setattr(args, name, os.path.abspath(getattr(args, name)))
    handler = {"all": run_all, "up": up, "ctl": ctl, "test": test, "down": down}[args.cmd]
    result = handler(args)
    return result if isinstance(result, int) else 0


if __name__ == "__main__":
    sys.exit(main())
