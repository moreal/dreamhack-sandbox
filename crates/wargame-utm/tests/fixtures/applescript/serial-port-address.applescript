on run argv
	tell application "UTM"
		set vm to virtual machine named (item 1 of argv)
		return (address of first serial port of vm)
	end tell
end run
