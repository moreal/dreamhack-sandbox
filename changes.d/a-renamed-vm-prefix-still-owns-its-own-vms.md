 -  Changing the VM name prefix no longer puts your own installation VM and
    built images out of reach. With a custom `[vm.utm].vm_prefix` and the
    default build prefix, `wargame profile stop|eject|delete` — and the
    build's own shutdown — refused to touch the VM they had just made,
    saying it was not wargame's. Both prefixes now count as this app's.

 -  A profile YAML written by hand is now checked for a usable
    `metadata.name` when it is read, the same way `wargame profile init`
    checks the name you give it. A name like `../x` used to be accepted and
    then used as a directory name for that profile's build record and logs.
