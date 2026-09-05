# MVP.md — Wargame VM Orchestrator

> Working title: **wargame-vm**  
> Status: Draft PRD  
> Initial target: **macOS host on Apple Silicon → macOS guest VM**  
> Primary provider: **Dreamhack Wargame**  
> Primary user: single local user

---

## 1. Summary

`wargame-vm` is a local-first wargame workspace orchestrator.

Its purpose is to let a user search for a security wargame problem, choose an execution environment, and immediately receive a disposable virtual machine containing:

- the selected problem files,
- a predictable workspace,
- preinstalled analysis/development tools,
- bootstrap scripts,
- an optional terminal or graphical VM session,
- and enough lifecycle management to discard or preserve the environment after solving the problem.

The central product principle is:

> **Untrusted challenge artifacts should be handled as data on the host and executed only inside a disposable VM.**

The first implementation supports only an Apple Silicon Mac host and macOS guest VMs. The architecture must nevertheless make the execution backend replaceable so that Linux VMs, local emulated environments, and remote x86-64 Windows Hyper-V hosts can be added later without redesigning the product.

---

## 2. Problem

Security wargames often distribute binaries, archives, scripts, documents, or other artifacts whose behavior is intentionally unusual and may contain exploitable vulnerabilities.

A normal workflow introduces friction and risk:

1. Find a problem in the browser.
2. Read its description.
3. Download one or more files to the host.
4. Decide whether they are safe to open.
5. Create or start an analysis VM.
6. Install missing tools.
7. Transfer the challenge files.
8. Re-create directory layouts and shell configuration.
9. Start Ghidra, Python, a debugger, or other tools.
10. Clean the VM afterward.

This makes environment management compete with the actual learning task.

There is also an architecture mismatch on Apple Silicon. A local Mac is excellent for ARM64 macOS/Linux virtualization, but some wargames require an x86-64 Windows environment. Those workloads should eventually be schedulable on a remote x86-64 machine instead of forcing the local Mac to emulate every target architecture.

---

## 3. Product Goal

Reduce the workflow from “find problem → prepare machine → transfer files → configure tools → solve” to:

1. Search/select a problem.
2. Select an environment profile.
3. Launch.
4. Solve inside the VM.
5. Discard or keep the session.

A successful launch should require no manual package installation and no host-side extraction or execution of challenge binaries.

---

## 4. Product Principles

### 4.1 Untrusted by default

Every challenge artifact is considered untrusted regardless of file extension or category.

The host may:

- download raw bytes,
- calculate a cryptographic hash,
- store the file temporarily,
- copy the file into a guest.

The host must not automatically:

- execute the file,
- load it as a library,
- run file-specific helper programs,
- extract archives,
- mount disk images,
- generate previews,
- invoke Quick Look,
- or pass the artifact to an analysis tool.

Archive extraction and content inspection happen in the VM.

### 4.2 Disposable environments

A challenge session is created from an immutable or reusable prepared image and should be cheap to discard.

The user should never feel pressure to keep a “pet VM” merely because configuring it was expensive.

### 4.3 Reproducible tool profiles

Python, uv, Ghidra, debuggers, compilers, package managers, scripts, and configuration should be expressed as an image/profile definition rather than installed manually after every launch.

### 4.4 Provider independence

Dreamhack is the first wargame source, not the core domain model.

Likewise, a local macOS VM is the first VM backend, not the core execution model.

### 4.5 Architecture is explicit

The product must never silently run a challenge in an incompatible environment.

A problem/session records at least:

- guest OS,
- guest CPU architecture,
- execution backend,
- image/profile,
- network mode.

### 4.6 Credentials stay outside the challenge guest

Dreamhack credentials/session cookies and remote-host administrative credentials remain on the trusted side.

A malicious challenge running in a guest must not automatically gain access to:

- Dreamhack authentication,
- SSH keys,
- Windows administrator credentials,
- host Keychain contents,
- unrelated host files.

---

## 5. First MVP Scope

### Included

The first MVP supports:

- macOS as the host OS,
- Apple Silicon (`arm64`) as the host architecture,
- macOS guest VMs,
- Dreamhack Wargame problem search,
- problem detail display,
- authenticated problem-file download when required,
- reusable macOS environment profiles,
- preinstalled tools such as Python, uv, and Ghidra,
- ephemeral VM creation from a prepared image,
- one-way problem artifact transfer into the VM,
- bootstrap script execution inside the VM,
- terminal access over SSH,
- graphical VM access,
- session stop/delete/keep,
- basic resource configuration,
- offline and NAT network modes,
- local configuration and credential storage.

### Explicitly not included in MVP

The first MVP does **not** need to support:

- local Windows guests,
- local x86-64 emulation,
- Linux guests,
- remote Hyper-V execution,
- VNC/RDP gateway management,
- multiple wargame providers,
- cloud VM providers,
- team/multi-user operation,
- collaborative sessions,
- automatic solving,
- automatic exploit generation,
- challenge write-up generation,
- malware-analysis-grade containment guarantees,
- snapshot trees or complex checkpoint UIs,
- public image registries managed by this product,
- browser extensions.

