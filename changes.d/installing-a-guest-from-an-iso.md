---
priority: -30
---
 -  Kali and Windows have no ready-made base image to clone, so `wargame`
    now walks you through installing one yourself, once:

     -  `wargame profile init <template> --name <yours>` copies a bundled
        profile under your own name, with its own provisioning scripts that
        a later upgrade will not overwrite.
     -  `wargame profile create <yours> --iso <path>` builds a long-lived
        installation VM from an installer image, checking `--sha256` against
        it first if you pass one. `--from-vm <uuid>` starts from a VM you
        already installed instead; that VM is only ever copied, never
        altered. Re-running the command against the same source just
        re-prints what to prepare in the guest; pointing it at a different
        source is refused rather than quietly making a second VM.
     -  `wargame profile console <yours>` opens the installer window. Install
        the OS, create your user, and leave an SSH server, the guest agent
        and this host's public key in place. `profile create` prints the
        whole list, including the elevated access the build needs — sudo
        without a password on Kali, a local administrator account on Windows
        — because a build that installs packages over a non-interactive
        connection cannot answer a password prompt.
     -  `wargame profile build <yours>` then boots that VM, runs your
        profile's provisioning scripts, checks the tools the profile
        promises, shuts the guest down and publishes the result as the image
        every launch clones.

 -  Until that build succeeds the profile reports that it is still awaiting
    its install, and a build that fails leaves the previous image launchable.
    Neither the installation VM nor a published image is ever removed by
    `wargame prune`, and the installation VM is what the next rebuild starts
    from, so keep it.

 -  Windows 11 additionally wants TPM 2.0 and Secure Boot, which cannot be
    switched on for you; `wargame profile create` prints the one-time step to
    do by hand before you install. You supply the Windows installer and its
    licence.
