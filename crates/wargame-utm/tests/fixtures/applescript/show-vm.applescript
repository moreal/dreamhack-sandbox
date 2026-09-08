-- One-line summary of a VM: identity plus the scalar configuration fields.
-- usage: osascript show-vm.applescript <vm-name>
-- NOTE 1: `count of drives of c` returns 0 on UTM 4.7.5 — a dictionary quirk. Use
--   `length of (drives of c)` (done below) or a `repeat` loop.
-- NOTE 2: `id of vm` is the UUID and is the only stable identifier. Display names are
--   NOT unique (an imported bundle keeps the source name) and `virtual machine named`
--   silently returns the first match.
on run argv
	tell application "UTM"
		set vm to virtual machine named (item 1 of argv)
		set c to configuration of vm
		return (id of vm) & " | name=" & (name of c) & " | arch=" & (architecture of c) & " | machine=" & (machine of c) & " | mem=" & (memory of c as text) & " | cores=" & (cpu cores of c as text) & " | hypervisor=" & (hypervisor of c as text) & " | uefi=" & (uefi of c as text) & " | netmode=" & (mode of item 1 of network interfaces of c as text) & " | drives=" & ((length of (drives of c)) as text) & " | status=" & (status of vm as text)
	end tell
end run
