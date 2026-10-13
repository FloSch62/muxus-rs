#!/bin/sh
# AUTHORIZED_KEYS: public keys for alice and nofwd. HTTP=1 serves /srv/www on
# the container's loopback (port 8080), and on every interface when HTTP=all.
set -e
if [ -n "$AUTHORIZED_KEYS" ]; then
  for user in alice nofwd; do
    install -d -m 700 -o "$user" -g "$user" "/home/$user/.ssh"
    printf '%s\n' "$AUTHORIZED_KEYS" >"/home/$user/.ssh/authorized_keys"
    chown "$user:$user" "/home/$user/.ssh/authorized_keys"
    chmod 600 "/home/$user/.ssh/authorized_keys"
  done
fi
ssh-keygen -A >/dev/null
case "$HTTP" in
  1) (cd /srv/www && exec python3 -m http.server 8080 --bind 127.0.0.1 >/var/log/http.log 2>&1) & ;;
  all) (cd /srv/www && exec python3 -m http.server 8080 >/var/log/http.log 2>&1) & ;;
esac
exec /usr/sbin/sshd -D -e
