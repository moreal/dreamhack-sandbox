#!/bin/zsh
set -u
# Driver that produced readback-transcript.txt sections 0-13 and 16.
# usage: ./readback-transcript.sh > readback-transcript.txt 2>&1
# Requires: UTM installed, the two Kali ISOs present, and no VM named wargame-t1-*.
# It creates two app-owned VMs and section 13/16 lists them for deletion; delete them
# by UUID afterwards, skipping any row whose name does not start with wargame-t1.
AS=${AS:-$(cd "$(dirname "$0")/applescript" && pwd)}
ISODIR="$HOME/Library/Application Support/wargame-vm/iso"
ARM="$ISODIR/kali-linux-2026.2-installer-arm64.iso"
AMD="$ISODIR/kali-linux-2026.2-installer-amd64.iso"
VM=wargame-t1-evidence
VMX=wargame-t1-evidence-x86

run() { print -r -- "\$ $*"; eval "$@" 2>&1; print -r -- "exit=$?"; print -- ""; }
oa()  { print -r -- "\$ osascript -e '$1'"; osascript -e "$1" 2>&1; print -r -- "exit=$?"; print -- ""; }

print -r -- "### Consolidated readback transcript — UTM 4.7.5 / macOS 26.6.2 / $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
print -- ""

print -r -- "## 0. app handshake (trap T8)"
run 'open -a UTM'
run 'until utmctl version >/dev/null 2>&1; do sleep 1; done; utmctl version'

print -r -- "## 1. create an aarch64 VM (hypervisor on, UEFI, ISO + 20 GiB disk)"
run "osascript $AS/create-qemu-vm.applescript $VM aarch64 true '$ARM'"

print -r -- "## 2. attach a SECOND removable drive (the Windows guest-tools CD case)"
run "osascript $AS/add-removable-drive.applescript $VM '$AMD'"

print -r -- "## 3. replace the ISO backing the FIRST removable drive"
run "osascript $AS/set-iso-source.applescript $VM '$AMD'"
print -r -- "# host size of drive 1 now matches the amd64 ISO (4580 MiB), drive 3 still the amd64 one too"
run "osascript $AS/show-drives.applescript $VM"

print -r -- "## 4. T5 quirk: 'count of' is always 0; 'length of' works except for serial ports"
oa 'tell application "UTM"
	set c to configuration of (virtual machine named "'$VM'")
	set d to 0
	repeat with x in (drives of c)
		set d to d + 1
	end repeat
	set sp to 0
	repeat with x in (serial ports of c)
		set sp to sp + 1
	end repeat
	return "drives: loop=" & (d as text) & " count-of=" & ((count of drives of c) as text) & " length-of=" & ((length of (drives of c)) as text) & "  ||  serial ports: loop=" & (sp as text) & " count-of=" & ((count of serial ports of c) as text) & " length-of=" & ((length of (serial ports of c)) as text)
end tell'
print -r -- "# ...yet the flattened whole-record dump clearly contains the serial port:"
run "osascript $AS/dump-configuration.applescript $VM"

print -r -- "## 5. qemu additional arguments round-trip (T10)"
oa 'tell application "UTM"
	set vm to virtual machine named "'$VM'"
	set c to configuration of vm
	set qemu additional arguments of c to {{argument string:"-device"}, {argument string:"virtio-balloon-pci"}}
	update configuration of vm with c
	set c2 to configuration of vm
	set out to ""
	repeat with a in (qemu additional arguments of c2)
		set out to out & (argument string of a) & " "
	end repeat
	return "readback: " & out
end tell'
oa 'tell application "UTM"
	set vm to virtual machine named "'$VM'"
	set c to configuration of vm
	set qemu additional arguments of c to {}
	update configuration of vm with c
	set c2 to configuration of vm
	return "cleared, length=" & ((length of (qemu additional arguments of c2)) as text)
end tell'

print -r -- "## 6. network mode -> host, and the host-side vmnet bridges BEFORE the VM runs"
run "osascript $AS/set-network-mode.applescript $VM host"
run "utmctl status $VM"
run "ifconfig | awk '/^bridge1[0-9][0-9]:/{b=substr(\$1,1,length(\$1)-1)} /inet /{if(b!=\"\"){print b, \$2; b=\"\"}}'"

