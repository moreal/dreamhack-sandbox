-- Create a QEMU-backend VM with an installer ISO on a removable drive plus one blank
-- disk. This is the only way to create a VM: `utmctl` has no create verb.
-- usage: osascript create-qemu-vm.applescript <name> <aarch64|x86_64> <true|false> <absolute-iso-path>
--        arg 3 is `hypervisor`.
-- NOTE 1: `POSIX file` must be evaluated OUTSIDE the `tell application "UTM"` block;
--   inside it UTM resolves the path in its own sandbox and fails with -1728.
-- NOTE 2: `hypervisor:true` is SILENTLY IGNORED when the guest architecture does not
--   match the host (x86_64 on Apple Silicon still gets `-accel tcg`). Reject that
--   combination in the caller; UTM will not.
-- NOTE 3: the new VM gets NO display, one `ptty` serial port and one `shared`
--   network interface.
-- NOTE 4: `guest size` is MiB and is write-only — it cannot be read back.
-- NOTE 5: the new VM does get one `ptty` serial port, but `serial ports of
--   <configuration>` reads back as an EMPTY list in every access form, so it is not
--   echoed here. Use `address of first serial port of vm` on the VM element instead
--   (see serial-port-address.applescript).
-- Unlike `duplicate`, `make` DOES return a specifier, so id/name are available here.
on run argv
	set vmName to item 1 of argv
	set archName to item 2 of argv
	set hyp to ((item 3 of argv) is "true")
	set isoFile to POSIX file (item 4 of argv)
	tell application "UTM"
		set vm to make new virtual machine with properties {backend:qemu, configuration:{name:vmName, architecture:archName, memory:4096, cpu cores:4, hypervisor:hyp, uefi:true, drives:{{removable:true, source:isoFile}, {guest size:20480}}}}
		set c to configuration of vm
		return (id of vm) & tab & (name of vm) & tab & "arch=" & (architecture of c) & tab & "machine=" & (machine of c) & tab & "hypervisor=" & (hypervisor of c as text) & tab & "uefi=" & (uefi of c as text) & tab & "netmode=" & (mode of item 1 of network interfaces of c as text)

	end tell
end run
