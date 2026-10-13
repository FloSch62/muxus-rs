#!/usr/bin/env python3
"""Screenshot regression set: every scene the feature examples and the app render, light
and dark, captured one at a time in a headless GNOME Shell (hack/shot.sh), and compared
with an earlier capture of the same set.

    hack/regress.py capture <out-dir> [--bin DIR] [--app BINARY] [--only SUBSTRING]
    hack/regress.py compare <before-dir> <after-dir> [--threshold 0.5]

`--bin` is the folder holding the example binaries (default target/debug/examples), `--app`
the main binary (default target/debug/muxus). `compare` prints the share of device pixels
that differ by more than 24/255 for every scene present in both folders, and marks those
above the threshold (percentage points). The scenes are the ones the parity files report
(`docs/parity/*.md`); their own crops and references live with each area's notes.
"""

import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

# (name, binary, environment, shot.sh options)
SCENES = []


def add(name, binary, env=(), opts=()):
    SCENES.append((name, binary, list(env), list(opts)))


for section in ["appearance", "terminal", "local-shells", "logging", "highlighting", "behavior",
                "x11", "keyboard", "passwords", "data", "debug", "about"]:
    add(f"settings-{section}", "settings", [f"MUXUS_SETTINGS_SECTION={section}"], ["--size", "1000x720"])
add("settings-shortcuts", "settings", ["MUXUS_SETTINGS_SECTION=keyboard", "MUXUS_SETTINGS_SHORTCUTS=1"],
    ["--size", "1000x720"])
for scene in ["general", "route", "forwards", "advanced", "auth", "appearance", "login", "logging",
              "highlighting", "serial", "telnet", "rdp", "vnc", "new"]:
    add(f"editor-{scene}", "host_editor", [f"MUXUS_EDITOR_SCENE={scene}"])
for scene in ["launch", "organize", "folder", "folder-new", "move", "bulk", "tunnel", "tunnel-edit"]:
    add(f"dialog-{scene}", "host_dialogs", [f"MUXUS_DIALOG_SCENE={scene}"])
add("launcher", "launcher", ["TZ=UTC"])
add("launcher-empty", "launcher", ["TZ=UTC", "MUXUS_LAUNCHER_QUERY="])
add("host-picker", "launcher", ["TZ=UTC", "MUXUS_LAUNCHER_SCENE=picker"])
for scene in ["catalog", "active", "menu", "save-as"]:
    add(f"workspaces-{scene}", "workspaces", ["TZ=UTC", f"MUXUS_WORKSPACES_SCENE={scene}"])
add("history", "history", ["TZ=UTC"])
for scene in ["bar", "group-menu", "menu", "dialog", "multi-exec", "multi-exec-off"]:
    add(f"commands-{scene}", "command_buttons", ["TZ=UTC", f"MUXUS_COMMAND_SCENE={scene}"])
add("log-viewer", "logs")
for scene in ["mobaxterm", "securecrt", "review"]:
    add(f"import-{scene}", "import", [f"MUXUS_SCENE={scene}"])
add("vault-secret", "vault", ["MUXUS_SCENE=secret"])
add("send-secret", "vault", ["MUXUS_SCENE=send", "MUXUS_AT=368,125"])
add("startup-unlock", "vault", ["MUXUS_SCENE=unlock"])
add("update", "notices", ["MUXUS_SCENE=update"])
add("rename-tab", "notices", ["MUXUS_SCENE=rename"])
for scene in ["panel", "menu", "adhoc"]:
    add(f"forwarding-{scene}", "forwarding", [f"MUXUS_SCENE={scene}"])
for scene in ["sftp", "remote-editor", "sidebar", "window"]:
    add(f"sftp-{scene}", "sftp", [f"MUXUS_SCENE={scene}"])
for scene in ["workbench", "get", "table", "raw", "history", "device", "set", "review", "operations",
              "security", "error"]:
    add(f"mgmt-{scene}", "management", [f"MGMT_SCENE={scene}"], ["--wait", "24"])
add("remote-desktop", "remote_desktop")
for scene in ["overview", "panes", "local", "localsplit", "emptytab"]:
    add(f"app-{scene}", "@app", [f"MUXUS_SCENE={scene}"], ["--wait", "11"])


def capture(out, bin_dir, app, only):
    os.makedirs(out, exist_ok=True)
    for name, binary, env, opts in SCENES:
        if only and only not in name:
            continue
        program = app if binary == "@app" else os.path.join(bin_dir, binary)
        for theme in ["light", "dark"]:
            path = os.path.join(out, f"{name}{'-dark' if theme == 'dark' else ''}.png")
            args = [os.path.join(HERE, "shot.sh"), "--name", "regress", "--wait", "14",
                    "--env", "MUXUS_WINDOW=fullscreen", "--env", f"MUXUS_THEME={theme}",
                    "--step", "key:42", "--step", "wait:1"]
            if theme == "dark":
                args.append("--dark")
            data = tempfile.mkdtemp(prefix="mx-regress-")
            args += ["--env", f"MUXUS_DATA_DIR={data}"]
            for kv in env:
                args += ["--env", kv]
            args += opts
            args += [path, "--", program]
            result = subprocess.run(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                    timeout=300)
            subprocess.run(["rm", "-rf", data])
            print(f"{'ok' if result.returncode == 0 and os.path.exists(path) else 'FAILED'} {name}-{theme}",
                  flush=True)


def compare(before, after, threshold):
    worst = []
    for file in sorted(os.listdir(after)):
        if not file.endswith(".png") or not os.path.exists(os.path.join(before, file)):
            continue
        result = subprocess.run(["python3", "-I", os.path.join(HERE, "compare.py"),
                                 os.path.join(after, file), os.path.join(before, file),
                                 os.path.join(after, "cmp-" + file[:-4])],
                                capture_output=True, text=True)
        line = next((l for l in result.stdout.splitlines() if l.startswith("differing")), "")
        try:
            share = float(line.split("(")[1].split("%")[0])
        except (IndexError, ValueError):
            share = -1.0
        mark = " <<" if share > threshold else ""
        print(f"{file[:-4]:32} {share:6.2f}%{mark}")
        worst.append((share, file))
    worst.sort(reverse=True)
    print("largest:", ", ".join(f"{f[:-4]} {s:.2f}%" for s, f in worst[:5]))


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(2)
    command = sys.argv[1]
    if command == "capture":
        out = sys.argv[2]
        bin_dir = os.path.join(ROOT, "target", "debug", "examples")
        app = os.path.join(ROOT, "target", "debug", "muxus")
        only = ""
        rest = sys.argv[3:]
        while rest:
            flag, value, rest = rest[0], rest[1], rest[2:]
            if flag == "--bin":
                bin_dir = value
            elif flag == "--app":
                app = value
            elif flag == "--only":
                only = value
        capture(os.path.abspath(out), os.path.abspath(bin_dir), os.path.abspath(app), only)
    elif command == "compare":
        threshold = 0.5
        if len(sys.argv) > 5 and sys.argv[4] == "--threshold":
            threshold = float(sys.argv[5])
        compare(sys.argv[2], sys.argv[3], threshold)
    else:
        print(__doc__)
        sys.exit(2)


if __name__ == "__main__":
    main()
