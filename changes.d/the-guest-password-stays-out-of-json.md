 -  `wargame profiles --json` no longer prints the guest password from the
    profile. It used to include the whole profile, password and all, so
    anything that captured that output — a log, a pipe, a CI artifact —
    captured the password with it. The password is still read from the
    profile and still used to build the image; it is simply never written
    out.
