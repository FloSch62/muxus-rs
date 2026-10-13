"""The end-to-end scenarios hack/e2e/run.py runs against the real app.

Each scenario drives the GUI through desktop.py (pointer, keyboard, screenshots,
the shell's window list) and checks what the user would see or get: files in
the browser, windows that close, traffic through tunnels, lines in main.log,
pixels that changed. Coordinates are logical pixels of the 1420x880 virtual
monitor; the main window opens maximized below GNOME Shell's 32px top bar.

Use it interactively too:

    python3 -i hack/e2e/scenarios.py --work <work>     # e = E2E(...) attached
"""

import json
import os
import re
import subprocess
import sys
import time
import traceback
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from desktop import Desktop  # noqa: E402

try:
    from PIL import Image, ImageChops
except ImportError:  # pragma: no cover - environment dependent
    Image = None

# Where things are in the maximized main window (1420x848 below the shell's bar).
TOP_BAR_Y = 57
SIDEBAR_FIRST_HOST_Y = 206  # first row under the "Hosts" folder
ROW = 27
# The hosts of the scratch ~/.ssh/config, listed under "Hosts" in name order.
CONFIG_HOSTS = ["e2e-cfgfwd", "e2e-inner", "e2e-jump", "e2e-kbd", "e2e-nofwd", "e2e-pass", "e2e-ssh", "e2e-tofu"]
# The Add host dialog: kind tabs, the General fields, Save & connect.
ADD_HOST_TABS = {"ssh": 347, "telnet": 470, "serial": 590, "rdp": 712, "vnc": 833}
FIELD_NAME = (800, 289)
FIELD_HOST = (750, 365)
FIELD_PORT = (1077, 365)
SAVE_AND_CONNECT = (1079, 749)


class Failed(AssertionError):
    pass


def check(condition, message):
    if not condition:
        raise Failed(message)


