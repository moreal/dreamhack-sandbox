 -  `wargame doctor --profile <name>` judges free disk space the same way
    `wargame profile create` does: it measures the volume the guest disk
    actually lands on, and only fails below the working floor rather than
    demanding the profile's whole nominal disk. It also no longer suggests
    lowering `resources.disk_gib`, which does not help.

 -  A profile whose installation VM exists but whose OS install is unfinished
    is now told to open its console and finish it — naming that VM — instead
    of being sent to `wargame profile create`, which refuses.

 -  A profile whose image was deleted outside wargame reads `missing`, not
    `ready`. `wargame profiles` and `wargame doctor` ask the VM backend
    whether the image is really there, so a UTM image you removed in UTM
    shows up before a launch fails on it.

 -  When the backend cannot be asked at all — UTM has not been granted
    permission to be automated, or will not start — the image reads
    `unknown` rather than `missing`, and `wargame profiles` prints what the
    backend said and points at `wargame doctor`. It no longer tells you to
    rebuild an image that is sitting there untouched.

 -  When a launch fails, the next step offered is to look at what happened,
    not to delete the session. `wargame delete` is still there, second.
