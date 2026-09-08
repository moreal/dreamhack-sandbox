-- Append a removable (CD/DVD) drive backed by an existing ISO.
-- usage: osascript add-removable-drive.applescript <vm-name-or-see-note> <absolute-iso-path>
-- NOTE: `POSIX file` must be evaluated OUTSIDE the `tell application "UTM"` block;
-- inside it, UTM resolves the path in its own sandbox and fails with -1728.
on run argv
	set vmName to item 1 of argv
	set isoFile to POSIX file (item 2 of argv)
	tell application "UTM"
		set vm to virtual machine named vmName
		set c to configuration of vm
		set dl to drives of c
		set drives of c to dl & {{removable:true, source:isoFile}}
		update configuration of vm with c
		set c2 to configuration of vm
		set out to ""
		repeat with d in (drives of c2)
			set out to out & "  id=" & (id of d) & " removable=" & (removable of d as text) & " iface=" & (interface of d as text) & " hostMiB=" & (host size of d as text) & linefeed
		end repeat
		return "drives now:" & linefeed & out
	end tell
end run
