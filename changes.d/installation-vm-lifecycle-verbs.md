 -  The installation VM a profile is built from now has verbs of its own.
    `wargame stop` and `wargame delete` only ever worked on challenge
    sessions, so the long-lived VM you install the OS into could be started
    by `wargame profile console` and then never stopped or removed from the
    CLI at all:

     -  `wargame profile stop <name>` stops it — waiting for it to really be
        down rather than trusting the request, and telling you which process
        to look at if something the tool did not start is still holding the
        VM.
     -  `wargame profile eject <name>` takes the installer disc out. Run it
        once the OS is installed, or the machine keeps booting the installer
        instead of the system on its disk. The VM has to be stopped first.
     -  `wargame profile delete <name>` (alias `rm`) removes the installation
        VM, every image its builds published, and its build record. It lists
        what it is about to delete and asks first (`--force` skips the
        question), refuses while any of those VMs is running, and never
        touches a VM the profile does not own.

 -  `wargame profile console` now says that it *starts* the VM and leaves it
    running, and names the commands that stop it and that take the installer
    disc out — which is what it should have said all along.
