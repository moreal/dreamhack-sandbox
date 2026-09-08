---
priority: 10
---
 -  A challenge archive that tries to write outside the place it was unpacked
    is now refused whole, rather than partly extracted. Absolute paths, `..`
    traversal, drive letters, UNC prefixes, reserved Windows device names,
    trailing dots or spaces, control characters and links all disqualify the
    archive.

 -  When that happens the original file stays untouched in `incoming/`, no
    destination directory is created, and the reason is written to
    `notes/archive-refusals.txt` so you can decide what to do with it.
    Archives are still only ever unpacked inside the guest, never on your
    machine.
