# Next steps (handoff for a fresh session)

State on 2026-09-06: the MVP is complete, merged to `main` (fast-forward from `feat/mvp`), and verified on real Tart VMs.
Read, in order: `docs/verification.md` (what works, what was not tested live), `DESIGN.md`
(architecture and verified external facts), `PLAN.md` (task map), `README.md` (user guide).

## Quick re-verification

```bash
nix flake check                                   # fmt, clippy, build + unit tests
nix develop -c scripts/e2e-smoke.sh macos-minimal-arm64   # real VM, needs the built profile image
```

## Open items, in suggested order

1. ~~Merge~~ — done 2026-09-06: `main` fast-forwarded to `feat/mvp` (28 commits), `nix flake check` green on `main`.
2. ~~Real Dreamhack run~~ — done 2026-09-06 with `dreamhack:15` (see `docs/verification.md`); three
   adapter fixes (bare-string response, `csrf_token` cookie, UUID filename) are committed (`dfc42c5`).
   `wargame auth login` must run in a real terminal (the password prompt needs a TTY).
3. ~~Reversing image~~ — done 2026-09-06: `wargame-profile-macos-re-arm64` built (≈2.7 min, 60 GiB disk),
   `dreamhack:15` launched into it, Ghidra headless + LLVM toolchain verified on PATH (see
   `docs/verification.md`, bugs 9–12 fixed in `083dc66`, `829de02`).
4. ~~Isolated networking~~ — done 2026-09-06 after the user installed the Softnet sudoers rule:
   default `isolated` launch boots in 20 s, guest gets a host-only IP, SSH works, guest has no
   internet, stop/resume/delete clean (see `docs/verification.md`).
5. ~~`wargame gui`~~ — done 2026-09-06: window opens, SSH keeps working, `stop`/`delete` with the window open leave
   nothing behind. Closing the window left the record stuck at `running` (bug 13, fixed: lifecycle ops now
   reconcile the record with `tart list`; see `docs/verification.md`).

All five items are done. The next milestone is open; candidates are multiple challenges per session, Linux
guest profiles, and a `wargame flag` submit flow.

## Host facts that matter

- `/etc/sudoers.d/wargame-softnet` grants passwordless sudo for `/opt/homebrew/bin/softnet` only, so the
  default isolated network works on this host; `doctor` reports `network:isolated ok`.
- The profile images `wargame-profile-macos-minimal-arm64` and `wargame-profile-macos-re-arm64` and
  the guest SSH key live under `~/Library/Application Support/wargame-vm`; a fresh `WARGAME_HOME`
  will not have that key.
- Use the `nix build` binary (`result/bin/wargame`) for real runs; `profile build` embeds its scripts,
  so no checkout path is needed at runtime.
- The user's own VM `kindle-vm` is unrelated to this project; never stop or delete it.
