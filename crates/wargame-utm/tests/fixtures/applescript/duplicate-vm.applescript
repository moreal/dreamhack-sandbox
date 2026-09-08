-- Duplicate a VM, optionally overriding configuration fields.
-- usage: osascript duplicate-vm.applescript <source-vm-name> <new-name>
-- NOTE: `duplicate` returns NOTHING (the sdef command has no <result>), so the new
-- VM must be looked up by name afterwards. Without `with properties` the copy is
-- named "<source> 2".
on run argv
	set srcName to item 1 of argv
	set newName to item 2 of argv
	tell application "UTM"
		duplicate (virtual machine named srcName) with properties {configuration:{name:newName, hypervisor:false}}
		set dup to virtual machine named newName
		set c to configuration of dup
		return (id of dup) & tab & (name of dup) & tab & "arch=" & (architecture of c) & tab & "hypervisor=" & ((hypervisor of c) as text)
	end tell
end run
