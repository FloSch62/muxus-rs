# RDP test server for hack/e2e: xrdp with Xorg and an xterm session,
# login rdpuser / rdp-pass.
FROM ubuntu:24.04
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      xrdp xorgxrdp xterm dbus-x11 ssl-cert \
 && rm -rf /var/lib/apt/lists/* \
 && useradd -m -s /bin/bash rdpuser && echo 'rdpuser:rdp-pass' | chpasswd \
 && adduser xrdp ssl-cert \
 && printf '#!/bin/sh\nexec xterm -geometry 100x30+0+0 -title e2e-rdp\n' >/home/rdpuser/.xsession \
 && chmod +x /home/rdpuser/.xsession && chown rdpuser:rdpuser /home/rdpuser/.xsession
COPY start.sh /start.sh
EXPOSE 3389
CMD ["sh", "/start.sh"]
