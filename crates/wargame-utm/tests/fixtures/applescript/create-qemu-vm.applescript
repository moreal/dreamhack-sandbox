on run argv
	set vmName to item 1 of argv
	set archName to item 2 of argv
	set hyp to ((item 3 of argv) is "true")
	set isoFile to POSIX file (item 4 of argv)
	tell application "UTM"
		set vm to make new virtual machine with properties {backend:qemu, configuration:{name:vmName, architecture:archName, memory:4096, cpu cores:4, hypervisor:hyp, uefi:true, drives:{{removable:true, source:isoFile}, {guest size:20480}}}}
		set c to configuration of vm
		return (id of vm) & tab & (name of vm) & tab & "arch=" & (architecture of c) & tab & "machine=" & (machine of c) & tab & "hypervisor=" & (hypervisor of c as text) & tab & "uefi=" & (uefi of c as text) & tab & "netmode=" & (mode of item 1 of network interfaces of c as text) & tab & "serial=" & (interface of item 1 of serial ports of c as text)
	end tell
end run