These are follow-up capabilities and must not complicate the first implementation more than necessary.

---

## 6. Recommended MVP Product Surface

The first interface should be a **TUI backed by a CLI/core library**.

Reasons:

- wargame work already happens heavily in terminals,
- SSH into the guest is naturally terminal-oriented,
- it is considerably smaller in scope than a desktop GUI,
- the same core can later drive a GUI,
- commands remain scriptable and testable.

Example:

```text
$ wargame

┌ Dreamhack ──────────────────────────────────────────┐
│ Search: [ out_of_bound___________________________ ] │
├─────────────────────────────────────────────────────┤
│ > out_of_bound      pwnable     Beginner            │
│   basic_exploitation_000        pwnable              │
│   ...                                                │
└─────────────────────────────────────────────────────┘

Enter: open   /: search   q: quit
```

Problem screen:

```text
out_of_bound
Category: pwnable
Provider: Dreamhack

Environment
> macos-re-arm64
  macos-minimal-arm64

Network
> isolated
  nat

Files
- challenge.zip
- README.txt

[ Launch VM ]
```

---

## 7. Core User Flow

### 7.1 First-time setup

1. Install `wargame-vm`.
2. Run `wargame doctor`.
3. The application verifies:
   - Apple Silicon,
   - supported macOS version,
   - VM backend availability,
   - disk space,
   - required permissions.
4. Configure Dreamhack authentication.
5. Pull/build at least one base profile.
6. Run a disposable smoke-test VM.

Expected result:

```text
✓ Host: macOS / arm64
✓ VM backend: available
✓ Profile: macos-re-arm64
✓ Dreamhack: authenticated
✓ SSH bootstrap: working
```

### 7.2 Solve a challenge

1. Run `wargame`.
2. Search Dreamhack.
3. Select a problem.
4. Read description and metadata.
5. Select an environment profile.
6. Select networking mode.
7. Press Launch.
8. Orchestrator creates a fresh VM clone.
9. Challenge files are downloaded as opaque bytes.
10. Files are copied into the guest without host-side extraction.
11. Guest bootstrap prepares the workspace.
12. User enters via SSH or GUI.
13. User solves the problem.
14. User chooses:
    - discard,
    - stop and keep,
    - export notes/artifacts later.

### 7.3 Reopen a preserved session

```text
$ wargame sessions

NAME                  PROBLEM        PROFILE             STATE
dh-out-of-bound-a12   out_of_bound   macos-re-arm64      stopped

$ wargame resume dh-out-of-bound-a12
```

---

## 8. Challenge Source Model

The core should use a provider-neutral model.

```text
Challenge
├─ provider
├─ id
├─ slug
├─ title
├─ description
├─ categories[]
├─ difficulty?
├─ tags[]
├─ assets[]
├─ remote_endpoints[]
└─ source_url
```

An asset contains:

```text
ChallengeAsset
├─ id
├─ filename
├─ size?
├─ download_descriptor
└─ expected_hash?
```

The core must not depend on Dreamhack-specific HTML/API response types.

---

## 9. Wargame Provider Interface

Conceptual interface:

```text
WargameProvider
├─ search(query, filters) -> ChallengeSummary[]
├─ get_challenge(id) -> Challenge
├─ authenticate() -> AuthState
├─ download_asset(challenge, asset, sink) -> DownloadResult
└─ healthcheck() -> ProviderStatus
```

### Dreamhack adapter

The Dreamhack implementation is responsible for:

- search,
- pagination,
- metadata normalization,
- authentication,
- problem detail retrieval,
- raw file download.

The integration must be isolated behind an adapter because external site endpoints, authentication flows, and page structure can change independently of the VM orchestration logic.

The MVP should not make a stable public Dreamhack API a hard assumption.

### Authentication

Preferred MVP behavior:

- never ask the user to store a Dreamhack password in plaintext,
- keep authentication material in macOS Keychain,
- only the Dreamhack adapter can read it,
- never forward the credential/session cookie into a challenge VM,
- redact tokens from logs.

If browser-based authentication can be integrated cleanly, it can replace manual token/session configuration later.

---

## 10. Artifact Download and Injection

This is a security-sensitive boundary.

### Required pipeline

```text
Dreamhack
   │
   │ authenticated HTTPS
   ▼
Host download adapter
   │
   │ raw byte stream only
   ▼
0600 temporary staging file
   │
   ├── calculate SHA-256
   │
   └── NO extraction / preview / execution
   ▼
Guest transfer channel
   │
   ▼
~/wargame/<challenge-slug>/incoming/
   │
   ▼
Guest-side bootstrap/extraction
```

### Rules

