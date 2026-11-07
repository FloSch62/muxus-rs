# Parity: containers, clusters and VMs on your hosts

Upstream commits `bff978f` (show containers, clusters and VMs on hosts), `1345c99` (refresh
them, add them to the sidebar) and `693ee67` (one logo mark in the sidebar). User guide:
`../muxus/docs/guide/environments.md`.

- Backend: `crates/muxus-session/src/environments/` (`parse.rs`, `probe.rs`, `discovery.rs`)
  and the `Backend` methods in `crates/muxus-session/src/routes/environments.rs`
  (`environments`, `environment_namespaces`, `environment_pods`, `environment_topology`,
  `environment_libvirt_addresses`, `environment_libvirt_display`,
  `environment_proxmox_addresses`, `environment_action`, `environment_port`,
  `kubus_available`).
- App: `crates/muxus-app/src/features/environments/` (logic, hub, panel, topology, logos) and
  the window wiring in `crates/muxus-app/src/ui/environments.rs`.
- Data (already in `76fd879`): `muxus_core::environments`, `environment` on local and SSH
  profiles, the host field `discover_environments`, prefs `discoverEnvironments`,
  `environmentRefreshSeconds`, `environmentsPanelWidth`.

```bash
cargo test -p muxus-session --lib environments        # parsers, probe script, discovery, actions
cargo test -p muxus-session --test environments       # route validation, missing connection
cargo test -p muxus-ssh --lib environment             # environment sessions over SSH
cargo test -p muxus-local --test pty_manager          # local environment sessions
cargo test -p muxus-app --lib environments            # client logic (environments, sessions, hosts, launcher)
cargo test -p muxus-app --test ui_environments        # hub + panel against a live SSH connection
```

