#!/usr/bin/env python3
"""Collect the license texts of every crate linked into the muxus binary.

Walks `cargo metadata` from muxus-app through its normal (not dev or build) dependencies for
one target, and writes, per package, its name, version, declared license and the license and
notice files it ships. Packages that ship no file are listed with their declared license.

    packaging/third_party_licenses.py OUTPUT [--target x86_64-unknown-linux-gnu]
"""

import argparse
import json
import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
LICENSE_PREFIXES = ("license", "licence", "copying", "notice", "copyright", "unlicense")


def metadata(target):
    output = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--offline", "--filter-platform", target],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(output)


def linked_packages(meta):
    packages = {package["id"]: package for package in meta["packages"]}
    nodes = {node["id"]: node for node in meta["resolve"]["nodes"]}
    start = next(p["id"] for p in meta["packages"] if p["name"] == "muxus-app")
    seen, stack = set(), [start]
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        for dep in nodes[current]["deps"]:
            # A dependency counts when some edge to it is a normal one.
            if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    workspace = set(meta["workspace_members"])
    return sorted(
        (packages[pid] for pid in seen if pid not in workspace),
        key=lambda package: (package["name"].lower(), package["version"]),
    )


def license_files(package):
    folder = pathlib.Path(package["manifest_path"]).parent
    found = []
    for entry in sorted(folder.iterdir()):
        name = entry.name.lower()
        if entry.is_file() and name.startswith(LICENSE_PREFIXES):
            found.append(entry)
    if package.get("license_file"):
        extra = (folder / package["license_file"]).resolve()
        if extra.is_file() and extra not in found:
            found.append(extra)
    return found


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("output")
    parser.add_argument("--target", default="x86_64-unknown-linux-gnu")
    args = parser.parse_args()
    packages = linked_packages(metadata(args.target))
    lines = [
        "Third-party software in Muxus",
        "=============================",
        "",
        "The muxus program is built from the Rust crates below. Each is listed with the",
        "license it declares and the license and notice files it ships.",
        "",
    ]
    # Many crates ship the same license text; each distinct text is written once.
    first_seen = {}
    for package in packages:
        label = f"{package['name']} {package['version']}"
        lines.append("-" * 78)
        lines.append(label)
        lines.append(f"License: {package.get('license') or 'see the files below'}")
        if package.get("repository"):
            lines.append(f"Source: {package['repository']}")
        for file in license_files(package):
            text = file.read_text(encoding="utf-8", errors="replace").rstrip()
            key = " ".join(text.split())
            lines.append("")
            if key in first_seen:
                lines.append(f"--- {file.name}: the same text as {first_seen[key]} ---")
                continue
            first_seen[key] = f"{file.name} of {label}"
            lines.append(f"--- {file.name} ---")
            lines.append(text)
        lines.append("")
    out = pathlib.Path(args.output)
    out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{len(packages)} packages -> {out}", file=sys.stderr)


if __name__ == "__main__":
    os.environ.setdefault("CARGO_TERM_COLOR", "never")
    main()