- Preserve the server-provided filename after sanitizing path traversal components.
- Never trust filenames such as `../../foo`.
- Never automatically extract an archive on the host.
- Never mount downloaded disk images on the host.
- The staging directory is private to the current user.
- Delete staging files after successful guest transfer unless debugging is explicitly enabled.
- Record SHA-256 in session metadata.
- A failed download must never leave a partial file pretending to be complete.

---

## 11. VM Backend Abstraction

Conceptual interface:

```text
VmProvider
├─ capabilities() -> ProviderCapabilities
├─ ensure_image(profile) -> ImageRef
├─ create_session(spec) -> VmRef
├─ start(vm) -> RuntimeInfo
├─ stop(vm)
├─ delete(vm)
├─ status(vm) -> VmStatus
├─ exec(vm, command) -> ExecResult
├─ copy_in(vm, source, destination)
├─ open_console(vm)
└─ inspect(vm) -> VmInfo
```

A backend advertises capabilities instead of the core guessing them.

Example:

```text
ProviderCapabilities
├─ guest_os: [macos]
├─ guest_arch: [arm64]
├─ graphical_console: true
├─ ssh: true
├─ snapshots: backend-dependent
├─ copy_in: true
└─ remote: false
```

Future Hyper-V:

```text
ProviderCapabilities
├─ guest_os: [windows, linux]
├─ guest_arch: [x86_64]
├─ graphical_console: true
├─ powershell_direct: true
├─ checkpoints: true
├─ copy_in: true
└─ remote: true
```

---

## 12. Local macOS Backend

### Recommended MVP backend: Tart

For the first implementation, use Tart rather than directly wrapping every API in Apple Virtualization.framework.

Why:

- it already targets Apple Silicon virtualization,
- it supports macOS VM creation and execution,
- it supports cloning prepared images,
- images can be pulled/pushed through OCI registries,
- Packer integrations can provision tools into images,
- it leaves the application free to focus on the wargame workflow.

The core must still depend on a `VmProvider` abstraction so Tart can later be replaced or supplemented.

### Direct Virtualization.framework backend

A native backend remains a future option when tighter control is valuable, for example:

- custom VM window integration,
- application-specific device configuration,
- custom networking,
- deeper lifecycle integration,
- eliminating an external CLI dependency.

This should not be required for MVP.

---

## 13. Image and Profile Model

A **base image** represents an OS installation.

A **profile** represents the tools and policy wanted for a class of challenge.

Example profile file:

```yaml
apiVersion: wargame.vm/v1
kind: Profile

metadata:
  name: macos-re-arm64

guest:
  os: macos
  arch: arm64

image:
  base: ghcr.io/cirruslabs/macos-tahoe-base:latest

resources:
  cpu: 4
  memory: 8GiB
  disk: 60GiB

tools:
  brew:
    - ghidra
    - python
    - llvm
    - binutils
  uv: true

provision:
  scripts:
    - scripts/macos/common.sh
    - scripts/macos/reversing.sh

workspace:
  root: /Users/admin/wargame
```

The exact schema may evolve, but profile definitions must be declarative and versionable.

### Suggested initial profiles

#### `macos-minimal-arm64`

- shell utilities,
- Git,
- Python,
- uv,
- curl/wget,
- editor utilities.

#### `macos-re-arm64`

Everything in minimal plus:

- Ghidra,
- LLVM tooling,
- binary utilities,
- hex/file inspection utilities,
- Java runtime required by Ghidra.

Do not create many profiles in MVP. Two are sufficient to prove the model.

---

## 14. Image Build Workflow

Prepared images must be built **before** untrusted challenge files are introduced.

```text
Known base macOS image
        │
        ▼
Provision image
  - OS settings
  - SSH
  - package manager
  - Python / uv
  - Ghidra
  - helper scripts
        │
        ▼
Validate
        │
        ▼
Immutable/reusable profile image
        │
        ├── clone challenge A
        ├── clone challenge B
        └── clone challenge C
```

Image definitions should live in source control.

A profile image should be rebuildable from:

- base image reference,
- profile definition,
- provisioning scripts,
- application version.

The session itself should not mutate the canonical profile image.

---

## 15. Guest Bootstrap

Every created VM receives a small trusted bootstrap script.

Responsibilities:

1. Create the challenge workspace.
2. Place incoming artifacts under `incoming/`.
3. Optionally unpack them **inside the guest**.
4. Create convenient directories:

```text
~/wargame/<slug>/
├─ incoming/
├─ work/
├─ notes/
└─ output/
```

5. Write metadata such as:

```json
{
  "provider": "dreamhack",
  "challenge_id": "...",
  "title": "...",
  "download_sha256": {
    "challenge.zip": "..."
  }
}
```

6. Print a concise welcome message.
7. Never execute the downloaded challenge automatically.

The bootstrap may extract known archive types after transfer if explicitly configured, but extraction must remain guest-side.

---

## 16. Session Model

A session is a concrete VM created for one challenge.