## Backend

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Parsers (docker/podman ps, ports, inspect, labs, kubectl, pvesh, virsh, guest agent, lxc-info) | server/src/environments/parse.ts | done | tests ported |
| Probe script, output parsing, failure sentence | server/src/environments/probe-runner.ts | done | one `sh -s` batch per host; tests ported |
| Remote runner over the live connection | probe-runner.ts `remoteProbeRunner` | done | `ConnectionCommand` exec channel, the path the status bar statistics use |
| Local runner | probe-runner.ts `localProbeRunner` | done | runs in the backend's local terminal environment (login shell PATH; sandbox HOME in tests); Windows runs each probe on its own |
| Discovery (podman-docker once, lab addresses, libvirt system + session, problems) | server/src/environments/discovery.ts | done | tests ported |
| Lookups: namespaces, pods, lab topology, libvirt/Proxmox addresses, VNC display | discovery.ts | done | |
| Lifecycle actions (container, compose, Proxmox, libvirt) | discovery.ts `actionArgv` | done | tests ported |
| Routes, validation, shared in-flight reading | server/src/routes/environments.ts | done | `Backend` methods; route tests ported |
| Automatic discovery skips hosts that never ran a command | routes/environments.ts, ssh/remote-shell-integration.ts `ranCommand` | done | `ManagedConnection::ran_command` (the shell probe's `ShellProbeMemo`, set when it printed its marker) |
| Published port through an SSH forward (reused when one exists) | routes/environments.ts `/ports` | done | shows in the forwarding panel |
| Environment sessions over SSH (target command, forced TTY) | ssh/connection-manager.ts `environmentSession` | done | test ported |
| Saved hosts keep the session's environment | connection-manager.ts `resolveProfile` | done | test ported |
| Local environment sessions (tool argv, no shell integration) | local/pty-manager.ts | done | test added |
| No login sequence for environment sessions | ws/terminal-socket.ts | done | |
| Kubus detection and `kubus://` links | electron/src/main.ts | done | `xdg-mime` on Linux, `/Applications/Kubus.app` on macOS, the registry on Windows; route check tested |
| Discovery over system OpenSSH (over the login the terminal shares, after the command probe; no ports) | routes/environments.ts `systemOpenSsh`, probe-runner.ts `scriptProbeRunner` | done | `probe::script_probe_runner` over `SystemOpenSshSession::run_script`; automatic readings ask `runs_commands` first; `environment_port` refuses such sessions |

## App

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Hosts, entries, store, live transports, forgetting disconnected hosts; system OpenSSH tabs without a shared login are skipped | client/src/environments.ts (`tabRunsCommands`), state/environments.ts | done | `model.rs`; tests ported |
| Badges (counts) | environments.ts `environmentBadges` | done | `EnvironmentCount` (a `…Badge` type name reads as a widget to `xtask style`) |
| Discovery on window open, on connect, on the refresh interval; host override | components/environments/useEnvironmentDiscovery.ts | done | `hub.rs`, one per window; the shell feeds it tabs and prefs every frame (cheap when unchanged) and the catalog when loaded |
| Panel dials a host without a terminal, prompts through the window, lets go on close | environments.ts `environmentSource`, EnvironmentsPanel.tsx | done | prompts reuse the window's auth/host key dialogs |
| Session profiles (container, pod, logs, consoles, SSH through the host, VNC through the host, lab nodes) | client/src/environment-sessions.ts | done | `sessions.rs`; tests ported |
| Add to hosts (enter draft, jump draft, local terminal profile) | client/src/environment-hosts.ts | done | `hosts.rs`; tests ported; the host editor opens with `NewHost::draft` |
| Quick launcher entries, "Show containers & VMs", names are not SSH targets | environment-launcher.ts, QuickLauncherDialog.tsx | done | tests ported; logo as the row icon |
| Panel: header (tab-strip height), All hosts, read again, widen, close | EnvironmentsPanel.tsx | done | |
| Panel: filter and Stopped toggle | EnvironmentsPanel.tsx | done | |
| Panel: resize (drag, double-click resets), width pref | EnvironmentsPanel.tsx, PanelResizeHandle | done | |
| Host cards: tools, status, age, problems, Connect, Remove from this list | EnvironmentsPanel.tsx `HostCard` | done | |
| Tree lines: chevron, icon, detail, status, hover actions, hover details | EnvironmentTree.tsx | done | details show as a tooltip card whenever a line has them (the TS also shows them for clipped text) |
| Docker/Podman: compose groups, shells, logs, lifecycle with confirmation, port chips, Add to hosts, copy exec | EnvironmentSections.tsx | done | |
| Containerlab: labs, nodes, SSH as admin through the host, shell, logs, Add to hosts | EnvironmentSections.tsx | done | |
| Kubernetes: contexts, namespaces, pods by workload, shells, logs, previous logs, Kubus | KubernetesSection.tsx | done | namespace picker is a menu instead of a select |
| Proxmox: guests, pct enter / qm terminal, SSH via guest agent, web UI, power, Add to hosts | EnvironmentSections.tsx | done | |
| libvirt: domains, serial console, VNC through the hypervisor, SSH, power, Add to hosts | EnvironmentSections.tsx | done | |
| Lab topology | ClabTopologyDialog.tsx | partial | drawn natively from the `.clab.yml` (nodes, links) and saved annotation positions; the TS frames the containerlab web viewer, which needs a web view |
| Logos in the tools' own colors (dark containerlab variant) | environment-icons.tsx, logos/*.svg | done | `assets/icons/env-*.svg`, copied by `hack/extract_icons.mjs`, rasterized per size and scale |
| Sidebar mark (overlapped logos, counts in tooltip, toggles the panel) on hosts and the local terminal row | EnvironmentBadges.tsx, HostRow.tsx, SessionSidebar.tsx | done | |
| Options button takes no width until the row is hovered | HostRow.tsx (`693ee67`) | done | |
| Host menu "Containers & VMs" (greyed for a system OpenSSH host on Windows, or without a connected terminal that can run commands) | sidebar/HostContextMenu.tsx | done | the window's hosts with such a terminal reach the sidebar with its marks (`SidebarMarks::live`) |
| Top bar control | layout/TopBar.tsx | done | |
| Panel beside the sidebar, hidden in focus mode | layout/AppShell.tsx | done | |
| Tab icons of environment sessions | components/host-kind-icon.ts `environmentTabIcon` | done | |
| Settings → Behavior: Find containers…, Refresh … | SettingsDialog.tsx | done | |
| Host editor → Advanced → Find containers, clusters and VMs (with count) | host-editor/AdvancedSection.tsx, HostEditorDialog.tsx | done | the TS bulk editor has no such field, so neither does this one |
| Greying out actions for system OpenSSH hosts | EnvironmentSections.tsx (`9b1aaf9`) | done | follows the host's `systemOpenSsh` metadata |
