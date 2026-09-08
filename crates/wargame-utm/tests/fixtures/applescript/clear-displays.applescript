-- Remove every display device, so the console lands on the serial ptty instead of a
-- UTM window. Useful for headless boot capture.
-- usage: osascript clear-displays.applescript <vm-name>
-- NOTE: the VM must be `stopped`, else -2700.
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