```text
Session
├─ id
├─ display_name
├─ provider
├─ challenge_id
├─ challenge_snapshot
├─ vm_provider
├─ image_profile
├─ guest_os
├─ guest_arch
├─ network_mode
├─ resources
├─ created_at
├─ state
├─ vm_ref
├─ assets[]
└─ keep_policy
```

Session states:

```text
creating
downloading
booting
provisioning
ready
running
stopped
failed
deleting
deleted
```

The state machine should be explicit enough that interrupted launches can be diagnosed and cleaned up.

---

## 17. Session Lifecycle Commands

Suggested CLI:

```bash
wargame search <query>
wargame open <provider:id>
wargame launch <provider:id> --profile macos-re-arm64
wargame sessions
wargame ssh <session>
wargame gui <session>
wargame stop <session>
wargame resume <session>
wargame delete <session>
wargame doctor
wargame profiles
wargame profile build <name>
```

TUI actions map to the same core operations.

There should be no TUI-only business logic.

---

## 18. Networking

Networking is a major part of the threat model.

### MVP modes

#### `isolated`

- VM receives no external networking.
- Preferred default for offline reversing/file-analysis challenges.

#### `nat`

- VM can reach external networks.
- Needed for remote challenge servers, package debugging, or web problems.
- The UI clearly indicates that networking is enabled.

Example launch prompt:

```text
Network: NAT

The challenge VM will be able to initiate network connections.
Continue? [y/N]
```

### Not required in MVP

A robust policy engine that selectively blocks:

- host services,
- LAN devices,
- RFC1918 ranges,
- metadata endpoints,
- selected domains,

is valuable but should be a later security hardening feature unless it can be implemented without destabilizing the MVP.

---

## 19. Host Isolation Requirements

MVP must follow these defaults:

- no host home-directory mount,
- no automatic shared folder,
- no automatic host clipboard sharing when avoidable,
- no host SSH-agent forwarding,
- no host Keychain forwarding,
- no Dreamhack credential forwarding,
- no remote Hyper-V credential forwarding,
- no automatic USB passthrough,
- ephemeral challenge-specific VM clone,
- bounded CPU/RAM/disk,
- challenge data transferred through an explicit copy channel.

The product is a risk-reduction tool, not a formal malware sandbox. The UI and documentation must not promise containment stronger than the underlying hypervisor and host OS provide.

---

## 20. Resource Defaults

Reasonable default profile:

```text
CPU:     4 vCPU
Memory:  8 GiB
Disk:    60 GiB
Network: isolated
```

The user may override these per profile or session.

Before launch, check:

- available disk space,
- requested RAM,
- profile image presence,
- backend health.

A launch should fail early with an actionable message rather than halfway through image creation.

---

## 21. Storage Layout

Suggested host layout:

```text
~/Library/Application Support/wargame-vm/
├─ config/
│  └─ config.toml
├─ profiles/
├─ sessions/
│  └─ <session-id>/
│     └─ metadata.json
├─ cache/
└─ logs/
```

Secrets must not live here in plaintext; use macOS Keychain.

Temporary challenge downloads should use a private temporary directory with restrictive permissions and should be removed after transfer.

Large VM disks may remain managed by Tart rather than copied into the application directory.

---

## 22. Configuration

Example:

```toml
[general]
default_provider = "dreamhack"
default_profile = "macos-re-arm64"
default_network = "isolated"

[vm.local_macos]
provider = "tart"

[vm.local_macos.resources]
cpu = 4
memory_gib = 8

[wargame.dreamhack]
credential_store = "macos-keychain"
```

Future:

```toml
[vm.windows_lab]
provider = "hyperv-remote"
endpoint = "https://windows-lab.example.internal"
guest_arch = "x86_64"
```

---

## 23. Error Handling

Errors should be expressed in user terms.

Good:

```text
Cannot launch macos-re-arm64.

The profile image requires 60 GiB, but only 24 GiB is currently free.

Try:
  wargame sessions
  wargame delete <old-session>
```

Bad:

```text
process exited with status 1
```

Error categories:

- authentication,
- provider unavailable,
- challenge not found,
- download failed,
- hash/write failure,
- incompatible architecture,
- image missing,
- insufficient resources,
- VM creation failed,
- guest boot timeout,
- guest bootstrap failed,
- copy failed,
- network unavailable,
- cleanup failed.

Raw backend errors may be retained in debug logs.

---

## 24. Observability and Logs

Logs must be useful without leaking secrets.

Record:

- application version,
- provider,
- challenge ID,
- VM backend,
- image/profile,
- lifecycle transitions,
- backend command exit status,
- timing,
- sanitized paths,
- file SHA-256.

Never log:

- passwords,
- session cookies,
- authorization headers,
- private keys,
- full Keychain values.

Support:

```bash
wargame doctor
wargame session inspect <id>
wargame --verbose ...
```

---

## 25. Compatibility Rules

Before VM creation, validate:

```text
Challenge requirement
        +
Profile guest OS/arch
        +
VM provider capabilities
        =
Launchable?
```

Examples:

