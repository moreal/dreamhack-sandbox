on run argv
	tell application "UTM"
		set vm to virtual machine named (item 1 of argv)
		set c to configuration of vm
		return (id of vm) & " | name=" & (name of c) & " | arch=" & (architecture of c) & " | machine=" & (machine of c) & " | mem=" & (memory of c as text) & " | cores=" & (cpu cores of c as text) & " | hypervisor=" & (hypervisor of c as text) & " | uefi=" & (uefi of c as text) & " | netmode=" & (mode of item 1 of network interfaces of c as text) & " | drives=" & (count of drives of c) & " | status=" & (status of vm as text)
	end tell
end run
