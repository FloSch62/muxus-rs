#!/bin/bash
# A home directory that looks like a real one, plus the awkward cases a file
# browser meets on real servers.
set -euo pipefail
home="$1"; user="$2"
cd "$home"
# Dotfiles and dot folders.
printf 'export EDITOR=vim\nalias ll="ls -la"\n' >>.bashrc
mkdir -p .config/htop .config/git .cache/pip .local/share .local/bin .ssh .vim/undo
printf '[user]\n\tname = Alice\n' >.config/git/config
printf 'set number\n' >.vimrc
printf 'history\n' >.bash_history
printf '{}' >.config/htop/htoprc
# Ordinary content.
mkdir -p projects/muxus/src projects/website/assets Documents Downloads Pictures logs backups
for i in $(seq 1 40); do printf 'line %s\n' "$i" >"projects/muxus/src/module_$i.rs"; done
printf '# Notes\n\nSome text.\n' >Documents/notes.md
printf 'hello\n' >hello.txt
printf '#!/bin/sh\necho deploy\n' >deploy.sh && chmod 755 deploy.sh
for i in $(seq -w 1 120); do printf 'log %s\n' "$i" >"logs/app-2026-10-$i.log"; done
# A directory with many entries (several READDIR batches).
mkdir -p many
for i in $(seq -w 1 1500); do : >"many/file-$i.dat"; done
for i in $(seq -w 1 60); do mkdir -p "many/dir-$i"; done
# Names: long, unicode, spaces, leading dash, shell metacharacters, a tab.
long=$(printf 'x%.0s' $(seq 1 200))
: >"$long.txt"
: >'Ünïcödé — 文件 — файл 📁.txt'
mkdir -p 'Résumé et documents'
: >'name with spaces.txt'
: >'-dash-first'
: >'quote'"'"'s & $dollar (paren) [br].txt'
: >"$(printf 'tab\there.txt')"
# Not valid UTF-8 (Latin-1 bytes as written by old tools).
: >"$(printf 'latin1-caf\xe9.txt')"
# Symbolic links: to a folder, to a file, dangling, absolute, a loop.
ln -s projects link-to-projects
ln -s hello.txt link-to-hello
ln -s /does/not/exist broken-link
ln -s /etc/hostname absolute-link
ln -s loop-link loop-link
# Special files.
mkfifo a-fifo
# Sizes and dates.
truncate -s 5G big-sparse.iso
head -c 3000000 /dev/urandom >random.bin
touch -d '1990-01-01 00:00:00' old-file.txt
touch -d '2099-12-31 23:59:59' future-file.txt
chmod 4755 deploy.sh
chown -R "$user:$user" "$home"
# Things the user may not read.
printf 'top secret\n' >root-only.txt && chown root:root root-only.txt && chmod 600 root-only.txt
mkdir -p locked-dir && printf 'x' >locked-dir/inside.txt && chown -R root:root locked-dir && chmod 700 locked-dir
mkdir -p no-exec-dir && : >no-exec-dir/a && chown -R "$user:$user" no-exec-dir && chmod 600 no-exec-dir