```text
macOS arm64 profile + local Tart on Apple Silicon
→ supported

Windows x86_64 profile + local macOS Tart backend
→ unsupported

Windows x86_64 profile + future remote x86_64 Hyper-V backend
→ supported
```

The application should explain the mismatch:

```text
This environment requires Windows/x86_64.

The current backend "local-macos" supports:
  - macOS/arm64

Configured compatible backends:
  none

A future remote Hyper-V backend can satisfy this requirement.
```

---

## 26. Future: Remote Hyper-V Backend

This is **not part of the first MVP**, but the MVP architecture should make it a normal `VmProvider`.

### Motivation

Apple Silicon is not an ideal place to hardware-virtualize Windows x86-64 guests.

A separate x86-64 Windows machine can provide Hyper-V and execute the guest architecture natively.

### Recommended topology

```text
┌──────────────────────────────┐
│ macOS client                 │
│                              │
│ TUI / CLI                    │
│ Core orchestrator            │
└──────────────┬───────────────┘
               │ authenticated control API
               │
               ▼
┌──────────────────────────────┐
│ Remote Windows x86-64 host   │
│                              │
│ wargame-vm agent             │
│ Hyper-V PowerShell           │
│ Image/template store         │
└──────────────┬───────────────┘
               │ Hyper-V
               ▼
┌──────────────────────────────┐
│ Disposable Windows VM        │
│                              │
│ challenge files              │
│ Ghidra / debugger / Python   │
└──────────────────────────────┘
```

### Why use a Windows-side agent

The macOS client should not need to understand every detail of:

- Hyper-V PowerShell,
- Windows credential delegation,
- VM image paths,
- PowerShell Direct,
- checkpoints,
- virtual switches,
- guest provisioning.

The agent provides a narrow remote API implementing the same logical `VmProvider` operations.

### Candidate agent operations

```text
GET    /v1/capabilities
GET    /v1/images
POST   /v1/vms
GET    /v1/vms/:id
POST   /v1/vms/:id/start
POST   /v1/vms/:id/stop
DELETE /v1/vms/:id
POST   /v1/vms/:id/files
POST   /v1/vms/:id/exec
POST   /v1/vms/:id/checkpoints
```

The exact transport is intentionally deferred.

### Guest file/bootstrap channel

For Windows guests, a Windows-side agent can use Hyper-V/PowerShell facilities to execute bootstrap commands and copy files without requiring the guest network to be configured first.

This is preferable to exposing broad administrative remoting directly from the Mac client.

### Graphical access

The remote backend should return a `ConsoleDescriptor`, for example:

```text
ConsoleDescriptor
├─ type: rdp | vnc | web
├─ endpoint
├─ expires_at?
└─ credential_ref?
```

Likely approaches:

1. RDP directly to the guest through a private network/VPN.
2. VNC installed in the guest.
3. A web gateway such as an RDP/VNC-to-browser proxy.
4. A Windows-hosted console bridge.

The core UI should only consume the descriptor; it should not assume VNC specifically.

---

## 27. Future: Additional VM Backends

The same abstraction should later support:

### Local Linux on Apple Silicon

Potentially using the same local backend where supported.

Use cases:

- pwnable,
- ELF binaries,
- Linux tooling,
- ARM64 Linux environments.

### x86-64 Linux

Possible options:

- remote x86-64 host,
- cloud runner,
- local emulation where performance is acceptable.

The architecture should represent “virtualized” and “emulated” execution differently if that distinction affects compatibility or expected performance.

### Local Windows ARM64

Potentially useful for Windows tooling, but it must not be treated as equivalent to Windows x86-64.

---

## 28. Future: Image Registry

A later version may let profiles reference versioned OCI-distributed images:

```text
wargame-vm/macos-re-arm64:2026.09
wargame-vm/windows-re-x64:2026.09
wargame-vm/linux-pwn-x64:2026.09
```

Desired properties:

- immutable digest pinning,
- reproducible build source,
- optional signing,
- local caching,
- controlled updates.

For a single-user MVP, local images plus upstream Tart images are enough.

---

## 29. Security Threat Model

### Assets to protect

- host filesystem,
- host account,
- host Keychain,
- SSH keys,
- browser sessions,
- Dreamhack account,
- remote Windows admin credentials,
- LAN devices,
- preserved VM sessions.

### Adversarial inputs

- downloaded binaries,
- archives,
- filenames,
- challenge-provided scripts,
- malformed documents,
- malicious symbolic links inside archives,
- compromised remote challenge services,
- unexpected provider HTML/API responses.

### Primary threats

#### T1. Host-side accidental execution

Mitigation:

- opaque-byte download path,
- no host extraction,
- no preview,
- no auto-open,
- explicit VM transfer.

#### T2. Credential theft from guest

Mitigation:

- do not forward host credentials,
- use Keychain on host,
- separate provider authentication from guest bootstrap.

#### T3. Guest writes into host filesystem

Mitigation:

- no shared host folders by default,
- use copy-in rather than mounted workspace.

