 -  `wargame prune` now deletes only the VMs this wargame home actually
    made and wrote down. A VM that merely starts with the same name prefix
    — one belonging to another `WARGAME_HOME`, or one you made yourself —
    is listed as "not managed by this wargame home" and left alone instead
    of being offered up for deletion.

 -  A running VM is never deleted by `prune` either: it is reported with
    the `wargame delete` that does remove it. An ignored VM is reported with
    the way to remove it by hand, on the virtualizer it actually lives on,
    so you are never told a VM exists without being told what to do about
    it. `prune` also covers every virtualizer you use, which its help now
    says instead of naming Tart.

 -  `wargame delete` now finishes a delete that was interrupted — one that
    got as far as marking the session and then died, leaving a VM behind.
    It used to refuse such a session outright, which left the VM with no way
    to remove it from the CLI at all. It also works on a session a killed
    `wargame launch` left stuck mid-boot.

 -  After `prune` removes a VM, the session record stops claiming it: an
    interrupted delete is finished off and its record removed, while a
    failed session stays listed (its failure is still worth reading) with no
    VM attached.
