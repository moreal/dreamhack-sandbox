---
priority: -50
---
 -  Challenges can now be solved on Kali Linux and Windows guests, not only
    macOS. Three profiles ship with the binary: `kali-arm64`, `kali-x86_64`
    and `windows-x86_64`.

 -  A profile now says which processor architecture its guest uses. An arm64
    guest runs virtualized at close to native speed; an `x86_64` guest runs
    emulated, so expect it to be markedly slower and to need far longer to
    boot the first time (30 minutes is the default patience, raise it with
    `[vm.utm].boot_timeout_secs`).

 -  The Kali and Windows profiles have not yet been driven end to end
    against a fully installed guest. Treat them as ready to try, not as
    proven.
