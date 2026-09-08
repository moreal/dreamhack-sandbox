-- Return the whole `qemu configuration` record as osascript flattens it to text.
-- usage: osascript dump-configuration.applescript <vm-name>
-- NOTE: the record is NOT complete. A drive's `source` and `guest size` are never
--   returned; only `id`, `removable`, `interface` and `host size` come back. Asking
--   for the missing ones inside the returned record fails with -1700. Keep the ISO
--   path and requested disk size in the app's own build record.
on run argv
	tell application "UTM"
		set vm to virtual machine named (item 1 of argv)
		set config to configuration of vm
		return config
	end tell
end run
