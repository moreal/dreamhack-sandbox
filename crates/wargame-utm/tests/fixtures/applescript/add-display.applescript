-- Give a VM a display device, so UTM opens a graphical console window for it.
-- usage: osascript add-display.applescript <vm-name> <hardware-name>
--        e.g. virtio-ramfb (aarch64) or virtio-vga (x86_64)
-- NOTE 1: a VM created by `make` has NO display (`displays:{}`); without one the
--   firmware/boot output only reaches the serial ptty and the user can never run a
--   manual OS install.
-- NOTE 2: an unknown hardware name is accepted silently and falls back to a default
--   (the sdef says so). Validate the name before calling this.
-- NOTE 3: the VM must be `stopped`, else -2700.
on run argv
	set vmName to item 1 of argv
	set hw to item 2 of argv
	tell application "UTM"
		set vm to virtual machine named vmName
		set c to configuration of vm
		set displays of c to {{hardware:hw}}
		update configuration of vm with c
		set c2 to configuration of vm
		set out to "displays:" & linefeed
		repeat with d in (displays of c2)
			set out to out & "  index=" & (index of d as text) & " hardware=" & (hardware of d) & " dynamic=" & (dynamic resolution of d as text) & " native=" & (native resolution of d as text) & linefeed
		end repeat
		return out
	end tell
end run
