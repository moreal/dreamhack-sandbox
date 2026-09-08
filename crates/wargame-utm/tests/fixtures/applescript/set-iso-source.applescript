-- Swap the ISO backing an existing removable drive, addressed by its drive id.
-- usage: osascript set-iso-source.applescript <vm-name> <absolute-iso-path>
-- NOTE 1: `POSIX file` must be evaluated OUTSIDE the `tell application "UTM"` block
--   (-1728 otherwise).
-- NOTE 2: the drive `id` must be echoed back in the replacement record; omitting it
--   would create a new drive instead of replacing this one.
-- NOTE 3: the VM must be `stopped`, else -2700.
-- The readback shows `host size` in MiB, which is the only way to confirm from UTM
-- that an ISO of the expected size is attached — `source` itself is not readable.
on run argv
	set vmName to item 1 of argv
	set isoFile to POSIX file (item 2 of argv)
	tell application "UTM"
		set vm to virtual machine named vmName
		set config to configuration of vm
		set i to id of item 1 of drives of config
		set item 1 of drives of config to {id:i, source:isoFile}
		update configuration of vm with config
		set config2 to configuration of vm
		return "drive 1 id=" & i & " hostMiB=" & (host size of item 1 of drives of config2 as text)
	end tell
end run
