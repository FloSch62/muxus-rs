# Telnet test server for hack/e2e: busybox telnetd with a login.
FROM alpine:3.22
RUN apk add --no-cache busybox-extras \
 && adduser -D -s /bin/sh tuser && echo 'tuser:tuser-pass' | chpasswd \
 && echo 'muxus e2e telnet' >/etc/motd
EXPOSE 23
CMD ["busybox-extras", "telnetd", "-F", "-p", "23", "-l", "/bin/login"]
