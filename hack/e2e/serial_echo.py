"""The far end of the scenarios' serial line: a console that answers.

    python3 hack/e2e/serial_echo.py <pty>

Writes a banner, then answers every line it reads with `serial-echo: <line>` and
a prompt, as a device console would.
"""

import os
import sys
import tty

path = sys.argv[1]
# Every line received also goes to <pty>.lines, for the scenario to check.
record = open(path + ".lines", "ab", buffering=0)
fd = os.open(path, os.O_RDWR | os.O_NOCTTY)
tty.setraw(fd)
os.write(fd, b"\r\nmuxus e2e serial console\r\ndevice> ")
line = b""
while True:
    data = os.read(fd, 1024)
    if not data:
        break
    for byte in data:
        ch = bytes([byte])
        if ch in (b"\r", b"\n"):
            record.write(line + b"\n")
            os.write(fd, b"\r\nserial-echo: " + line + b"\r\ndevice> ")
            line = b""
        elif ch in (b"\x7f", b"\x08"):
            line = line[:-1]
            os.write(fd, b"\x08 \x08")
        else:
            line += ch
            os.write(fd, ch)