#### T4. Guest attacks LAN/host over network

MVP mitigation:

- isolated networking as default,
- explicit NAT opt-in.

Future mitigation:

- stronger network policy and egress filtering.

#### T5. Persistent compromised VM

Mitigation:

- ephemeral per-challenge clone,
- easy deletion,
- no mutation of canonical profile image.

#### T6. Malicious archive path traversal

Mitigation:

- no host extraction,
- guest bootstrap extracts into a dedicated directory,
- guest extraction code still sanitizes paths.

#### T7. External provider parser breakage

Mitigation:

- Dreamhack-specific adapter,
- schema normalization,
- parser fixture tests,
- graceful “provider changed” error.

---

## 30. Functional Requirements

### FR-01 — Search

The user can search Dreamhack wargames from the TUI/CLI.

Acceptance:

- returns title and stable provider identifier,
- supports pagination or incremental loading,
- a result can be opened without using a browser.

### FR-02 — Problem detail

The user can inspect:

- title,
- description,
- category/tags,
- available files,
- relevant connection metadata when available.

### FR-03 — Authentication

The application can perform authenticated operations required to download a user-accessible Dreamhack asset.

Acceptance:

- authentication data is not stored in plaintext config,
- secret values do not appear in logs.

### FR-04 — Profile selection

The user can select a compatible macOS/arm64 profile.

### FR-05 — Image preparation

The application can ensure that a requested prepared image exists locally.

### FR-06 — Disposable VM

A fresh challenge VM can be created without modifying the canonical profile image.

### FR-07 — Safe download

Problem assets are downloaded to private temporary storage as raw bytes.

### FR-08 — Copy-in

Downloaded assets are copied into the guest.

### FR-09 — Guest bootstrap

The workspace is automatically initialized inside the guest.

### FR-10 — Terminal

The user can enter the running VM through SSH from one product action.

### FR-11 — GUI

The user can open the VM graphical console.

### FR-12 — Lifecycle

The user can:

- list,
- stop,
- resume,
- delete,
- preserve

sessions.

### FR-13 — Network selection

The user can choose between isolated and NAT networking before launch.

### FR-14 — Architecture validation

An incompatible guest OS/architecture/backend combination is rejected before VM creation.

### FR-15 — Health check

`wargame doctor` identifies missing prerequisites and common configuration problems.

---

## 31. Non-Functional Requirements

### NFR-01 — Local-first

The orchestration service does not require a hosted control plane for MVP.

### NFR-02 — Recoverable

A crash during launch must leave enough metadata to identify and delete an orphan VM.

### NFR-03 — Deterministic profiles

A profile definition should produce a predictable environment.

### NFR-04 — Secret hygiene

No application logs or session metadata contain plaintext credentials.

### NFR-05 — Backend isolation

Dreamhack code cannot directly call Tart, and Tart-specific code cannot directly parse Dreamhack responses.

### NFR-06 — Fast repeated launch

Once a profile image exists locally, creating another challenge environment should primarily be a clone/start/copy/bootstrap operation rather than a full OS installation.

### NFR-07 — Explicit destructive actions

Deleting a preserved session requires a deliberate action.

### NFR-08 — Testable core

Search normalization, compatibility decisions, lifecycle state transitions, config parsing, and command construction should be testable without booting a real VM.

---

## 32. Suggested Internal Architecture

```text
┌─────────────────────────────────────────────────────┐
│ Presentation                                        │
│ CLI / TUI                                           │
└─────────────────────┬───────────────────────────────┘
                      │
┌─────────────────────▼───────────────────────────────┐
│ Application / Orchestrator                          │
│                                                     │
│ SearchChallenge                                     │
│ LaunchChallenge                                     │
│ StopSession                                         │
│ DeleteSession                                       │
│ OpenConsole                                         │
└──────────────┬───────────────────────────┬──────────┘
               │                           │
      ┌────────▼─────────┐       ┌────────▼──────────┐
      │ Wargame Provider │       │ VM Provider       │
      │                  │       │                   │
      │ Dreamhack        │       │ Local Tart        │
      └────────┬─────────┘       └────────┬──────────┘
               │                           │
      ┌────────▼─────────┐       ┌────────▼──────────┐
      │ Credential Store │       │ Image/Profile     │
      │ macOS Keychain   │       │ Manager           │
      └──────────────────┘       └───────────────────┘

                      ┌───────────────────────────────┐
                      │ Session Repository            │
                      │ Local metadata                │
                      └───────────────────────────────┘
```

Future additions:

```text
WargameProvider
├─ Dreamhack
├─ pwnable.kr
└─ ...

VmProvider
├─ TartLocal
├─ HyperVRemote
├─ LinuxRemote
└─ ...
```

---

## 33. Recommended Module Boundaries

Language-neutral logical modules:

