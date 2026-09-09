 -  **`wargame gui` no longer reboots a running session.** On UTM the window
    is open from the moment the VM starts, but `gui` stopped and restarted
    the machine anyway — it came back with everything you were running in the
    guest gone, and said "SSH keeps working" while the guest's uptime was
    zero. It now just brings the window to the front. A stopped session is
    still started with a window, as before.
