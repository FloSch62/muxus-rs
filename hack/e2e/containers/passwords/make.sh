#!/usr/bin/env bash
# Writes into the directory given:
#   keepass/both.kdbx   KeePassXC, password "kf-master" and the key file both.keyx
#   keepass/only.kdbx   KeePassXC, the key file only.keyx and no password
#   gnupg/              a key ring whose one key has no passphrase
#   pass/               a store made with `pass`
#   gopass/             a store made with `gopass`
#   versions            what wrote them
set -euo pipefail

out="${1:?where to write}"
mkdir -p "$HOME" "$out/keepass"

# --- KeePassXC ------------------------------------------------------------------------------
cd "$out/keepass"
kp() { keepassxc-cli "$@"; }
printf 'kf-master\nkf-master\n' | kp db-create -q --set-key-file both.keyx -p -t 100 both.kdbx
printf 'kf-master\n' | kp mkdir -q -k both.keyx both.kdbx Network
printf 'kf-master\n' | kp mkdir -q -k both.keyx both.kdbx Network/Edge
printf 'kf-master\nedge-pässword\n' |
  kp add -q -k both.keyx -u fw-admin --url https://edge-fw -p both.kdbx Network/Edge/edge-fw
printf 'kf-master\n' | kp add -q -k both.keyx -u nobody both.kdbx Network/no-password
printf 'kf-master\nbin-pw\n' | kp add -q -k both.keyx -u old -p both.kdbx Network/retired
# Twice: the first moves it to the recycle bin.
printf 'kf-master\n' | kp rm -q -k both.keyx both.kdbx Network/retired

kp db-create -q --set-key-file only.keyx -t 100 only.kdbx </dev/null >/dev/null 2>&1
printf 'only-pässword\n' | kp add -q --no-password -k only.keyx -u root -p only.kdbx console

# --- GnuPG ----------------------------------------------------------------------------------
export GNUPGHOME="$out/gnupg"
mkdir -p "$GNUPGHOME"
chmod 700 "$GNUPGHOME"
gpg --batch --quiet --passphrase '' --pinentry-mode loopback \
  --quick-generate-key 'Muxus E2E <muxus-e2e@example.invalid>' ed25519 cert,sign never 2>/dev/null
key="$(gpg --batch --list-keys --with-colons | awk -F: '/^fpr:/ { print $10; exit }')"
gpg --batch --quiet --passphrase '' --pinentry-mode loopback --quick-add-key "$key" cv25519 encr never

# --- pass -----------------------------------------------------------------------------------
export PASSWORD_STORE_DIR="$out/pass"
pass init "$key" >/dev/null
printf 'pass-password\nuser: root\nurl: ssh://lab\n' | pass insert --multiline servers/lab/root >/dev/null
printf 'wifi-pässword\n' | pass insert --echo wifi >/dev/null
pass git init >/dev/null 2>&1 || true

# --- gopass ---------------------------------------------------------------------------------
# A store of its own: with PASSWORD_STORE_DIR set, gopass would write into the one above.
unset PASSWORD_STORE_DIR
gopass --yes init --crypto gpg --storage fs --path "$out/gopass" "$key" >/dev/null 2>&1
printf 'gopass-password\nuser: admin\n' | gopass insert --multiline network/core-sw >/dev/null

gpgconf --kill all || true
{
  keepassxc-cli --version | sed 's/^/keepassxc /'
  pass version | grep -o 'v[0-9.]*' | sed 's/^/pass /'
  gopass --version | head -1
  gpg --version | head -1
} >"$out/versions"
cat "$out/versions"
