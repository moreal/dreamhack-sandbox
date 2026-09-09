 -  `wargame prune` now deletes only the VMs this wargame home actually
    made and wrote down. A VM that merely starts with the same name prefix
    — one belonging to another `WARGAME_HOME`, or one you made yourself —
    is listed as "not managed by this wargame home" and left alone instead
    of being offered up for deletion.

 -  A running VM is never deleted by `prune` either: it is reported with
    the session to delete first. `prune` also covers every virtualizer you
    use, which its help now says instead of naming Tart.
