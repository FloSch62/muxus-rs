# A lab portal for the shared sessions checks
# (crates/muxus-session/tests/shared_sessions_portal.rs): nginx publishing session lists
# over http (80), over https with a certificate of its own authority (443, the authority at
# /ca.pem) and over https with a certificate nobody vouches for (444).
FROM nginx:alpine
RUN apk add --no-cache openssl
COPY make.sh /usr/local/bin/make-portal
RUN chmod 755 /usr/local/bin/make-portal && make-portal /etc/nginx/certs /usr/share/nginx/html
COPY portal.conf /etc/nginx/conf.d/default.conf
