# Password managers parity (`crates/muxus-passwords`)

Port of `server/src/password-managers`: login passwords read from KeePass, the `pass` store,
1Password, Bitwarden, Proton Pass, LastPass and Dashlane. The crate depends on `muxus-core`
only. The database its sources are kept in comes in as the `SourceStore` trait, and a login's
status lines and prompts as `PasswordManagerIo`; `muxus-session` implements both.

Nothing here writes to a manager. A password is handed to the login that asked for it and
kept nowhere; hosts and folders store a reference to an entry.

```bash
cargo test -p muxus-passwords
# What KeePassXC, pass and gopass write themselves (needs gpg here):
docker build -t muxusrs-e2e-passwords hack/e2e/containers/passwords
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD/target/e2e-passwords:/out" muxusrs-e2e-passwords
MUXUS_E2E_PASSWORDS=$PWD/target/e2e-passwords cargo test -p muxus-passwords --test tools -- --ignored
```

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| Provider contract, problems, entry order | `provider.ts` | done | `PasswordManagerProvider` (async trait; `can_unlock` stands for the optional `unlock`), `PasswordManagerError { problem, message }`, `sort_entries` |
| Running a manager's program (no shell, closed stdin, timeout and kill, output limit, PATH and PATHEXT lookup, last-line messages) | `cli.ts` | done | `run_cli`, `resolve_program`, `cli_message`. The environment a program gets is the process's with a `HostEnv` laid over it, so nothing sets process variables (tests point `GNUPGHOME` at their own key ring that way) |
| KeePass (.kdbx 3.1 and 4, AES-KDF and Argon2, key files, recycle bin left out, `{REF:P@I:…}` and `{REF:U@I:…}`, reread after a save, password changed in KeePass) | `keepass.ts` | done | The `keepass` crate reads the file; the key is derived on the blocking pool. Entry IDs are the UUID in base64, as kdbxweb names them, so a host linked in the Electron build keeps its entry. `password-managers-keepass.test.ts` ported against the same two databases |
| pass / gopass (store walk to depth 12, hidden names left out, `gpg --batch --decrypt`, first line) | `pass.ts` | done | |
| 1Password (`op`: accounts, item list, `op read` by vault and item ID, service-account token) | `onepassword.ts` | done | |
| Bitwarden and Vaultwarden (`bw`: status, unlock through `--passwordenv`, session key in the environment only, lock, sync, list, get) | `bitwarden.ts` | done | |
| Proton Pass (`pass-cli`: vault by vault, share or vault-name references) | `protonpass.ts` | done | |
| LastPass (`lpass`: status, ls with a field separator, show by numeric ID, no pinentry) | `lastpass.ts` | done | |
| Dashlane (`dcli`: status, password list, `dl://` reads by UUID) | `dashlane.ts` | done | |
| Connected managers (one provider per source for as long as its settings stand, list with states, installed kinds, default names, unlock, lock, entries, `passwordFor` with the unlock prompt and three attempts) | `password-managers.ts` | done | `PasswordManagers`; sources are described concurrently |

## Tests

- `tests/keepass.rs`: `password-managers-keepass.test.ts`, plus the entry IDs the TypeScript
  build stored and `~` in the database and key file paths.
- `tests/cli.rs`: `password-managers-cli.test.ts`. The stand-in programs are shell scripts
  (the TypeScript ones are Node scripts); the `pass` cases run `gpg` with a key ring of
  their own and are skipped without it.
- `tests/managers.rs`: the registry and `password_for` against the KeePassXC database, with
  sources kept in memory.
- `tests/tools.rs` (ignored unless asked for): a KeePassXC 2.7.10 database opened with a
  password and a key file, one opened with a key file alone, and stores made by `pass`
  1.7.4 and `gopass` 1.15.16, all written in the `hack/e2e/containers/passwords` container.
- The login itself is tested where the layers meet: `muxus-ssh`
  (`connection/tests/linked_passwords.rs`), `muxus-session` (`tests/password_managers.rs`,
  the VNC cases of `desktop/tests.rs`) and `muxus-mgmt` (`tests/management_session.rs`).

## Known differences

- **KeePass 1 files.** kdbxweb reads KDBX only; the `keepass` crate also opens the older
  `.kdb` format, so such a file works here and is "not a KeePass database" in the Electron
  build.
- **A wrong KDBX 3 password.** That format has no check of the key ahead of the payload, so
  the reader fails on the padding of what it decrypted; this is reported as the wrong
  password it is (`open_failure`).
- **A kind this build does not know.** A source whose kind a newer build stored is left out
  of the list rather than shown as unavailable, and a login that needs it says "Muxus does
  not know the password manager …".
- **A program is a file.** `resolve_program` takes a file with an execute bit; `access(2)`
  as Node asks it also says yes to a directory, which then fails to start.
- **Proton Pass vault names.** A reference whose vault name is not valid percent-encoding is
  an entry that is "not valid" rather than an exception.
