# Parity: management

The network-device management workbench: gNMI, gNOI, gNSI and NETCONF. The
backend (`server/src/management`, the management route) and all the non-UI
logic of the workbench (`client/src/management`) live in one crate.

Crate: `muxus-mgmt` (L2, depends on `muxus-core` only). Tests:
`cargo test -p muxus-mgmt` (94 tests: 58 unit, 36 integration against
in-process fake devices: a tonic gNMI server, a raw HTTP/2 gRPC target, a
TLS target with a self-signed certificate, framing-level NETCONF servers and
fake devices for the explorer).

## Status

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| gNMI protobuf messages | `server/src/management/gnmi/protobuf.ts`, `gnmi/schema.ts` | done | prost types generated from the vendored `gnmi.proto`/`gnmi_ext.proto` with `protox` (no `protoc`). Map fields are compiled as ordered `…Entry` lists so path keys keep the device's order. |
| gNMI values (TypedValue, paths, notifications, encodings, Set values) | `gnmi/values.ts` | done | 64-bit integers stay exact (numbers up to 2^53, strings beyond), decimals, leaf-lists, legacy `Update.value`, pre-0.4 string paths. |
| gRPC over a Muxus-owned stream | `gnmi/grpc.ts` | done | hyper HTTP/2 client (32 MiB windows, 30 s keepalive, 20 s timeout) with tonic framing; credentials as metadata; `grpc-timeout`; response byte counting and the `maxBytes` limit with the TS message; non-200 answers mapped to gRPC codes. The frame-decoder unit tests are tonic's concern now. A compressed message is refused with tonic's wording. Ping round trip (`latencyMs`) is not measured; nothing read it. |
| gNMI client (Capabilities, Get with depth, Set with commit-confirmed, Subscribe with poll/cancel) | `gnmi/client.ts` | done | Embedded pre-0.7 errors rejected; first Subscribe request validated before anything is sent. |
| TLS with pinning, SNI rules, ALPN h2, mutual TLS, CA files | `tls.ts`, `remote-desktop/certificates.ts` | done | rustls (ring). Verification is recorded, not enforced, so the session decides (verify/pin, skip-verify, plaintext). Verification errors use Node's words (`DEPTH_ZERO_SELF_SIGNED_CERT`, `CERT_HAS_EXPIRED`, hostname mismatch); rarer OpenSSL codes fall back to rustls names. Dates in OpenSSL form. |
| NETCONF framing (EOM and chunked, size limit, whitespace tolerance) | `netconf/framing.ts` | done | Linear on large replies in tiny packets. |
| NETCONF XML helpers (root element, hello, entities) | `netconf/xml.ts` | done | |
| NETCONF client (hello, base 1.1 negotiation, message ids, out-of-order replies, notifications, timeout, cancel, close-session) | `netconf/client.ts` | done | Runs on any `NetconfChannel` (AsyncRead + AsyncWrite); the SSH side opens it. Bytes that followed the hello in the same packet are decoded right away (TS waited for the next packet). |
| gNOI/gNSI method registry | `services/registry.ts` | done | The allowed paths are `muxus_core::management::grpc_methods::ALL`; kinds and message types come from the vendored service definitions. |
| gNOI/gNSI JSON mapping | `services/json.ts` | done | Over prost-reflect descriptors, same rules: proto field names, enums by name, base64 bytes, exact 64-bit values, unknown keys refused, Timestamp/Any as plain messages, maps as ordered objects. Uses the full upstream messages (the TS had hand-picked subsets), so a few more fields are accepted. |
| gNOI file download (hash checked: SHA-256/512, MD5) and upload (64 KiB pieces, backpressure, SHA-256 trailer) | `services/transfer.ts` | done | |
| Service discovery through gRPC reflection (v1alpha, then v1) | `services/transfer.ts` | done | |
| Management session (connect, login with retries and remember, certificate challenge and pinning, requests, subscriptions with batching, gNOI/gNSI streams, uploads with acks, downloads in 1 MiB chunks, TLS probe, cancel, lost transport) | `management-socket.ts` | done | Channels instead of a WebSocket: `ManagementClientMessage` in, `ManagementServerMessage` out. Store, vault and SSH come in through `ManagementEnv`; see "Needs". |
| Password account names and labels | `security/password-vault.ts` (`desktopPasswordAccount`, `desktopPasswordLabel`) | done | Same keyring account strings as the TS. |
| Vault unlock/create prompts | `remote-desktop/desktop-passwords.ts` | partial | The session calls `PasswordVault::read/remember` with a `SessionIo` for prompts; the unlock/create dialogue itself belongs to the vault implementation (store/session). |
| Dial errors | `util/dial.ts` | done | `connect_tcp` (15 s, nodelay, keepalive) and the one-sentence failure messages; `DialError::from_channel_open` for the SSH gateway side. |
| Saved request library (validation, size limit, list/save/delete) | `routes/management-requests.ts` | done | Over a `ManagementRequestStore` trait; the database part is the store's. |
| Request drafts, RPC bodies, Set/Subscribe messages, draft problems, summaries, operation menu | `client/src/management/requests.ts` | done | Drafts serialize to the library's JSON shape (numbers written like JavaScript). |
| Data tree (JSON and XML merge, key guessing and learning, module prefixes only where they change) | `data-tree.ts` | done | Nodes are found by id; the tree is an owned value. |
| Live telemetry table (flattening, counters vs gauges, rates, history, deletes, SI formatting) | `live-table.ts` | done | |
| NETCONF XML in the UI (parse, pretty print, rpc-reply, unwrap pasted rpc, subtree/XPath filters, edit-config skeletons) | `xml.ts` | done | A small DOM over quick-xml that keeps names as written and resolves namespaces; `XMLSerializer` behaviour reproduced. Parser error texts differ from the browser's. |
| Set review (gNMI merge semantics, canonical JSON) | `set-review.ts` | done | |
| Copy as gnmic / ncclient / ssh | `copy-as.ts` | done | |
| gNOI/gNSI tool formats (permissions, authorized keys, accounting records) | `tool-format.ts` | done | |
| Workbench store (drafts, history of 100 runs, explorer state, candidate, locks, tools) | `workbench-store.ts` | done | `WorkbenchStore` = state behind a lock plus a watchable revision. |
| Session client (requests, subscriptions, gRPC streams, uploads, downloads, close) | `session-client.ts` | done | `ManagementConnection`. |
| Workbench controller (runs, commit-confirmed, explorer with depth retries 2→3→4→0 and limits, key discovery, NETCONF explorer, refresh after changes, locks, candidate check) | `controller.ts` | done | Generic over `WorkbenchTransport`, so tests drive it with fake devices. |
| gNOI File tool paths (SR OS `cf3:\`, 777 permissions, breadcrumbs, stat entries) | `components/management/tools/FilesTool.tsx` (helpers) | done | `workbench::files`. |
| Other pure helpers inside the React components | `components/management/*` (`PathField` suggestions, `DataTreeView.visibleRows`, `NetconfForm.helloModules`, `LiveView.commonPrefix`, `ResultPane` hints/quick starts, `ReviewDialog.lineDelta`, `tools/common` time formats and `gnoiPath`, `rotation.tsx`, `CertzTool` PEM handling, `PathzTool` drafts) | missing | Left for the UI wave with the components. |
| Live check against SR Linux 26.3.1 (containerlab, gNMI over TLS) | n/a | done | `tests/live_device.rs` (ignored unless `MUXUS_MGMT_LIVE_HOST`/`_PASSWORD` are set): TLS with a lab CA (`UNABLE_TO_GET_ISSUER_CERT_LOCALLY`), Capabilities (362 models), reflection (19 services), depth-2 Get, SAMPLE stream (first update right away here, then every second), gNOI Time, and a full session with login plus the explorer loading the top level and an interface entry. NETCONF was not checked live (needs the SSH crate). |

## Device quirks kept

- The gNMI depth extension returns only leaves at its last level: the
  explorer looks again at depths 3, 4 and 0 (everything) when a look shows
  nothing, adds the branches each deeper look reveals, and stops past 4 MiB
  (24 MiB for the first look) or when a large look adds nothing.
- Devices that reject the depth extension switch the explorer to whole subtrees.
- SR OS rejects module prefixes in gNMI paths: tree paths keep a prefix only
  on the first element and where the module changes from the parent's.
- gNOI File paths may be `cf3:\file` (SR OS), and Put accepts only 555/777
  there; the File helpers start at `cf3:\` with 777 when the device lists
  `nokia-conf`.
- SAMPLE subscriptions deliver their first update after one interval; the
  subscription stays open and batches updates every 100 ms.

## Public API (for the next wave)

- `session::ManagementSession::start(env) -> (Sender<ManagementClientMessage>, Receiver<ManagementServerMessage>)`
  runs one tab's session on the current tokio runtime. `ManagementEnv`
  (all methods have defaults): `saved_profile`, `record_connection`,
  `trusted_identity`/`trust_identity`, `passwords() -> PasswordVault`,
  `connect_tcp`, `connect_ssh_gateway(gateway, SessionIo) -> SshLease`,
  `connect_netconf(profile, SessionIo) -> SshLease`. `SshLease`:
  `open_direct_tcpip`, `open_subsystem("netconf")`, `address`, `closed`.
  `SessionIo` routes SSH status lines, auth prompts and host-key questions
  through the session.
- `workbench::connection::ManagementConnection::new(sender, profile)` plus
  `dispatch(receiver, events)` (or `receive`/`finish` by hand): the UI end,
  with `ConnectionEvent`s for login round-trips, `request`, `subscribe`,
  `grpc_call`, `grpc_stream`, `download_file`, `upload_file`, `close`.
- `workbench::controller::WorkbenchController::new(store, connection, profile, info, runtime)`:
  `run`, `request`, `cancel`, `poll`, `finish_gnmi_commit`, `load_node`,
  `refresh_explorer`, `check_candidate`, `detach`, `has_service`, `has_capability`.
- `workbench::store::WorkbenchStore` (`read`, `update`, `watch`) with
  `WorkbenchState`; `workbench::{requests, data_tree, live_table, xml, set_review, copy_as, tool_format, files}` are pure.
- Lower level, usable on their own: `gnmi::GnmiClient`, `netconf::NetconfClient`,
  `tls::upgrade_gnmi_tls`, `services::{json, registry, transfer}`,
  `library::{list_requests, save_request, delete_request}`.

## Needs from other crates

- `muxus-session` implements `ManagementEnv` and starts sessions on its runtime.
- `muxus-ssh` provides what `SshLease` wraps: a direct-tcpip channel (map
  open failures with `DialError::from_channel_open`) and an SSH subsystem
  channel as `AsyncRead + AsyncWrite`; NETCONF connects like
  `ssh -s host netconf` (ssh_config, user, port, the gateway as ProxyJump).
- `muxus-store`: saved host lookup and "recent connection", pinned desktop
  identities by (host, port, gateway), the password vault behind
  `PasswordVault`, and `ManagementRequestStore` for the library.

## Attribution

- `crates/muxus-mgmt/proto/` vendors protos under the Apache License 2.0
  (text in `proto/LICENSE-APACHE-2.0`): openconfig/gnmi (gnmi.proto,
  gnmi_ext.proto; commit a4e40e6), openconfig/gnoi (types, common, system,
  file, healthz, bgp, os; commit b56005d), openconfig/gnsi (authz, pathz,
  certz, credentialz, acctz; commit 80ac586), grpc/grpc-proto (reflection
  v1 and v1alpha; commit 8133308). `gnoi/os/os.proto` is modified: the
  bootconfig import and `ActivateRequest.boot_config` are removed (marked in
  the file).
