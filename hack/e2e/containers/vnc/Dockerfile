# VNC test server for hack/e2e: TigerVNC with an xterm, password vnc-pass.
FROM alpine:3.22
RUN apk add --no-cache tigervnc xterm font-dejavu \
 && mkdir -p /root/.vnc \
 && echo vnc-pass | vncpasswd -f >/root/.vnc/passwd && chmod 600 /root/.vnc/passwd
COPY start.sh /start.sh
EXPOSE 5900
CMD ["sh", "/start.sh"]
