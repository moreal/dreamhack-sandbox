 -  **`wargame profile create` now tells you to take the installer disc out,
    and when.** The steps it prints went straight from setting up SSH to
    `wargame profile build`, so nothing warned you that the machine still
    boots the disc: an installation VM rebooted at the end of its install
    lands back in the installer, and an unattended disc starts the whole
    install over on the disk it has just written. The checklist now says to
    shut the guest down from inside the installer, run
    `wargame profile eject <profile>` before it boots again, and only then
    boot the installed system to prepare it. It also says that
    `wargame profile build` ejects the disc as well, so a skipped step is
    recoverable. `wargame profile console` and the README describe the same
    order.

 -  **A profile whose installer disc you have ejected no longer reads
    `awaiting-install`.** `wargame profiles` shows `media ejected (not built)`
    for it, and `wargame doctor` offers `wargame profile build <profile>`
    instead of sending you back to the console to finish an OS install you
    already finished. The wording stops at what is actually known — the disc is
    out — rather than claiming the guest was checked; a profile whose disc is
    still in is still `awaiting-install`.
