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
