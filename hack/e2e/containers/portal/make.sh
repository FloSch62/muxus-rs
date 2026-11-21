#!/bin/sh
# Write the portal's certificates into $1 and the lists it publishes into $2.
set -eu
certs="$1"
html="$2"
mkdir -p "$certs" "$html/lists"
cd "$certs"

names="subjectAltName=DNS:localhost,IP:127.0.0.1"

# An authority of the portal's own, as a company would run one, and a certificate from it.
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=Muxus e2e portal authority" \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -keyout ca.key -out ca.pem 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout server.key -out server.csr 2>/dev/null
printf '%s\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n' \
  "$names" > server.ext
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 3650 \
  -extfile server.ext -out server.pem 2>/dev/null

# A certificate that only vouches for itself.
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=localhost" \
  -addext "$names" -addext "basicConstraints=CA:FALSE" \
  -keyout selfsigned.key -out selfsigned.pem 2>/dev/null
chmod 644 ./*.key

cp ca.pem "$html/ca.pem"

# A MobaXterm shared sessions file as a Windows portal writes it: CRLF lines, and the
# folder "Zürich" in the Windows code page (0xFC), not UTF-8.
{
  printf '[Bookmarks]\r\nSubRep=\r\nImgNum=42\r\n'
  printf 'edge=#109#0%%edge.lab.example.net%%22%%alice%%%%-1%%-1%%%%%%%%%%0%%0%%0%%%%%%-1%%0%%0%%0%%%%1080%%%%0%%0%%1#MobaFont%%10\r\n'
  printf '[Bookmarks_1]\r\nSubRep=Z\374rich\r\nImgNum=41\r\n'
  printf 'node0=#109#0%%node0.lab.example.net%%2222%%alice%%%%-1%%-1%%%%%%%%%%0%%0%%0%%%%%%-1%%0%%0%%0%%%%1080%%%%0%%0%%1#MobaFont%%10\r\n'
  printf 'desk=#91#4%%10.0.1.9%%3389%%admin%%0%%-1%%-1%%0%%0%%0%%0%%0%%-1%%bastion.lab.example.net%%2222%%jump%%0%%0%%%%-1\r\n'
} > "$html/lists/lab.mxtsessions"

# A folder another Muxus shared.
cat > "$html/lists/team.muxus-sessions.json" <<'JSON'
{
  "format": "muxus-shared-sessions",
  "version": 1,
  "name": "Team",
  "createdAt": "2026-11-20T10:00:00.000Z",
  "hosts": [
    {
      "id": "bastion",
      "name": "bastion",
      "folder": "Jump hosts",
      "profile": { "kind": "ssh", "target": "bastion.lab.example.net", "useConfig": false, "port": 2222 }
    },
    {
      "id": "leaf",
      "name": "leaf-01",
      "folder": "Fabric",
      "color": "#a855f7",
      "icon": "router",
      "profile": {
        "kind": "ssh",
        "target": "leaf-01.lab.example.net",
        "useConfig": false,
        "proxyJump": ["muxus-host:bastion"]
      }
    },
    {
      "id": "console",
      "name": "console",
      "profile": { "kind": "telnet", "host": "console.lab.example.net", "port": 2001 }
    }
  ]
}
JSON
