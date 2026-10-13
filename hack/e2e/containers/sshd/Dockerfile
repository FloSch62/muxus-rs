# OpenSSH test server for hack/e2e: password, public key and
# keyboard-interactive users, TCP forwarding, a web service on the
# container's loopback and a realistic home directory for the file browser.
FROM ubuntu:24.04
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      openssh-server python3 curl tmux screen procps iproute2 \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /run/sshd
COPY e2e.conf /etc/ssh/sshd_config.d/00-e2e.conf
COPY tree.sh entrypoint.sh /opt/e2e/
RUN chmod +x /opt/e2e/*.sh \
 && useradd -m -s /bin/bash alice && echo 'alice:alice-pass' | chpasswd \
 && useradd -m -s /bin/bash kbd && echo 'kbd:kbd-pass' | chpasswd \
 && useradd -m -s /bin/bash nofwd && echo 'nofwd:nofwd-pass' | chpasswd \
 && /opt/e2e/tree.sh /home/alice alice \
 && mkdir -p /srv/www && echo 'muxus-e2e-web-ok' > /srv/www/index.html
EXPOSE 22
ENTRYPOINT ["/opt/e2e/entrypoint.sh"]
