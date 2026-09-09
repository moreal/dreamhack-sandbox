 -  `wargame profile build` now takes the installer disc out of the
    installation VM before it boots it. With the disc still in, the machine
    boots the installer again instead of the system you installed, so the
    build would sit there until its boot timeout ran out — and the image it
    published would have carried the disc too. The build says what it
    detached, and doing it twice is harmless.

 -  If the installation VM is still running when you build, the disc cannot
    be taken out (its settings can only be changed while it is stopped): the
    build says so and carries on, and tells you to stop it and build again.