class E2E:
    def __init__(self, work, state):
        self.work = work
        self.state = state
        self.desk = Desktop(state["socket"], timeout=10)
        self.out = os.path.join(work.path, "out")
        self.hosts = list(CONFIG_HOSTS)
        self.log_path = os.path.join(work.data, "logs", "main.log")
        self.shots = 0

    # ----------------------------------------------------------------- input

    def click(self, x, y, wait=0.3):
        self.desk.click(x, y)
        time.sleep(wait)

    def dclick(self, x, y, wait=0.3):
        self.desk.dclick(x, y)
        time.sleep(wait)

    def rclick(self, x, y, wait=0.5):
        self.desk.rclick(x, y)
        time.sleep(wait)

    def move(self, x, y):
        self.desk.move(x, y)

    def key(self, *chords, wait=0.2):
        for chord in chords:
            self.desk.key(chord)
        time.sleep(wait)

    def type(self, text, wait=0.2):
        self.desk.type(text)
        time.sleep(wait)

    def scroll(self, x, y, steps):
        self.desk.request("scroll", x=x, y=y, dy=steps)

    # ----------------------------------------------------------------- looking

    def shot(self, name, crop=None):
        """A screenshot (and its crop, x,y,w,h logical px) in out/; returns the path."""
        self.shots += 1
        path = os.path.join(self.out, f"{name}.png")
        self.desk.shot(path)
        if crop and Image:
            x, y, w, h = crop
            Image.open(path).crop((x, y, x + w, y + h)).save(os.path.join(self.out, f"{name}-crop.png"))
        return path

    def changed(self, before, after, box=None, threshold=24):
        """Share of pixels (0..1) that differ by more than `threshold` in `box`."""
        a = Image.open(before).convert("RGB")
        b = Image.open(after).convert("RGB")
        if box:
            x, y, w, h = box
            a = a.crop((x, y, x + w, y + h))
            b = b.crop((x, y, x + w, y + h))
        diff = ImageChops.difference(a, b).convert("L")
        hist = diff.histogram()
        changed = sum(hist[threshold + 1:])
        return changed / (a.size[0] * a.size[1])

    def ink_rows(self, path, box, background_tolerance=12):
        """How many pixel rows of `box` carry something other than the background
        (the top-left pixel's color): a rough count of what a list shows."""
        im = Image.open(path).convert("RGB")
        x, y, w, h = box
        im = im.crop((x, y, x + w, y + h))
        bg = im.getpixel((0, 0))
        rows = 0
        for row in range(im.size[1]):
            for col in range(0, im.size[0], 2):
                px = im.getpixel((col, row))
                if max(abs(px[i] - bg[i]) for i in range(3)) > background_tolerance:
                    rows += 1
                    break
        return rows

    def windows(self):
        return self.desk.windows()

    def wait_windows(self, count, timeout=10):
        deadline = time.time() + timeout
        while time.time() < deadline:
            wins = self.windows()
            if len(wins) == count:
                return wins
            time.sleep(0.2)
        raise Failed(f"expected {count} windows, have {[w['title'] for w in self.windows()]}")

    def log(self):
        try:
            with open(self.log_path, encoding="utf-8", errors="replace") as f:
                return f.read()
        except FileNotFoundError:
            return ""

    def log_mark(self):
        return len(self.log())

    def wait_log(self, pattern, since=0, timeout=15):
        rx = re.compile(pattern)
        deadline = time.time() + timeout
        while time.time() < deadline:
            text = self.log()[since:]
            m = rx.search(text)
            if m:
                return m
            time.sleep(0.2)
        raise Failed(f"main.log never matched {pattern!r}; tail:\n{self.log()[since:][-1500:]}")

    # ----------------------------------------------------------------- the app

    def relaunch(self, fresh=False, args=(), env=None, wait=7):
        """Restart the app; `fresh` wipes its data folder (no restored workspace)."""
        pid = self.state.get("app_pid")
        if pid:
            self.desk.request("signal", pid=pid, sig=15)
            self.desk.request("wait_exit", pid=pid, timeout=10)
        if fresh:
            subprocess.run(["rm", "-rf", self.work.data], check=False)
            os.makedirs(self.work.data, mode=0o700, exist_ok=True)
            self.hosts = list(CONFIG_HOSTS)
        from run import launch_app
        self.state["app_pid"] = launch_app(self.desk, self.work, self.state["binary"], args, env)
        self.work.save(self.state)
        time.sleep(wait)
        # A tap on Shift gives the session its keyboard.
        self.key("shift", wait=0.1)

    def app_alive(self):
        return self.desk.request("alive", pid=self.state["app_pid"])["alive"]

    def host_row(self, alias):
        order = sorted(self.hosts, key=str.lower)
        return (75, SIDEBAR_FIRST_HOST_Y + ROW * order.index(alias))

    def add_host(self, kind, name, host, port, then=None):
        """Add host → kind tab → name, host, port (→ `then(e)` for more) → Save & connect."""
        self.click(263, 113, wait=1)
        self.click(ADD_HOST_TABS[kind], 225, wait=0.8)
        self.click(*FIELD_NAME)
        self.type(name)
        self.click(*FIELD_HOST)
        self.type(host)
        if port is not None:
            self.click(*FIELD_PORT)
            self.key("ctrl+a")
            self.type(str(port))
        self.key("Escape") if kind == "serial" else None
        if then:
            then(self)
        self.hosts.append(name)
        self.click(*SAVE_AND_CONNECT, wait=3)

    def open_host(self, alias, wait=4):
        # One click opens (the default `sidebarOpenGesture`); a double click would land
        # its second press on a sign-in dialog that is already up, and cancel it.
        x, y = self.host_row(alias)
        self.click(x, y, wait=wait)

    def terminal_type(self, text, enter=True):
        self.type(text)
        if enter:
            self.key("Return")

    # ----------------------------------------------------------------- servers

    def docker(self, *args, check=True):
        return subprocess.run(["docker", *args], check=check, capture_output=True, text=True)

    def exec_in(self, container, command):
        return self.docker("exec", container, "sh", "-c", command, check=False).stdout

    def wait_file(self, container, path, timeout=10):
        """Wait until `path` exists in the container (a command typed in the app made it)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.docker("exec", container, "test", "-e", path, check=False).returncode == 0:
                return
            time.sleep(0.3)
        raise Failed(f"{path} never appeared in {container}")

    @staticmethod
    def http(port, timeout=5, proxy=None):
        if proxy:
            out = subprocess.run(
                ["curl", "-s", "-m", str(timeout), "--socks5-hostname", f"127.0.0.1:{proxy}",
                 f"http://127.0.0.1:{port}/"], capture_output=True, text=True)
            return out.stdout
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=timeout) as r:
                return r.read().decode()
        except OSError as error:
            return f"error: {error}"


# =========================================================================== scenarios
#
# Each takes an E2E and raises Failed (or any exception) to fail. They run in order
# against one desktop; each starts the app as it needs it.

SCENARIOS = []


def scenario(fn):
    SCENARIOS.append(fn)
    return fn


@scenario
def logging_records_connections(e):
    """main.log carries the start line, the connect attempt and the session."""
    e.relaunch(fresh=True)
    mark = e.log_mark()
    e.open_host("e2e-ssh")
    e.wait_log(r"INFO ssh connect requested target=e2e-ssh", mark)
    e.wait_log(r"INFO ssh session established target=e2e-ssh host=127\.0\.0\.1 user=alice", mark)
    check("Muxus " in e.log() and " starting on " in e.log(), "no start line")


@scenario
def sftp_follows_the_terminal(e):
    """The browser beside the terminal lists the home, then follows `cd many` and
    shows that folder's files (it kept the old rows before)."""
    e.click(988, TOP_BAR_Y, wait=2.5)
    home = e.shot("sftp-home", crop=(1040, 130, 380, 730))
    rows_home = e.ink_rows(home, (1050, 275, 160, 530))
    check(rows_home > 300, f"the home listing looks empty ({rows_home} ink rows)")
    e.click(600, 400)
    e.terminal_type("cd many")
    time.sleep(3)
    many = e.shot("sftp-many", crop=(1040, 130, 380, 730))
    check(e.changed(home, many, (1050, 275, 160, 530)) > 0.03, "the list did not change after cd")
    # dir-01 … are folders: the first row's icon is a folder (yellow), as in the home.
    e.terminal_type("cd ~")
    time.sleep(2)


