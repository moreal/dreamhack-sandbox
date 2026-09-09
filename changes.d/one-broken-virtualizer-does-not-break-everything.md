 -  A virtualizer you do not have installed no longer breaks the commands
    that do not need it. `wargame prune` used to stop with an error on a
    machine without UTM, cleaning nothing up at all; it now sweeps every
    virtualizer it can reach, and names the one it could not, with the
    reason and what to do about it.

 -  `wargame sessions`, the session list in the interactive app and
    `wargame session inspect` behave the same way. One unreachable
    virtualizer used to hide every session, including the ones on the other
    virtualizer; now every session is still listed, and a row whose
    virtualizer could not confirm its state is marked with the reason
    underneath instead of showing a state nobody checked. Stopping,
    resuming or deleting such a session still reports the real failure.
