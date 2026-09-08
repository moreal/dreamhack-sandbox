---
priority: -20
---
 -  Windows guests are driven through PowerShell. `wargame profile build`
    runs the profile's PowerShell provisioning scripts, and a script that
    asks for a reboot is honoured: the build waits for the guest to come
    back and carries on. `wargame ssh` into a Windows session drops you at a
    PowerShell prompt, and every other command that reaches into the guest
    uses the session's own shell rather than assuming a Unix one.

 -  Commands and paths survive the trip intact — spaces, backslashes,
    quotes, `$`, and non-ASCII text such as Korean all arrive unmangled, and
    the guest command's own exit code comes back to you.

 -  A Windows guest gets the same challenge workspace as every other guest:
    `incoming`, `work`, `notes` and `output`, with the challenge summary
    written alongside them.
