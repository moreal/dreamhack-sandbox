on run argv
	tell application "UTM"
		set vm to virtual machine named (item 1 of argv)
		set c to configuration of vm
		set displays of c to {}
		update configuration of vm with c
		set c2 to configuration of vm
		return "displays length=" & ((length of (displays of c2)) as text)
	end tell
end run
