 -  `wargame doctor` no longer reports a profile you have never built as a
    failure. A Mac with no Kali or Windows installation ISO is not a broken
    machine, and three permanent ✗ rows for the templates that ship with
    wargame only taught you to ignore ✗. Those rows now read as "not set up
    yet (nothing is wrong)" and still tell you the command that would set
    them up.

 -  A profile you *did* build is still a failure when its image has gone
    missing, when the profile changed since, or when a build stopped
    partway: nothing was swept under the rug. `doctor --json` says which is
    which in a new `status` field (`pass`, `fail` or `not-set-up`).
