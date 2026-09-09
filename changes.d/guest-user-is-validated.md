 -  A profile whose `guest.user` is not a usable account name is now refused
    when it is read, naming the profile and the field. That name is pasted
    into the sudoers line `wargame profile create` asks you to run as root
    and into the `user@host` an `ssh` command carries, so a quote or a
    space in it used to break the command you were told to paste.

 -  A profile file that cannot be used no longer just disappears from
    `wargame profiles`: asking for it by name now tells you what is wrong
    with it instead of "profile not found".