@scenario
def sftp_window_closes(e):
    """The browser in a window of its own has window controls; close works."""
    e.click(1397, 146, wait=3)
    wins = e.wait_windows(2)
    check(any("SFTP" in w["title"] for w in wins), f"no SFTP window: {wins}")
    e.shot("sftp-window", crop=(0, 32, 1420, 120))
    e.click(1398, TOP_BAR_Y, wait=1.5)
    wins = e.wait_windows(1)
    check(wins[0]["title"] == "Muxus", f"the main window is gone: {wins}")


@scenario
def ssh_sign_in_methods(e):
    """Key, password, keyboard-interactive (PAM ends it with an empty round), a host-key
    question, and a jump host: each session runs a command on the right server."""
    from run import known_hosts
    known_hosts(e.work)  # only the servers the scenarios trust up front
    e.relaunch(fresh=True)
    mark = e.log_mark()
    stamp = str(int(time.time()))
    # Password: the dialog asks once; the PAM round after it asks nothing.
    e.open_host("e2e-pass", wait=2)
    e.shot("auth-password")
    e.type("alice-pass")
    e.key("Return", wait=3)
    e.terminal_type(f"touch /tmp/pass-{stamp}")
    e.wait_file("muxusrs-e2e-ssh", f"/tmp/pass-{stamp}")
    # Keyboard-interactive only.
    e.open_host("e2e-kbd", wait=2)
    e.type("kbd-pass")
    e.key("Return", wait=3)
    e.terminal_type(f"touch /tmp/kbd-{stamp}")
    e.wait_file("muxusrs-e2e-ssh", f"/tmp/kbd-{stamp}")
    # An unknown host key: the question, then trusted into known_hosts.
    known = open(os.path.join(e.work.home, ".ssh", "known_hosts")).read()
    check("22203" not in known, "the host-key scenario's port is trusted already")
    e.open_host("e2e-tofu", wait=2)
    e.shot("auth-host-key")
    e.click(960, 539, wait=3)
    known = open(os.path.join(e.work.home, ".ssh", "known_hosts")).read()
    check("[127.0.0.1]:22203" in known, "the trusted key was not written to known_hosts")
    e.terminal_type(f"touch /tmp/tofu-{stamp}")
    e.wait_file("muxusrs-e2e-jump", f"/tmp/tofu-{stamp}")
    # Through the jump host (its own host key is new too).
    e.open_host("e2e-inner", wait=4)
    e.click(960, 539, wait=4)
    e.terminal_type(f"touch /tmp/inner-{stamp}")
    e.wait_file("muxusrs-e2e-inner", f"/tmp/inner-{stamp}")
    e.wait_log(r"ssh session established target=e2e-inner host=inner", mark)
    for target in ("e2e-pass", "e2e-kbd", "e2e-tofu"):
        e.wait_log(rf"ssh session established target={target} ", mark)


