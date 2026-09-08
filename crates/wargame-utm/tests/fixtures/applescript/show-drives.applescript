-- List the drives of a VM with every field UTM actually returns.
-- usage: osascript show-drives.applescript <vm-name>
-- NOTE: `source` (which ISO/qcow2 backs the drive) and `guest size` are NOT readable
--   — reading them inside the returned record fails with -1700. `host size` (MiB,
--   host-side allocation) is the only cross-check available: it equals the ISO size
--   for a removable drive and 0 for a fresh sparse qcow2.
on run argv
	tell application "UTM"
		set c to configuration of (virtual machine named (item 1 of argv))
		set out to ""
		set n to 0
		repeat with d in (drives of c)
			set n to n + 1
			set out to out & "  [" & (n as text) & "] id=" & (id of d) & " removable=" & (removable of d as text) & " iface=" & (interface of d as text) & " hostMiB=" & (host size of d as text) & linefeed
		end repeat
		return "drives (" & (n as text) & "):" & linefeed & out
	end tell
end run
