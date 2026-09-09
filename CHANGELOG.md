# Changelog

All notable changes to `wargame-vm` are recorded here.

Entries for the next release are kept as [sacho](https://sacho.dev/) fragments
under `changes.d/` — one file per user-visible change — and compiled into the
generated region below. Never edit that region, or a released section, by
hand; add or edit a fragment instead.

This project has not made a versioned release yet. The `[Unreleased]`
sections after the generated region are frozen notes from the milestones that
predate sacho, kept for history.

<!-- sacho:unreleased:begin -->


Unreleased
----------

To be released.

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

 -  Guests that Tart cannot host are now run through UTM. You never choose
    the virtualizer yourself: each profile names the guest it wants and
    `wargame` picks the one that can run it, so macOS guests keep working
    exactly as before.

 -  `wargame profiles` shows what that resolved to — the guest system and
    architecture, the virtualizer and whether it virtualizes or emulates,
    and the profile's image state — and `wargame doctor` reports the health
    of every virtualizer you actually have profiles for. A session made with
    a profile whose virtualizer this build cannot drive still lists and
    still deletes; only starting it is refused.

 -  The same four answers appear wherever you pick or watch a profile: the
    interactive profile picker shows them next to each name, `wargame launch`
    prints what the profile resolved to and which image it will clone before
    it creates anything, and `wargame sessions` and `wargame session inspect`
    name the virtualizer each session actually runs on. That last one matters
    once macOS and Kali sessions sit in the same list — every stop, resume,
    delete or console goes to the session's own virtualizer, including for
    sessions created before wargame had more than one.

 -  Kali and Windows have no ready-made base image to clone, so `wargame`
    now walks you through installing one yourself, once:

     -  `wargame profile init <template> --name <yours>` copies a bundled
        profile under your own name, with its own provisioning scripts that
        a later upgrade will not overwrite.
     -  `wargame profile create <yours> --iso <path>` builds a long-lived
        installation VM from an installer image, checking `--sha256` against
        it first if you pass one. `--from-vm <uuid>` starts from a VM you
        already installed instead; that VM is only ever copied, never
        altered. Re-running the command against the same source just
        re-prints what to prepare in the guest; pointing it at a different
        source is refused rather than quietly making a second VM.
     -  `wargame profile console <yours>` opens the installer window. Install
        the OS, create your user, and leave an SSH server, the guest agent
        and this host's public key in place. `profile create` prints the
        whole list, including the elevated access the build needs — sudo
        without a password on Kali, a local administrator account on Windows
        — because a build that installs packages over a non-interactive
        connection cannot answer a password prompt.
     -  `wargame profile build <yours>` then boots that VM, runs your
        profile's provisioning scripts, checks the tools the profile
        promises, shuts the guest down and publishes the result as the image
        every launch clones.

 -  Until that build succeeds the profile reports that it is still awaiting
    its install, and a build that fails leaves the previous image launchable.
    Neither the installation VM nor a published image is ever removed by
    `wargame prune`, and the installation VM is what the next rebuild starts
    from, so keep it.

 -  The setup steps `wargame profile create` prints now also say that on
    Kali the guest agent is checked with `systemctl is-active`, not
    `is-enabled` — that unit is started by udev and reports `static`, which
    looks like a failure and is not — and note that an emulated guest uses
    every core it is given, so two installs or builds at once on one machine
    make both crawl. The profiles' own defaults are unchanged.

 -  Windows 11 additionally wants TPM 2.0 and Secure Boot, which cannot be
    switched on for you; `wargame profile create` prints the one-time step to
    do by hand before you install. You supply the Windows installer and its
    licence.

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

 -  `wargame doctor --profile <name>` checks only what that one profile needs
    — its virtualizer, execution mode, network, image and guest tooling —
    instead of everything on the host. Useful when one profile misbehaves and
    the rest are fine.

 -  **A guest is no longer declared reachable before it has an IPv4 address.**
    Linux guests pick up an IPv6 address about ten seconds before their IPv4
    lease arrives, and wargame used to take whichever came first — so
    `wargame launch` and `wargame profile build` went ahead, and then died
    copying the first file into the guest, after the whole boot. They now
    keep waiting, within the same boot timeout, and say plainly when the
    timeout runs out with only IPv6 in sight.

 -  Copying files to a guest reached over IPv6 works too: the address is now
    bracketed the way `scp` requires.

 -  **A profile that already has an image can be built again.** The second
    `wargame profile build` of any UTM profile failed at the very last step,
    after the guest had been provisioned and verified in full — UTM names
    each virtual machine's folder after the machine, and refused to create
    the new image under a name the old one still had. Every build now
    publishes under a name of its own, the build record is what identifies
    the image, and the image being replaced is deleted only once the record
    points at the new one.

 -  A publish that fails no longer leaves a virtual machine behind that
    nothing knows about. Whatever UTM registered is written into the build
    record first, so `wargame prune` leaves it alone and the next successful
    build cleans it up.

 -  A profile whose `guest.user` is not a usable account name is now refused
    when it is read, naming the profile and the field. That name is pasted
    into the sudoers line `wargame profile create` asks you to run as root
    and into the `user@host` an `ssh` command carries, so a quote or a
    space in it used to break the command you were told to paste.

 -  A profile file that cannot be used no longer just disappears from
    `wargame profiles`: asking for it by name now tells you what is wrong
    with it instead of "profile not found".

 -  Challenge files are now checksummed again inside the guest. The host
    records a digest for everything it copies over and the guest recomputes
    each one before anything is unpacked, so a truncated or corrupted
    transfer is caught in the guest rather than silently solved around.

 -  **Deleting a UTM session, profile or image now removes only the virtual
    machine you named.** It removed *every* virtual machine UTM had — yours
    included, whether or not wargame created it — and still reported success.
    `wargame delete`, `wargame prune`, `wargame profile delete` and a
    rebuild's cleanup of the image it replaced were all affected. If you ran
    any of them against a UTM profile on an earlier build, check UTM: what it
    deleted is gone from disk and cannot be recovered from here.

 -  Every other UTM operation — starting, stopping, changing a VM's hardware,
    network or display, attaching or ejecting an installer disc, taking a
    copy — now addresses one virtual machine by its identifier as well, so
    none of them can act on your whole library either.

 -  **Every copy of a UTM virtual machine now gets its own network address.**
    Copies inherited the original's MAC address, so two sessions launched
    from the same built image came up as the same machine on the same
    network: they answered to one IP, and a file copied into one could land
    in the other. Each copy — a session's VM, a published image, an
    installation VM adopted from a VM you already had — now gets a fresh,
    locally administered address before it is ever started.

 -  The installation VM a profile is built from now has verbs of its own.
    `wargame stop` and `wargame delete` only ever worked on challenge
    sessions, so the long-lived VM you install the OS into could be started
    by `wargame profile console` and then never stopped or removed from the
    CLI at all:

     -  `wargame profile stop <name>` stops it — waiting for it to really be
        down rather than trusting the request, and telling you which process
        to look at if something the tool did not start is still holding the
        VM.
     -  `wargame profile eject <name>` takes the installer disc out. Run it
        once the OS is installed, or the machine keeps booting the installer
        instead of the system on its disk. The VM has to be stopped first.
     -  `wargame profile delete <name>` (alias `rm`) removes the installation
        VM, every image its builds published, and its build record. It lists
        what it is about to delete and asks first (`--force` skips the
        question), refuses while any of those VMs is running, and never
        touches a VM the profile does not own.

 -  `wargame profile console` now says that it *starts* the VM and leaves it
    running, and names the commands that stop it and that take the installer
    disc out — which is what it should have said all along.

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

 -  `wargame doctor` no longer reports a profile you have never built as a
    failure. A Mac with no Kali or Windows installation ISO is not a broken
    machine, and three permanent ✗ rows for the templates that ship with
    wargame only taught you to ignore ✗. Those rows now read as "not set up
    yet (nothing is wrong)" and still tell you the command that would set
    them up.

 -  A profile you *did* build is still a failure when its image has gone
    missing, when the profile changed since, or when a build stopped
    partway: nothing was swept under the rug. `doctor --json` says which is
    which in a new `status` field (`pass`, `fail` or `not-set-up`).

 -  **`wargame gui` no longer reboots a running session.** On UTM the window
    is open from the moment the VM starts, but `gui` stopped and restarted
    the machine anyway — it came back with everything you were running in the
    guest gone, and said "SSH keeps working" while the guest's uptime was
    zero. It now just brings the window to the front. A stopped session is
    still started with a window, as before.

 -  `wargame profile build` now takes the installer disc out of the
    installation VM before it boots it. With the disc still in, the machine
    boots the installer again instead of the system you installed, so the
    build would sit there until its boot timeout ran out — and the image it
    published would have carried the disc too. The build says what it
    detached, and doing it twice is harmless.

 -  If the installation VM is still running when you build, the disc cannot
    be taken out (its settings can only be changed while it is stopped): the
    build says so and carries on, and tells you to stop it and build again.

 -  Nothing is ever detached from a VM you brought yourself. A profile
    created with `--from-vm` never had a disc put in by wargame, so its
    drives are left exactly as you set them up. And if a machine reports
    *every* drive as removable — which can happen to a VM built elsewhere —
    wargame refuses to detach anything rather than leave it with no disk to
    boot: the build carries on and says so, and `wargame profile eject`
    stops with the reason and points you at UTM's own drive settings.

 -  `wargame profile create` no longer refuses to build a guest that would
    have fitted. It used to demand as much free space as the profile's whole
    nominal disk — 80 GiB for both bundled Kali profiles — even though a
    guest disk is created empty and grows as it is used: a finished minimal
    Kali install takes a few GiB of that 80. It now refuses only below a
    working floor of 8 GiB, and when there is less room than the profile's
    disk asks for it says so and explains why it is going ahead anyway.

 -  It also measures the right filesystem. UTM stores its virtual machines
    inside its own sandbox container, not in your wargame home, so the check
    (and the message) now name the volume the disk will actually land on.

 -  When the check does refuse, it no longer suggests deleting an old
    session — a profile being created has none — and says what will and will
    not help instead.

 -  `wargame prune` now deletes only the VMs this wargame home actually
    made and wrote down. A VM that merely starts with the same name prefix
    — one belonging to another `WARGAME_HOME`, or one you made yourself —
    is listed as "not managed by this wargame home" and left alone instead
    of being offered up for deletion.

 -  A running VM is never deleted by `prune` either: it is reported with
    the `wargame delete` that does remove it. An ignored VM is reported with
    the way to remove it by hand, on the virtualizer it actually lives on,
    so you are never told a VM exists without being told what to do about
    it. `prune` also covers every virtualizer you use, which its help now
    says instead of naming Tart.

 -  `wargame delete` now finishes a delete that was interrupted — one that
    got as far as marking the session and then died, leaving a VM behind.
    It used to refuse such a session outright, which left the VM with no way
    to remove it from the CLI at all. It also works on a session a killed
    `wargame launch` left stuck mid-boot.

 -  After `prune` removes a VM, the session record stops claiming it: an
    interrupted delete is finished off and its record removed, while a
    failed session stays listed (its failure is still worth reading) with no
    VM attached.

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

 -  When a guest never becomes reachable, the error now says what was
    actually missing — the SSH service, key authentication, or the shell
    itself — along with how long it waited, the timeout in force and how to
    raise it. A guest whose SSH service has not started is no longer reported
    as ready.

<!-- sacho:unreleased:end -->

## [Unreleased] — Usability pass (2026-09-08)

### Changed

- **Session references**: every command that takes a session id now also
  accepts a unique prefix or substring of the id or challenge title, and
  (except `delete`) may omit it to mean the most recent session. Ambiguous
  references list their candidates (`WargameError::AmbiguousSession`).
- **Next-step hints**: `launch` ends with an elapsed time and the four
  follow-up commands; `search`, `open`, `sessions`, `session inspect`,
  `stop`, `resume`, `keep`, `profiles`, `profile build` and `auth` each
  end with a `Next:` line. `stop`/`resume` on a session already in that
  state succeed with a message; state errors read "session is stopped and
  cannot become running" and carry a state-specific hint; deleting a kept
  session raises `SessionKept` naming `--force`.
- **Tables**: `sessions` gains `keep`/`age` columns, a summary line and an
  empty-state hint; `search` explains no-result pages; `doctor` renders
  ✓/✗ with a failure summary; `profiles` marks the default.
- **Help**: every flag is documented, defaults are visible, `--network` is
  an enum (case-insensitive), global flags sort last, `wargame --help`
  carries a quick-start block, `profile build` defaults to the configured
  profile, `-y`/`-f` short flags, aliases `find`, `show`/`info`, `run`,
  `ls`/`ps`, `start`, `rm`. New `wargame completions <shell>`.
- **TUI**: Ctrl+C quits anywhere; the footer lists only keys that do
  something on the current screen; "Searching…"/"Opening…" indicators;
  `[`/`]` page through results; Enter launches from the problem screen;
  the launch log is coloured and auto-scrolls; the sessions screen is a
  real table (header, keep, age, state colours, empty state) with `g`
  gui, `k` keep, `R` refresh, and state-aware explanations instead of
  transition errors; async errors show their hint.
- Hints no longer carry a `Run:` prefix; an unwritable `--home` reports
  the path and the fix.

## [Unreleased] — MVP

### Added

- **Core domain & ports** (`wargame-core`): `ChallengeRef`/`Challenge`,
  `Profile`, `Session`/`SessionState` with a validated state machine,
  provider/backend compatibility checks, `WargameError` with per-error
  hints, and the `WargameProvider`/`VmProvider`/`GuestTransport`/
  `CredentialStore`/`SessionRepository` port traits.
- **Security & infra** (`wargame-core`): filename sanitization, streaming
  SHA-256 staging with atomic rename-on-success (partial downloads never
  get a final name), a tracing redaction layer, a `ProcessRunner`
  abstraction (argv arrays only, never shell strings), macOS Keychain and
  plaintext-file credential stores, a JSON session repository, and an SSH
  guest transport — plus a full set of in-memory fakes for testing.
- **Dreamhack provider** (`wargame-dreamhack`): search/detail/download
  against the Dreamhack JSON API with session-cookie auth, CSRF headers,
  and response-shape fixtures.
- **Tart provider** (`wargame-tart`): the Tart CLI driven entirely through
  argv arrays — clone/set/run/ip/stop/delete/list — plus a profile image
  builder that provisions a fresh clone over a temporary password login and
  installs the per-install guest key.
- **Application services** (`wargame-core::app`): `launch` (the full
  search-free-download-boot-provision pipeline), session lifecycle
  (`stop`/`resume`/`delete`/`keep`/`prune`), console access, and `doctor`.
- **Profiles & guest bootstrap**: `macos-minimal-arm64` and
  `macos-re-arm64` profile definitions, their provisioning scripts, and
  `guest/bootstrap.sh` (guest-side-only archive extraction, zip-slip-safe,
  never executes downloaded content).
- **CLI** (`wargame-cli`): the full `wargame` subcommand surface (`search`,
  `open`, `launch`, `sessions`, `session inspect`, `ssh`, `gui`, `stop`,
  `resume`, `keep`, `delete`, `prune`, `doctor`, `profiles`,
  `profile build`, `auth login|status|logout`), a `FixtureProvider` for
  network-free testing, and `--json` output throughout.
- **TUI** (`wargame-cli::tui`): an Elm-style ratatui interface over the
  same application services as the CLI — search → problem detail → launch →
  sessions — with no TUI-only business logic.
- **Docs & tooling** (this change): `README.md` (install, first-run walk-
  through, the isolated-vs-NAT networking model, security summary,
  troubleshooting), `scripts/e2e-smoke.sh` (a real-VM smoke test using the
  fixture provider), `crates/wargame-tart/tests/e2e.rs` (an `#[ignore]`d,
  `WARGAME_E2E=1`-gated M0 proof against a real Tart VM), and this
  changelog.

### Nix

- `flake.nix` provides `packages.default`/`apps.default` (the wrapped
  `wargame` binary, with `tart`/`sshpass`/`openssh` resolved from `PATH` at
  run time), `devShells.default`, and `checks.{build,fmt,clippy,tests}` for
  `aarch64-darwin` and `x86_64-linux`.