@scenario
def telnet_serial_and_local_terminals(e):
    """Saved telnet and serial hosts and the local shell carry typed commands."""
    stamp = str(int(time.time()))
    mark = e.log_mark()
    e.add_host("telnet", "e2e-telnet", "127.0.0.1", 22323)
    e.type("tuser")
    e.key("Return", wait=1.5)
    e.type("tuser-pass")
    e.key("Return", wait=2)
    e.terminal_type(f"touch /tmp/telnet-{stamp}")
    e.wait_file("muxusrs-e2e-telnet", f"/tmp/telnet-{stamp}")
    e.wait_log(r"telnet session established host=127\.0\.0\.1 port=22323", mark)

    # A fresh pty pair: a killed app leaves its exclusive lock on the old one (socat keeps
    # the device open, so the kernel never clears it).
    from run import serial_down, serial_up
    serial_down(e.state)
    serial_up(e.work, e.state)
    e.work.save(e.state)
    serial = e.state["serial"]
    e.add_host("serial", "e2e-serial", serial, None)
    e.terminal_type(f"show version {stamp}")
    deadline = time.time() + 5
    lines = ""
    while time.time() < deadline:
        try:
            lines = open(os.path.join(e.work.path, "ttyB.lines")).read()
        except FileNotFoundError:
            lines = ""
        if f"show version {stamp}" in lines:
            break
        time.sleep(0.2)
    check(f"show version {stamp}" in lines, f"the serial console never got the line: {lines!r}")
    e.wait_log(r"serial session established path=", mark)

    e.click(80, 152, wait=3)  # Local terminal
    marker = os.path.join(e.work.path, f"local-{stamp}")
    e.terminal_type(f"touch {marker}")
    deadline = time.time() + 5
    while time.time() < deadline and not os.path.exists(marker):
        time.sleep(0.2)
    check(os.path.exists(marker), "the local shell did not run the command")


@scenario
def remote_desktops(e):
    """VNC and RDP sign in, show the desktop and take typing."""
    stamp = str(int(time.time()))
    mark = e.log_mark()
    e.add_host("vnc", "e2e-vnc", "127.0.0.1", 25900)
    e.type("vnc-pass")
    e.key("Return", wait=4)
    e.click(700, 300)
    e.type(f"touch /tmp/vnc-{stamp}")
    e.key("Return")
    e.wait_file("muxusrs-e2e-vnc", f"/tmp/vnc-{stamp}")
    e.shot("vnc")
    e.wait_log(r"vnc session established host=127\.0\.0\.1 port=25900", mark)

    def logon(e):
        e.click(343, 321, wait=0.8)  # Logon
        e.click(*FIELD_NAME)
        e.type("rdpuser")

    e.add_host("rdp", "e2e-rdp", "127.0.0.1", 23389, then=logon)
    e.type("rdp-pass")
    e.key("Return", wait=5)
    e.shot("rdp-certificate")
    e.click(943, 640, wait=8)  # Trust certificate
    e.shot("rdp")
    e.click(500, 300)
    e.type(f"touch /tmp/rdp-{stamp}")
    e.key("Return")
    e.wait_file("muxusrs-e2e-rdp", f"/tmp/rdp-{stamp}")
    e.wait_log(r"rdp session established host=127\.0\.0\.1 port=23389", mark)


