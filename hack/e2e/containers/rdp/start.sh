mkdir -p /run/xrdp /var/run/xrdp
rm -f /var/run/xrdp/*.pid
/usr/sbin/xrdp-sesman
exec /usr/sbin/xrdp --nodaemon
