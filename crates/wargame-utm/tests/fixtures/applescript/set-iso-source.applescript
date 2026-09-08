on run argv
	set vmName to item 1 of argv
	set isoFile to POSIX file (item 2 of argv)
	tell application "UTM"
		set vm to virtual machine named vmName
		set config to configuration of vm
		set i to id of item 1 of drives of config
		set item 1 of drives of config to {id:i, source:isoFile}
		set n to index of item 1 of network interfaces of config
		set item 1 of network interfaces of config to {index:n, mode:host}
		update configuration of vm with config
		set config2 to configuration of vm
		return "mode=" & (mode of item 1 of network interfaces of config2 as text) & " hostsize0=" & (host size of item 1 of drives of config2 as text)
	end tell
end run