@scenario
def sftp_file_operations(e):
    """New folder, rename, delete, upload and download through the native dialogs, edit
    and save in the remote editor; each lands on the server."""
    e.exec_in("muxusrs-e2e-ssh", "rm -rf /home/alice/e2e-ops && mkdir -p /home/alice/e2e-ops && "
              "cd /home/alice/e2e-ops && echo one > a-file.txt && echo two > b-file.txt && "
              "chown -R alice:alice /home/alice/e2e-ops")
    e.relaunch(fresh=True)
    e.open_host("e2e-ssh")
    e.click(988, TOP_BAR_Y, wait=2.5)
    e.click(1240, 183)
    e.key("ctrl+a")
    e.type("/home/alice/e2e-ops")
    e.key("Return", wait=1.5)
    # New folder.
    e.click(1062, 220, wait=1)
    e.type("made-here")
    e.key("Return", wait=1.5)
    check("made-here" in e.exec_in("muxusrs-e2e-ssh", "ls /home/alice/e2e-ops"), "no new folder")
    # Rename a-file.txt (second row, after the folder) from its menu: the menu opens at
    # the pointer, items 34px apart (Open in editor, default program, Open with…,
    # Download, Rename, Delete).
    e.rclick(1100, 312, wait=0.8)
    e.shot("sftp-row-menu")
    e.click(1180, 312 + 161, wait=1)
    e.key("ctrl+a")
    e.type("renamed.txt")
    e.key("Return", wait=1.5)
    listing = e.exec_in("muxusrs-e2e-ssh", "ls /home/alice/e2e-ops")
    check("renamed.txt" in listing and "a-file.txt" not in listing, f"rename: {listing}")
    # Delete b-file.txt (now the second row).
    e.rclick(1100, 312, wait=0.8)
    e.click(1160, 312 + 195, wait=1)
    e.key("Return", wait=1.5)
    listing = e.exec_in("muxusrs-e2e-ssh", "ls /home/alice/e2e-ops")
    check("b-file.txt" not in listing, f"delete: {listing}")
    # Download renamed.txt through the save dialog.
    downloads = os.path.join(e.work.home, "Downloads")
    for name in os.listdir(downloads):
        os.unlink(os.path.join(downloads, name))
    e.click(1100, 312)
    e.click(1155, 220, wait=3)
    e.shot("sftp-save-dialog")
    e.key("Return", wait=2)
    check(os.listdir(downloads) == ["renamed.txt"], f"download: {os.listdir(downloads)}")
    # Upload through the open dialog.
    upload = os.path.join(e.work.home, "upload-me.txt")
    with open(upload, "w") as f:
        f.write("uploaded-content\n")
    e.click(1092, 220, wait=3)
    e.key("ctrl+l", wait=0.5)
    e.type(upload)
    e.key("Return", wait=3)
    check(e.exec_in("muxusrs-e2e-ssh", "cat /home/alice/e2e-ops/upload-me.txt").strip()
          == "uploaded-content", "upload did not arrive")
    # Edit and save.
    e.dclick(1100, 312, wait=3)
    e.shot("sftp-editor")
    e.click(600, 219)
    e.key("End")
    e.type(" edited")
    e.key("ctrl+s", wait=2)
    content = e.exec_in("muxusrs-e2e-ssh", "cat /home/alice/e2e-ops/renamed.txt")
    check("one edited" in content, f"save: {content!r}")


@scenario
def tunnels_carry_traffic_and_show_state(e):
    """Local, remote and SOCKS forwards carry traffic; the top bar badge counts them;
    a port in use and a server that forbids forwarding report errors; ssh-config forwards
    start with their session and stop with it."""
    import http.server
    import threading

    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):  # noqa: N802 - the handler's name
            body = b"local-web-ok\n"
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    web = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Quiet)
    threading.Thread(target=web.serve_forever, daemon=True).start()
    try:
        e.relaunch(fresh=True)
        mark = e.log_mark()
        bar_before = e.shot("tunnels-bar-empty", crop=(1000, 35, 60, 45))
        e.open_host("e2e-ssh")
        e.click(1029, TOP_BAR_Y, wait=1.5)

        def forward(kind, bind, host=None, port=None):
            e.click(1394, 275, wait=1.5)
            e.click({"local": 527, "remote": 710, "dynamic": 893}[kind], 365, wait=0.5)
            if kind == "dynamic":
                e.click(504, 566)
                e.type(str(bind))
                e.click(623, 566, wait=2)
            else:
                e.click(472, 566)
                e.type(str(bind))
                if host:
                    e.click(668, 566)
                    e.key("ctrl+a")
                    e.type(host)
                e.click(862, 566)
                e.type(str(port))
                e.click(948, 566, wait=2)

        forward("local", 18080, port=8080)
        check(e.http(18080) == "muxus-e2e-web-ok\n", "local forward carries nothing")
        e.move(700, 700)
        bar = e.shot("tunnels-bar-one", crop=(1000, 35, 60, 45))
        check(e.changed(bar_before, bar, (1000, 35, 60, 45)) > 0.02, "no badge on the top bar")
        forward("remote", 19000, host="127.0.0.1", port=web.server_address[1])
        got = e.exec_in("muxusrs-e2e-ssh", "curl -s -m 5 127.0.0.1:19000")
        check(got == "local-web-ok\n", f"remote forward: {got!r}")
        forward("dynamic", 11080)
        check(e.http(8080, proxy=11080) == "muxus-e2e-web-ok\n", "SOCKS forward carries nothing")
        e.move(700, 700)
        e.shot("tunnels-three", crop=(880, 35, 540, 400))
        e.wait_log(r"forward started .*kind=Local bind_port=18080", mark)
        e.wait_log(r"forward started .*kind=Remote bind_port=19000", mark)
        e.wait_log(r"forward started .*kind=Dynamic bind_port=11080", mark)
        # A port in use: the error shows and is logged.
        forward("local", 11080, port=8080)
        e.shot("tunnels-port-in-use")
        e.wait_log(r"WARN forward could not start .*bind_port=11080 .*Address already in use", mark)
        e.click(968, 627, wait=0.5)  # Cancel
        # The forwarding panel's own stop.
        e.click(1394, 303, wait=1.5)  # stop the first forward (-L 18080)
        check(e.http(18080, timeout=2).startswith("error"), "a stopped forward still listens")
        e.wait_log(r"forward stopped .*bind_port=18080", mark)
    finally:
        web.shutdown()


