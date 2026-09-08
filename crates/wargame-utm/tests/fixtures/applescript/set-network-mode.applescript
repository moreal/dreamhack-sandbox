on run argv
	set vmName to item 1 of argv
	set m to item 2 of argv
	tell application "UTM"
		set vm to virtual machine named vmName
		set c to configuration of vm
		set n to index of item 1 of network interfaces of c
		if m is "shared" then
			set item 1 of network interfaces of c to {index:n, mode:shared}
		else if m is "host" then
			set item 1 of network interfaces of c to {index:n, mode:host}
		else if m is "emulated" then
			set item 1 of network interfaces of c to {index:n, mode:emulated, port forwards:{{host port:22022, guest port:22}}}
		else if m is "bridged" then
			set item 1 of network interfaces of c to {index:n, mode:bridged, host interface:"en0"}
		end if
		update configuration of vm with c
		set c2 to configuration of vm
		set ni to item 1 of network interfaces of c2
		return "mode=" & (mode of ni as text) & " hardware=" & (hardware of ni) & " hostif=" & (host interface of ni)
	end tell
end run
