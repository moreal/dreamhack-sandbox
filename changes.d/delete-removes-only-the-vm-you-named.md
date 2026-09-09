 -  **Deleting a UTM session, profile or image now removes only the virtual
    machine you named.** It removed *every* virtual machine UTM had — yours
    included, whether or not wargame created it — and still reported success.
    `wargame delete`, `wargame prune`, `wargame profile delete` and a
    rebuild's cleanup of the image it replaced were all affected. If you ran
    any of them against a UTM profile on an earlier build, check UTM: what it
    deleted is gone from disk and cannot be recovered from here.

 -  Every other UTM operation — starting, stopping, changing a VM's hardware,
    network or display, attaching or ejecting an installer disc, taking a
    copy — now addresses one virtual machine by its identifier as well, so
    none of them can act on your whole library either.