```text
core/
  challenge
  session
  profile
  compatibility

application/
  search
  launch
  lifecycle
  doctor

providers/
  wargame/
    dreamhack
  vm/
    tart
    hyperv_remote   # future

security/
  credentials
  staging
  filename_safety

infra/
  process
  filesystem
  http
  ssh
  keychain

ui/
  cli
  tui
```

The design should avoid a single “manager” object that owns every responsibility.

---

## 34. Implementation Direction

A native macOS-first CLI/TUI can be implemented in Rust, but the PRD does not require Rust-specific architecture.

If Rust is selected, likely categories of dependencies include:

- async runtime,
- HTTP client,
- serialization,
- TOML/YAML,
- TUI,
- process execution,
- SSH or system SSH integration,
- macOS Keychain integration.

Prefer calling a stable external CLI for Tart in MVP over creating a deep native binding prematurely.

All external-process invocation should use structured argument arrays rather than shell-string concatenation.

---

## 35. Testing Strategy

### Unit tests

Test without VMs:

- filename sanitization,
- challenge normalization,
- profile parsing,
- capability matching,
- session state transitions,
- secret redaction,
- backend command construction,
- temporary-file cleanup decisions.

### Provider fixture tests

Store sanitized Dreamhack response fixtures and test:

- search parsing,
- detail parsing,
- asset parsing,
- authentication failure detection.

The fixture layer makes external markup/API changes easier to diagnose.

### VM backend integration tests

On a supported Mac:

1. backend healthcheck,
2. clone known test image,
3. boot,
4. obtain guest connectivity,
5. copy a harmless fixture,
6. run guest bootstrap,
7. verify file,
8. destroy VM.

### End-to-end smoke test

Use a harmless internal fixture challenge rather than a real exploit binary:

```text
search fixture
→ launch VM
→ copy ZIP
→ guest extracts ZIP
→ SSH command verifies expected file
→ delete session
```

---

## 36. MVP Milestones

### M0 — VM proof

Goal: prove safe disposable local macOS sessions.

Deliver:

- Tart backend healthcheck,
- one profile,
- clone/start/stop/delete,
- SSH,
- copy-in,
- bootstrap,
- no Dreamhack integration.

Exit condition:

> A local harmless file can be injected into a fresh macOS VM and the VM can be discarded from one CLI workflow.

### M1 — Profile system

Deliver:

- declarative profile,
- `macos-minimal-arm64`,
- `macos-re-arm64`,
- automated image build/provision,
- Python/uv/Ghidra availability checks.

Exit condition:

> A newly cloned reversing profile starts with all declared tools available.

### M2 — Dreamhack adapter

Deliver:

- authentication,
- search,
- detail,
- asset download,
- SHA-256,
- safe staging.

Exit condition:

> A selected Dreamhack problem file can be downloaded without opening/extracting it on the host.

### M3 — Unified launch

Deliver:

```text
Dreamhack search
→ select problem
→ select profile
→ create VM
→ download
→ copy
→ bootstrap
→ SSH/GUI
```

Exit condition:

> The normal happy path no longer requires the user to manually create a VM, install tools, or move challenge files.

### M4 — TUI polish and lifecycle

Deliver:

- search UI,
- problem view,
- launch progress,
- session list,
- keep/delete,
- errors,
- `doctor`.

This completes the first product MVP.

---

## 37. MVP Acceptance Scenario

Given:

- an Apple Silicon Mac,
- a prepared `macos-re-arm64` profile,
- valid Dreamhack authentication,
- a Dreamhack challenge with a downloadable file,

when the user:

1. runs `wargame`,
2. searches for the challenge,
3. opens it,
4. selects `macos-re-arm64`,
5. keeps networking isolated,
6. launches,

then the application must:

1. create a new disposable VM,
2. download the challenge file without host-side execution/extraction,
3. compute and record SHA-256,
4. copy the artifact into the guest,
5. initialize the guest workspace,
6. report the VM as ready,
7. allow SSH and GUI access,
8. leave Dreamhack credentials on the host,
9. allow the user to delete the entire challenge session afterward.

If all nine conditions work reliably, the MVP has achieved its primary goal.

---

## 38. Success Metrics

Because the first user is the developer, the initial metrics should be operational rather than growth-oriented.

Track:

### Setup friction

- number of manual steps required before first launch,
- number of tools manually installed after profile build.

Target:

- zero manual tool installation per challenge.

### Launch friction

Measure:

```text
challenge selected
→ usable shell inside prepared VM
```

The main goal is consistency and few interactions, not an arbitrary benchmark before real measurements exist.

### Safety defaults

Target:

- 100% of challenge assets copied without host-side auto-extraction,
- 0 credentials written to plaintext logs/config,
- 0 host-folder mounts created implicitly.

### Cleanup

Target:

- one action to delete a challenge session,
- detectable/cleanable orphan sessions after interrupted launches.

---

## 39. Open Questions

These do not block the architecture, but should be resolved during implementation.

### Q1. Dreamhack authentication mechanism

Which current Dreamhack authentication flow is the least brittle while keeping credentials out of the guest?

