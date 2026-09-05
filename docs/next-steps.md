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
4. **Isolated networking**: add the Softnet sudoers rule printed by `wargame doctor`, then confirm
   `wargame launch … --network isolated` boots and SSH still works over the host-only bridge.
5. **`wargame gui`**: try it once by hand; it re-runs the VM with a Tart window and has only unit tests.

## Host facts that matter

- Passwordless sudo is not configured, so isolated mode is refused in preflight until step 4.
- The profile images `wargame-profile-macos-minimal-arm64` and `wargame-profile-macos-re-arm64` and
  the guest SSH key live under `~/Library/Application Support/wargame-vm`; a fresh `WARGAME_HOME`
  will not have that key.
- Use the `nix build` binary (`result/bin/wargame`) for real runs; `profile build` embeds its scripts,
  so no checkout path is needed at runtime.
- The user's own VM `kindle-vm` is unrelated to this project; never stop or delete it.
