on run argv
	tell application "UTM"
		set vm to virtual machine named (item 1 of argv)
		set config to configuration of vm
		return config
	end tell
end run
