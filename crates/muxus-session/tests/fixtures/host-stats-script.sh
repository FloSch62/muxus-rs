LC_ALL=C
PATH="$PATH:/usr/bin:/bin:/usr/sbin:/sbin"
export LC_ALL PATH
muxus_section() { printf '\n@@muxus:%s\n' "$1"; }
muxus_section begin
muxus_section hostname; uname -n 2>/dev/null
muxus_section kernel; uname -sr 2>/dev/null
muxus_section os-release; grep -E '^(PRETTY_NAME|NAME|VERSION_ID)=' /etc/os-release 2>/dev/null
muxus_section who; who 2>/dev/null
muxus_section df; df -P -k / 2>/dev/null
if [ -r /proc/stat ]; then
  muxus_section uptime; cat /proc/uptime 2>/dev/null
  muxus_section stat; head -n 1 /proc/stat 2>/dev/null; grep -c '^cpu[0-9]' /proc/stat 2>/dev/null
  muxus_section loadavg; cat /proc/loadavg 2>/dev/null
  muxus_section meminfo; grep -E '^(MemTotal|MemAvailable|MemFree|Buffers|Cached|SwapTotal|SwapFree):' /proc/meminfo 2>/dev/null
  muxus_section route; cat /proc/net/route 2>/dev/null
  muxus_section netdev; cat /proc/net/dev 2>/dev/null
else
  muxus_section sw-vers; sw_vers 2>/dev/null
  muxus_section now; date +%s 2>/dev/null
  muxus_section boottime; sysctl -n kern.boottime 2>/dev/null
  muxus_section bsd-loadavg; sysctl -n vm.loadavg 2>/dev/null
  muxus_section ncpu; sysctl -n hw.ncpu 2>/dev/null
  muxus_section memsize; sysctl -n hw.memsize 2>/dev/null
  muxus_section vm-stat; vm_stat 2>/dev/null
  muxus_section bsd-route; route -n get default 2>/dev/null
  muxus_section netstat; netstat -ibn 2>/dev/null
fi
muxus_section end
