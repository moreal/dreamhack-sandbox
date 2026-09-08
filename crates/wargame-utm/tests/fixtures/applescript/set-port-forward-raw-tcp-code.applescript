-- Set a TCP port forward on an `emulated` (QEMU SLIRP) interface.
-- usage: osascript set-port-forward-raw-tcp-code.applescript <vm-name> <host-port> <guest-port>
-- NOTE: writing `protocol:TCP` fails with -1700 because the terminology name `TCP`
-- collides with the `serial interface` enumerator `tcp` (code "TcP "). Use the raw
-- four-char code «constant ****TcPp», or omit `protocol` entirely (defaults to TCP).
-- `protocol:UDP` works as written.
on run argv
	set vmName to item 1 of argv
	set hp to (item 2 of argv) as integer
	set gp to (item 3 of argv) as integer
	tell application "UTM"
		set vm to virtual machine named vmName
		set c to configuration of vm
		set n to index of item 1 of network interfaces of c
		set item 1 of network interfaces of c to {index:n, mode:emulated, port forwards:{{protocol:«constant ****TcPp», host port:hp, guest port:gp}}}
		update configuration of vm with c
		set c2 to configuration of vm
		set pf to item 1 of port forwards of item 1 of network interfaces of c2
		return "proto=" & ((protocol of pf) as text) & " " & ((host port of pf) as text) & "->" & ((guest port of pf) as text)
	end tell
end run
