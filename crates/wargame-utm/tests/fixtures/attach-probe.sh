#!/bin/zsh
set -u
# Captures, with stdout and stderr kept apart and the real exit code recorded:
#   (a) what `make` produces when the `drives` key is omitted entirely, and
#   (b) how `utmctl start <vm> --attach` and `utmctl attach <vm>` actually behave.
# usage: ./attach-probe.sh > attach-and-default-drives.txt 2>&1
# Creates one app-owned VM named wargame-t1-attach2 and deletes it at the end.
V=wargame-t1-attach2
W=${TMPDIR:-/tmp}/wg-attach-probe; mkdir -p "$W"

hdr() { print -- ""; print -r -- "$1"; }
wait_stopped() {
	local n=0
	while [ $n -lt 30 ]; do
		[ "$(utmctl status $V 2>/dev/null)" = stopped ] && { print -r -- "  (stopped after ${n}s)"; return 0; }
		sleep 1; n=$((n+1))
	done
	print -r -- "  (still $(utmctl status $V) after ${n}s)"
}

print -r -- "### utmctl --attach probe + default-drive shape — UTM 4.7.5 / macOS 26.6.2 / $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
print -r -- "# Every run below records stdout and stderr separately and the real exit status of"
print -r -- "# utmctl itself (no pipeline, no 2>&1), plus wall-clock seconds."

hdr "## 1. app handshake"
print -r -- "\$ open -a UTM; until utmctl version >/dev/null 2>&1; do sleep 1; done; utmctl version"
open -a UTM; until utmctl version >/dev/null 2>&1; do sleep 1; done; utmctl version
print -r -- "exit=$?"

hdr "## 2. create a VM with NO \`drives\` key at all"
print -r -- "\$ osascript -e 'tell application \"UTM\" to make new virtual machine with properties {backend:qemu, configuration:{name:\"$V\", architecture:\"aarch64\", memory:1024, cpu cores:1}}'"
osascript -e "tell application \"UTM\"
	set vm to make new virtual machine with properties {backend:qemu, configuration:{name:\"$V\", architecture:\"aarch64\", memory:1024, cpu cores:1}}
	return (id of vm) & \" \" & (name of vm)
end tell"
print -r -- "exit=$?"

print -- ""
print -r -- "\$ osascript applescript/show-drives.applescript $V"
osascript "${AS:-$(cd "$(dirname "$0")/applescript" && pwd)}/show-drives.applescript" $V
print -r -- "exit=$?"

hdr "## 3. the generated QEMU command line for that driveless VM"
utmctl start $V >/dev/null 2>&1; sleep 7
print -r -- "\$ utmctl status $V"
utmctl status $V
print -- ""
print -r -- "\$ ps -Aww -o command | grep qemu-aarch64-softmmu | grep -v grep | tr ' ' '\\n' | grep -E 'media=cdrom|media=disk|usb-storage|virtio-blk'"
ps -Aww -o command | grep qemu-aarch64-softmmu | grep -v grep | tr ' ' '\n' | grep -E 'media=cdrom|media=disk|usb-storage|virtio-blk'
print -r -- "exit=$?"
print -r -- "# -> UTM adds a removable CD with NO file.filename plus a blank qcow2, unasked."

hdr "## 4. \`utmctl attach\` on the RUNNING VM"
print -r -- "\$ utmctl attach $V </dev/null   (stdout and stderr captured separately)"
S=$(date +%s); timeout 20 utmctl attach $V </dev/null >"$W/at.out" 2>"$W/at.err"; RC=$?; E=$(( $(date +%s) - S ))
print -r -- "exit=$RC   elapsed=${E}s"
print -r -- "--- stdout ($(wc -c <"$W/at.out" | tr -d ' ') bytes) ---"; cat "$W/at.out"
print -r -- "--- stderr ($(wc -c <"$W/at.err" | tr -d ' ') bytes) ---"; cat "$W/at.err"

utmctl stop $V --kill >/dev/null 2>&1; wait_stopped

hdr "## 5. \`utmctl start --attach\` on a STOPPED VM, three consecutive runs"
for n in 1 2 3; do
	wait_stopped >/dev/null
	sleep 3
	print -- ""
	print -r -- "### run $n"
	print -r -- "\$ utmctl start $V --attach </dev/null   (20 s timeout, streams separate)"
	S=$(date +%s); timeout 20 utmctl start $V --attach </dev/null >"$W/s$n.out" 2>"$W/s$n.err"; RC=$?; E=$(( $(date +%s) - S ))
	print -r -- "exit=$RC   elapsed=${E}s   (124 = the timeout fired, i.e. utmctl was still running)"
	print -r -- "--- stdout ($(wc -c <"$W/s$n.out" | tr -d ' ') bytes) ---"; cat "$W/s$n.out"
	print -r -- "--- stderr ($(wc -c <"$W/s$n.err" | tr -d ' ') bytes) ---"; cat "$W/s$n.err"
	print -r -- "\$ utmctl status $V"; utmctl status $V
	utmctl stop $V --kill >/dev/null 2>&1
	wait_stopped
done

hdr "## 6. cleanup"
print -r -- "\$ utmctl list"
utmctl list
