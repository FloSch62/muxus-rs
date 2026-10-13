Xvnc :1 -geometry 1024x768 -depth 24 -rfbport 5900 -SecurityTypes VncAuth \
  -PasswordFile /root/.vnc/passwd -desktop muxus-e2e-vnc &
sleep 1
export DISPLAY=:1
while :; do xterm -geometry 80x24+20+20 -title e2e-xterm; sleep 1; done
