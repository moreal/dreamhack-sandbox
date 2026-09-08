-- Host path of the VM's first serial port, i.e. the machine-readable console.
-- usage: osascript serial-port-address.applescript <vm-name>
-- NOTE 1: this is EMPTY while the VM is stopped. Call it only after `utmctl status`
--   reports `started`.
-- NOTE 2: `utmctl attach` is a stub in 4.7.5 ("WARNING: attach command is not
--   implemented yet!") and only prints this same path, so read the ptty yourself.
-- NOTE 3: if the VM has a display device, boot output goes to the UTM window and the
--   serial port stays silent — use clear-displays.applescript for headless capture.
on run argv
	tell application "UTM"
		set vm to virtual machine named (item 1 of argv)
		return (address of first serial port of vm)
	end tell
end run