print -r -- "## 7. start in host mode; bridges AFTER (FR-6 host-management connection)"
run "utmctl start $VM"
run 'sleep 8; true'
run "utmctl status $VM"
run "ps -Aww -o command | grep qemu-aarch64-softmmu | grep -v grep | grep -oE '\-netdev [^ ]+'"
run "ifconfig | awk '/^bridge1[0-9][0-9]:/{b=substr(\$1,1,length(\$1)-1)} /inet /{if(b!=\"\"){print b, \$2; b=\"\"}}'"
print -r -- "# the bridge that appeared, with its member interface:"
run "ifconfig bridge104"

print -r -- "## 8. serial console address while running, and the --attach flag"
run "osascript $AS/serial-port-address.applescript $VM"

print -r -- "## 9. input automation (QEMU backend only): send Enter as raw scan codes"
oa 'tell application "UTM" to input scan code of virtual machine named "'$VM'" codes {28, 156}'

print -r -- "## 10. suspend / resume / suspend --save-state — the full status enum in motion"
run "utmctl suspend $VM"
run "sleep 2; utmctl status $VM"
run "utmctl start $VM"
run "sleep 2; utmctl status $VM"
run "utmctl suspend $VM --save-state"
run "sleep 3; utmctl status $VM"
print -r -- "# stop refuses a PAUSED VM (and still exits 0 — trap T1)"
run "utmctl stop $VM"
run "utmctl start $VM"
run "sleep 2; utmctl status $VM"
run "utmctl stop $VM"
run "for i in 1 2 3 4 5 6 7 8 9 10; do s=\$(utmctl status $VM); echo \"t=\${i}s status=\$s\"; [ \"\$s\" = stopped ] && break; sleep 1; done"

print -r -- "## 11. export / import — the import keeps the source display name (trap T3)"
OUT=${OUT:-${TMPDIR:-/tmp}/wargame-t1-export}
run "rm -rf $OUT && mkdir -p $OUT"
oa 'tell application "UTM" to export virtual machine named "'$VM'" to POSIX file "'$OUT'/evidence.utm"'
run "du -sh $OUT/evidence.utm"
oa 'tell application "UTM"
	set vm to import new virtual machine from POSIX file "'$OUT'/evidence.utm"
	return "imported id=" & (id of vm) & " name=" & (name of vm)
end tell'
print -r -- "# two VMs now share one display name; lookup by name silently returns the first"
run "utmctl list"
oa 'tell application "UTM" to return "named lookup -> " & (id of virtual machine named "'$VM'")'
print -r -- "# delete the imported duplicate again, by UUID"
oa 'tell application "UTM"
	set out to ""
	repeat with vm in (virtual machines)
		if (name of vm) is "'$VM'" then set out to out & (id of vm) & " "
	end repeat
	return "both ids: " & out
end tell'

print -r -- "## 12. x86_64: hypervisor:false vs hypervisor:true, config readback AND generated accel"
run "osascript $AS/create-qemu-vm.applescript $VMX x86_64 false '$AMD'"
oa 'tell application "UTM"
	set c to configuration of (virtual machine named "'$VMX'")
	return "config readback hypervisor=" & ((hypervisor of c) as text)
end tell'
run "utmctl start $VMX"
run 'sleep 8; true'
run "ps -Aww -o command | grep qemu-x86_64-softmmu | grep -v grep | grep -oE '\-accel [^ ]+|\-smp [^ ]+|\-cpu [^ ]+'"
run "utmctl stop $VMX --kill"
run "for i in 1 2 3 4 5 6 7 8 9 10; do s=\$(utmctl status $VMX); [ \"\$s\" = stopped ] && { echo stopped after \${i}s; break; }; sleep 1; done"
oa 'tell application "UTM"
	set vm to virtual machine named "'$VMX'"
	set c to configuration of vm
	set hypervisor of c to true
	update configuration of vm with c
	return "update issued"
end tell'
oa 'tell application "UTM"
	set c to configuration of (virtual machine named "'$VMX'")
	return "config readback hypervisor=" & ((hypervisor of c) as text)
end tell'
run "utmctl start $VMX"
run 'sleep 8; true'
print -r -- "# hypervisor is TRUE in the configuration, yet the accelerator is still TCG (trap T6)"
run "ps -Aww -o command | grep qemu-x86_64-softmmu | grep -v grep | grep -oE '\-accel [^ ]+|\-smp [^ ]+|\-cpu [^ ]+'"
run "utmctl stop $VMX --kill"
run "for i in 1 2 3 4 5 6 7 8 9 10; do s=\$(utmctl status $VMX); [ \"\$s\" = stopped ] && { echo stopped after \${i}s; break; }; sleep 1; done"

print -r -- "## 13. cleanup — delete by UUID, skipping any VM that is not ours"
run "utmctl list"