@scenario
def tunnels_denied_and_from_ssh_config(e):
    e.relaunch(fresh=True)
    mark = e.log_mark()
    e.open_host("e2e-cfgfwd", wait=4)
    check(e.http(18090) == "muxus-e2e-web-ok\n", "LocalForward from ssh config carries nothing")
    check(e.http(8080, proxy=11081) == "muxus-e2e-web-ok\n", "DynamicForward from ssh config")
    e.click(1029, TOP_BAR_Y, wait=1.5)
    e.move(700, 700)
    e.shot("tunnels-config", crop=(880, 35, 540, 400))
    # Closing the tab ends the session's forwards.
    e.click(478, 104, wait=1.5)
    e.key("Return", wait=2)
    check(e.http(18090, timeout=2).startswith("error"), "config forward outlived its session")
    e.wait_log(r"terminal session closed", mark)
    # A server that forbids forwarding: the remote bind fails with ssh2's words. (The
    # panel lists live connections as it polls them, every 5 s as the client does.)
    e.open_host("e2e-nofwd", wait=6)
    e.click(1394, 275, wait=1.5)
    e.click(710, 365, wait=0.5)
    e.click(472, 566)
    e.type("19001")
    e.click(862, 566)
    e.type("8080")
    e.click(948, 566, wait=2)
    e.shot("tunnels-denied")
    e.wait_log(r"forward could not start .*remote bind failed: Unable to bind to 127\.0\.0\.1:19001", mark)


SCENARIO_NAMES = None


def run_all(work, state, only=None):
    e = E2E(work, state)
    results = []
    for fn in SCENARIOS:
        if only and fn.__name__ not in only:
            continue
        started = time.time()
        try:
            fn(e)
            results.append((fn.__name__, "ok", time.time() - started, ""))
        except Exception as error:  # noqa: BLE001 - one failure must not stop the run
            detail = "".join(traceback.format_exception_only(type(error), error)).strip()
            e.shot(f"FAILED-{fn.__name__}")
            results.append((fn.__name__, "FAILED", time.time() - started, detail))
        status = results[-1]
        print(f"{status[1]:6} {status[0]:42} {status[2]:6.1f}s {status[3]}", flush=True)
    failed = [r for r in results if r[1] != "ok"]
    print(f"\n{len(results) - len(failed)} passed, {len(failed)} failed")
    with open(os.path.join(work.path, "out", "results.json"), "w") as f:
        json.dump(results, f, indent=2)
    return 1 if failed else 0


if __name__ == "__main__":
    import argparse

    from run import DEFAULT_WORK, Work

    parser = argparse.ArgumentParser()
    parser.add_argument("--work", default=DEFAULT_WORK)
    args = parser.parse_args()
    work = Work(args.work)
    e = E2E(work, work.state())
    print("attached: e = E2E(...)")
