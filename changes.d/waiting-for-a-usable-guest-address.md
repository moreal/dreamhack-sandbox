 -  **A guest is no longer declared reachable before it has an IPv4 address.**
    Linux guests pick up an IPv6 address about ten seconds before their IPv4
    lease arrives, and wargame used to take whichever came first — so
    `wargame launch` and `wargame profile build` went ahead, and then died
    copying the first file into the guest, after the whole boot. They now
    keep waiting, within the same boot timeout, and say plainly when the
    timeout runs out with only IPv6 in sight.

 -  Copying files to a guest reached over IPv6 works too: the address is now
    bracketed the way `scp` requires.
