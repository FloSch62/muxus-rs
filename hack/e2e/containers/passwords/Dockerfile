# Password managers for the muxus-passwords checks: KeePassXC, pass and gopass write the
# databases and stores the tests then read (crates/muxus-passwords/tests/tools.rs).
FROM alpine:3.22
RUN apk add --no-cache bash gnupg gopass keepassxc pass
COPY make.sh /usr/local/bin/make-passwords
RUN chmod 755 /usr/local/bin/make-passwords
ENV HOME=/tmp/home
VOLUME /out
CMD ["make-passwords", "/out"]
