---
priority: -25
---
 -  The Kali and Windows profiles now install a real tool set instead of an
    empty one. Kali gets Python 3 with pip and venv, Git, curl, unzip, file,
    GDB, binutils and build-essential from its own package repositories;
    Windows gets OpenSSH Server, Python 3, Git and 7-Zip through winget, with
    PowerShell left as the shell every wargame command is phrased for. Each
    script says where every tool comes from and how it is checked, logs every
    step with its exit code, and can be re-run against an image it already
    provisioned. A tool that will not install fails the build rather than
    being quietly skipped, and no image is published until every check
    passes.

 -  Adding your own tools is the same flow: drop a script under
    `profiles/scripts/`, list it in the profile's `provision.scripts` after
    the baseline one, add a command per tool to `tools.verify`, and run
    `wargame profile build <profile>` again. A commented starting point ships
    at `profiles/scripts/custom/my-tools.sh` (and `.ps1` for Windows) with
    the rules a provisioning script has to keep.

 -  Anything under `profiles/` that you have edited now survives an upgrade,
    including a change to one of the bundled scripts. Previously only profile
    files were protected and the bundled scripts were rewritten on every run.
    A file you have not touched still follows the tool, so an unedited script
    is never left behind at an old version.
