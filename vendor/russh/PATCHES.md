# Muxus patches to russh

This is [russh](https://github.com/warp-tech/russh) 0.64.1 as published on crates.io
(upstream commit `e5d80a07dc554480b6ea3b2e14adfc190407b627`), patched into the workspace
through `[patch.crates-io]` in the root `Cargo.toml`. russh is licensed under the Apache
License 2.0 (`LICENSE`; the crates.io package ships no license file, so the stock text is
added here). Everything outside the files listed below is unchanged; each changed file
carries a notice in its first line and a `Muxus patch` comment at every change.

## RFC 4335 `break`

Muxus' Send BREAK reaches network gear behind console servers over SSH
(`server/src/ssh/channel-break.ts` in the TypeScript app). russh 0.64 has no client API for
the `break` channel request and no way to send a raw channel request, so the request is
added the way russh sends its other channel requests:

| File | Change |
|---|---|
| `src/channels/mod.rs` | `ChannelMsg::Break { want_reply, break_length_ms }`; `Channel::request_break` and `ChannelWriteHalf::request_break`. |
| `src/client/session.rs` | `Session::request_break` writes `SSH_MSG_CHANNEL_REQUEST "break"` with `uint32 break-length`. |
| `src/client/mod.rs` | Dispatches `ChannelMsg::Break` to `Session::request_break` (outbound channel messages wait for a running key exchange, as all others do). |
| `src/server/encrypted.rs` | Decodes an incoming `break`, forwards it as `ChannelMsg::Break` and calls `Handler::break_request` (it used to fall through to "unknown channel request"). |
| `src/server/mod.rs` | `Handler::break_request`; the default refuses with `SSH_MSG_CHANNEL_FAILURE`, the answer an unknown request got before. |

The answer to a want-reply `break` arrives on the channel as `ChannelMsg::Success` or
`ChannelMsg::Failure`, like the replies to `pty-req`, `shell` and `exec`; replies come in the
order the requests were sent (RFC 4254 section 5.4), so the caller pairs them itself
(`crates/muxus-ssh/src/shell.rs`). The server half lets the in-process test server of
`muxus-ssh` accept, refuse or ignore a `break`.

## Updating

Copy the new release from `~/.cargo/registry/src/*/russh-<version>` over this folder, keep
`LICENSE` and this file, re-apply the changes above (search for `Muxus patch`) and update the
version here and in `ATTRIBUTION.md`. Drop the patch once upstream russh can send `break`.
