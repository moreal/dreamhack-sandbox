 -  `wargame profile create` no longer refuses to build a guest that would
    have fitted. It used to demand as much free space as the profile's whole
    nominal disk — 80 GiB for both bundled Kali profiles — even though a
    guest disk is created empty and grows as it is used: a finished minimal
    Kali install takes a few GiB of that 80. It now refuses only below a
    working floor of 8 GiB, and when there is less room than the profile's
    disk asks for it says so and explains why it is going ahead anyway.

 -  It also measures the right filesystem. UTM stores its virtual machines
    inside its own sandbox container, not in your wargame home, so the check
    (and the message) now name the volume the disk will actually land on.

 -  When the check does refuse, it no longer suggests deleting an old
    session — a profile being created has none — and says what will and will
    not help instead.