The adapter should permit this answer to change.

### Q2. GUI ownership

Should `wargame gui` simply delegate to the Tart graphical runner in MVP, or should the application eventually embed a VM view?

Recommendation: delegate in MVP.

### Q3. Guest transfer transport

Use:

- SSH/SFTP,
- backend-specific file copy,
- or a tiny guest agent?

Recommendation: use SSH/system tooling for the macOS MVP unless it becomes unreliable. Keep `copy_in` backend-neutral.

### Q4. Archive extraction

Should the guest automatically unpack common challenge archives?

Recommendation: support an explicit profile/session option, defaulting to unpack inside the guest but **never execute** extracted content.

### Q5. Challenge architecture metadata

Dreamhack challenge metadata may not always identify the required OS/CPU architecture.

Recommendation: let the user explicitly choose a profile in MVP and add heuristics only later.

---

## 40. Decisions

For the first implementation:

| Area | Decision |
|---|---|
| Host | macOS on Apple Silicon |
| UI | CLI + TUI |
| Wargame provider | Dreamhack |
| Local VM backend | Tart |
| Local guest | macOS arm64 |
| Image provisioning | Declarative profiles + provisioning scripts |
| Initial profiles | minimal, reversing |
| Challenge transfer | explicit copy-in |
| Host-side extraction | forbidden |
| Default network | isolated |
| Credentials | macOS Keychain |
| Session model | disposable clone per challenge |
| Remote Hyper-V | architecture only, post-MVP |
| Windows x86-64 | remote x86-64 backend later |
| GUI | backend-provided VM console |
| Multi-user/cloud | out of scope |

---

## 41. Future Roadmap

After MVP:

### v0.2 — Linux

- local Linux arm64 profile,
- pwnable-oriented image,
- architecture-aware environment recommendations.

### v0.3 — Remote Windows x86-64

- Windows Hyper-V agent,
- remote target registration,
- Windows image/profile definitions,
- PowerShell Direct bootstrap,
- RDP/VNC/web console descriptor,
- secure transport and host authentication.

### v0.4 — Network hardening

- guest-to-LAN restrictions,
- host protection policy,
- challenge endpoint allow rules,
- per-profile network policies.

### v0.5 — More wargame providers

- additional adapters using the same `WargameProvider` contract.

### Later

- snapshot/checkpoint UX,
- notes/export workflow,
- profile/image signing,
- reproducible remote image registry,
- desktop GUI,
- remote runner scheduling.

---

## 42. Technical Feasibility Notes

### Apple Silicon macOS virtualization

Apple's Virtualization framework supports creating and running macOS guests on Apple Silicon. It also provides VM lifecycle and graphical interaction APIs. This makes a native backend technically possible.

For MVP, Tart is a higher-level implementation choice that already provides a practical Apple-Silicon macOS VM workflow, cloning, reusable images, OCI distribution, and tooling around image creation.

### Local cross-architecture limitation

Apple's virtualization documentation expects guest kernels/OS components that support the host CPU architecture. Therefore an Apple Silicon local backend should not be modeled as a generic x86-64 Windows virtualization backend.

Cross-architecture emulation can exist as a distinct future backend, but should not be the default plan for Windows x86-64.

### Remote Hyper-V feasibility

Microsoft exposes Hyper-V management through PowerShell and remote-management facilities. Hyper-V PowerShell cmdlets can target remote hosts, and PowerShell Direct can execute commands and copy files between a Hyper-V host and supported Windows guests without first requiring guest networking.

This supports a future design where a Windows-side agent owns Hyper-V operations while the Mac acts only as the orchestrator/UI.

---

## 43. References

These references informed the technical direction of this PRD.

- Apple Virtualization framework  
  https://developer.apple.com/documentation/virtualization

- Apple — Virtualize macOS on a Mac  
  https://developer.apple.com/documentation/virtualization/virtualize-macos-on-a-mac

- Apple — Running macOS in a virtual machine on Apple silicon  
  https://developer.apple.com/documentation/virtualization/running-macos-in-a-virtual-machine-on-apple-silicon

- Tart — Quick Start  
  https://github.com/openai/tart/blob/main/docs/quick-start.md

- Packer plugin for Tart  
  https://github.com/cirruslabs/packer-plugin-tart/blob/main/docs/builders/tart.mdx

- Dreamhack Wargame  
  https://dreamhack.io/wargame/

- Microsoft — Remotely manage Hyper-V hosts  
  https://learn.microsoft.com/windows-server/virtualization/hyper-v/manage/remotely-manage-hyper-v-hosts

- Microsoft — PowerShell Direct  
  https://learn.microsoft.com/windows-server/virtualization/hyper-v/powershell-direct

---

## 44. One-Sentence MVP Definition

> **On an Apple Silicon Mac, search a Dreamhack challenge, choose a prepared macOS analysis profile, launch a disposable VM, safely inject the challenge files, and enter a ready-to-use solving environment without exposing host credentials or manually configuring the VM.**
