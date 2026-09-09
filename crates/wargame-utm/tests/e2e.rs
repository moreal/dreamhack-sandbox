//! Real end-to-end proof of the UTM adapter against an actual UTM
//! installation and an actual, already-built profile image (spec.md §7
//! AC-05/AC-08/AC-11, §9's "실VM 검사는 opt-in이다").
//!
//! This is the `wargame-utm` counterpart of `crates/wargame-tart/tests/e2e.rs`
//! and it drives the *provider* directly -- no `AppContext`, no CLI, no
//! sessions, no guest login. What it proves is the part of the lifecycle that
//! belongs to the backend: the recorded VMs are visible and stopped, a session
//! VM can be cloned from the published image, started, given an address by
//! the guest agent, stopped (through the T12 orphan check) and deleted, the
//! source VMs come out of that untouched, and a UUID this app never created
//! is `Missing` rather than something to delete.
//!
//! **Opt-in twice over.** `#[ignore]` keeps it out of `cargo test
//! --workspace`, and even under `--ignored` every test here returns
//! immediately unless *both*:
//!
//!   - `WARGAME_E2E=1`, the same switch `wargame-tart`'s e2e test uses, so a
//!     blanket `cargo test -- --ignored` can never boot a VM; and
//!   - `WARGAME_E2E_UTM_PROFILE=<profile>`, naming a profile in the caller's
//!     `$WARGAME_HOME` whose build record already carries a published image.
//!
//! Both being unset is also why this file compiles and passes on Linux and on
//! a Mac with no UTM: the gate is an environment check that returns, not a
//! `cfg(target_os)`. If the switches *are* set on a host with no `utmctl`,
//! each test says so and returns rather than failing -- there is nothing to
//! test, and nothing was broken.
//!
//! ```sh
//! WARGAME_E2E=1 \
//! WARGAME_E2E_UTM_PROFILE=t7-kali-arm64 \
//! WARGAME_HOME="$HOME/Library/Application Support/wargame-vm-t7" \
//!   cargo test -p wargame-utm --test e2e -- --ignored --nocapture --test-threads=1
//! ```
//!
//! **`--test-threads=1` is documented, and nothing here depends on it.** UTM
//! automation is a single AppleScript connection: two `osascript` calls at
//! once make it answer `UTM got an error: Connection is invalid. (-609)`, and
//! under cargo's default four-thread harness three of these four tests failed
//! on a healthy host (task-7-rerun-report.md N1). So every test takes
//! [`UTM_LOCK`] before touching UTM and the file runs one test at a time
//! whatever `--test-threads` says; the flag stays in the documented command
//! because it also keeps `--nocapture` output readable and is what the task
//! reports quote. The assertions that used to compare a *global* VM count
//! (which any concurrent clone breaks, whoever holds the lock) now compare
//! VMs by id instead.
//!
//! Optional knobs: `WARGAME_E2E_UTM_NETWORK=isolated|nat` (default `nat`,
//! which is what `scripts/e2e-smoke.sh` uses and what the guest agent is
//! known to answer on), `WARGAME_E2E_UTM_BOOT_SECS` to override the boot
//! timeout, and `WARGAME_E2E_UTM_CPU` / `WARGAME_E2E_UTM_MEMORY_MIB` for the
//! clone's resources.
//!
//! **What it creates, and where.** Exactly one VM, named
//! `<build_prefix>e2e-<pid>` (i.e. `wg-build-e2e-12345`) -- inside the app's
//! build prefix, so the adapter's ownership check accepts it and `prune`
//! (which matches sessions, not names) never mistakes it for an orphan. It is
//! stopped and deleted at the end whatever happens, and nothing outside that
//! one clone is ever written to: the installation VM and the image VM are
//! only ever listed and status-checked. (The blast-radius test makes three
//! more, `<build_prefix>e2e-del-<pid>-1..3`, and deletes them too -- every
//! throwaway this file creates is named `<build_prefix>e2e-`, which is how
//! the comparisons tell another run's leftovers from the user's own VMs.)

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use wargame_core::domain::{
    Backend, BuildRecord, NetworkMode, PublishedImage, ResolvedBackend, Resources, VmRef,
};
use wargame_core::infra::process::RealRunner;
use wargame_core::infra::{AppDirs, JsonBuildRepository};
use wargame_core::ports::{
    BuildRecordRepository, ImageRef, StartOptions, VmProvider, VmSpec, VmStatus,
};

use wargame_utm::{RecordImageCatalog, UtmConfig, UtmProvider};

/// A UUID this app has certainly never created, for the ownership refusal.
const FOREIGN_UUID: &str = "00000000-DEAD-4BEE-8000-000000000001";

/// **One test at a time, in this process.** UTM's AppleScript bridge does not
/// survive concurrent `osascript` calls (`-609`), so the whole file is
/// serialised here rather than relying on the caller remembering
/// `--test-threads=1` (task-7-rerun-report.md N1). `tokio::sync::Mutex` and
/// not `std::sync::Mutex` because the guard is held across `await`s; it also
/// does not poison, so a panicking test releases the lock and the rest of the
/// suite still runs.
static UTM_LOCK: Mutex<()> = Mutex::const_new(());

/// Everything a test needs, or `None` when this run is not opted in / has no
/// UTM. `None` is always accompanied by a printed reason.
struct Fixture {
    provider: UtmProvider,
    cfg: UtmConfig,
    record: BuildRecord,
    image: PublishedImage,
}

fn env_flag(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env_num(name: &str) -> Option<u64> {
    env_flag(name).and_then(|v| v.parse().ok())
}

/// The two switches, then UTM itself, then the build record. Anything missing
/// is a skip with a reason on stderr -- never a failure, because none of them
/// mean the code under test is wrong.
async fn fixture(test: &str) -> Option<Fixture> {
    if std::env::var("WARGAME_E2E").as_deref() != Ok("1") {
        eprintln!("skipping {test}: set WARGAME_E2E=1 to run this against a real UTM VM");
        return None;
    }
    let Some(profile) = env_flag("WARGAME_E2E_UTM_PROFILE") else {
        eprintln!(
            "skipping {test}: set WARGAME_E2E_UTM_PROFILE=<profile> to name a profile in \
             $WARGAME_HOME whose image is already built"
        );
        return None;
    };

    let cfg = UtmConfig::default();
    let dirs = AppDirs::resolve().expect("could not resolve $WARGAME_HOME");
    let builds: Arc<dyn BuildRecordRepository> =
        Arc::new(JsonBuildRepository::new(dirs.builds_dir()));

    // The catalog is not decoration. Since the D7 fix `UtmProvider::image_exists`
    // asks the build records *first* and only then UTM, so a provider left on
    // the default `NoBuildRecords` answers "not ready" for every image however
    // healthy UTM is -- which is why the read-only test could not pass
    // (task-7-rerun-report.md N2). `wargame-cli`'s `wiring.rs` gives the real
    // binary a `RecordImageCatalog` over the same `$WARGAME_HOME` build
    // records; a fixture without one would be testing a provider the CLI
    // never builds.
    let provider = UtmProvider::new(Arc::new(RealRunner), cfg.clone())
        .with_image_catalog(Arc::new(RecordImageCatalog::new(builds.clone())));
    if let Err(e) = provider.utmctl_path() {
        eprintln!("skipping {test}: no UTM on this host ({e})");
        return None;
    }

    let record = builds
        .load(&profile)
        .await
        .expect("could not read the build record")
        .unwrap_or_else(|| {
            panic!(
                "WARGAME_E2E_UTM_PROFILE={profile} has no build record under {}. \
                 Run `wargame profile create {profile} --iso <path>` and `wargame profile build \
                 {profile}` first, or point $WARGAME_HOME at the home that has one.",
                dirs.builds_dir().display()
            )
        });
    assert_eq!(
        record.backend, "utm",
        "profile {profile}'s build record is on backend {:?}, not utm",
        record.backend
    );
    let image = record.ready_image().cloned().unwrap_or_else(|| {
        panic!(
            "profile {profile}'s build record has published no image (phase: {}). \
             This test verifies what happens *after* `wargame profile build {profile}`.",
            record.phase
        )
    });

    Some(Fixture {
        provider,
        cfg,
        record,
        image,
    })
}

fn image_vm(image: &PublishedImage) -> VmRef {
    VmRef {
        backend: "utm".to_string(),
        id: image.id.clone(),
        name: image.name.clone(),
    }
}

fn network() -> NetworkMode {
    match env_flag("WARGAME_E2E_UTM_NETWORK").as_deref() {
        Some("isolated") => NetworkMode::Isolated,
        _ => NetworkMode::Nat,
    }
}

/// The clone's execution configuration, taken from the record so an emulated
/// x86_64 profile is cloned as one (hypervisor off) and an arm64 one is not.
fn execution(record: &BuildRecord) -> ResolvedBackend {
    ResolvedBackend {
        backend: Backend::Utm,
        mode: record.mode,
        os: record.os,
        arch: record.arch,
        distribution: None,
    }
}

fn resources() -> Resources {
    Resources {
        cpu: env_num("WARGAME_E2E_UTM_CPU").unwrap_or(2) as u32,
        memory_mib: env_num("WARGAME_E2E_UTM_MEMORY_MIB").unwrap_or(4096),
        // UTM cannot resize a cloned disk from a script (T5); the clone keeps
        // whatever the image was built with, and the adapter says so.
        disk_gib: 0,
    }
}

/// Start -> address -> status -> stop -> status, with no guest login at all:
/// every observation here is the provider's own (spec.md §7 AC-08).
///
/// Returns `Err` instead of panicking so the caller can always delete the VM
/// afterwards, whatever went wrong (NFR-02: nothing lingers).
async fn run_lifecycle(
    provider: &UtmProvider,
    vm: &VmRef,
    boot_timeout: Duration,
    network: NetworkMode,
) -> Result<(), String> {
    let opts = StartOptions {
        network,
        // UTM has no headless start at all (T7) and the adapter warns when
        // asked for one; asking honestly for a window keeps that warning out
        // of the log, since a window is what appears either way.
        headless: false,
        log_path: std::env::temp_dir().join(format!("{}-run.log", vm.name)),
    };

    let t0 = Instant::now();
    provider
        .start(vm, &opts)
        .await
        .map_err(|e| format!("start failed: {e}"))?;
    eprintln!("started {} in {:?}", vm.name, t0.elapsed());

    let t0 = Instant::now();
    let ip = provider
        .wait_ip(vm, boot_timeout)
        .await
        .map_err(|e| format!("wait_ip failed: {e}"))?;
    eprintln!("guest agent answered {ip} after {:?}", t0.elapsed());

    match provider.status(vm).await {
        Ok(VmStatus::Running) => {}
        Ok(other) => return Err(format!("a booted VM reports status {other:?}")),
        Err(e) => return Err(format!("status failed: {e}")),
    }

    // The stop is the interesting half: it resumes a paused VM first, polls
    // for `stopped` rather than trusting the verb (T9), escalates to `by
    // kill` only after `stop_timeout`, and then refuses if a `qemu-*-softmmu`
    // still carries this VM's `-uuid` (T12). All of that is inside
    // `VmProvider::stop`, so an `Ok` here is the orphan check passing too.
    let t0 = Instant::now();
    provider
        .stop(vm)
        .await
        .map_err(|e| format!("stop failed (this includes the T12 orphan check): {e}"))?;
    eprintln!("stopped {} in {:?}", vm.name, t0.elapsed());

    match provider.status(vm).await {
        Ok(VmStatus::Stopped) => Ok(()),
        Ok(other) => Err(format!("a stopped VM reports status {other:?}")),
        Err(e) => Err(format!("status after stop failed: {e}")),
    }
}

/// The whole backend lifecycle against the profile's real published image:
/// the recorded VMs are listed and stopped, a clone boots and is torn down,
/// and the sources survive it.
#[tokio::test]
#[ignore]
async fn utm_clone_boot_stop_delete_preserves_the_source() {
    let _utm = UTM_LOCK.lock().await;
    let Some(Fixture {
        provider,
        cfg,
        record,
        image,
    }) = fixture("utm_clone_boot_stop_delete_preserves_the_source").await
    else {
        return;
    };

    // --- 1. the recorded VMs, as the backend sees them -----------------
    let listed = provider.list().await.expect("utmctl list");
    let names: Vec<&str> = listed.iter().map(|vm| vm.name.as_str()).collect();
    eprintln!("this app's UTM VMs: {names:?}");

    let source = image_vm(&image);
    assert!(
        listed.iter().any(|vm| vm.id == source.id),
        "the published image {} ({}) is not among this app's VMs: {names:?}",
        source.name,
        source.id
    );
    assert_eq!(
        provider.status(&source).await.expect("status of the image"),
        VmStatus::Stopped,
        "the image VM {} must be stopped to be cloned",
        source.name
    );

    let installer = record.installer_vm.clone();
    if let Some(installer) = &installer {
        let status = provider
            .status(installer)
            .await
            .expect("status of the installation VM");
        eprintln!("installation VM {} is {status:?}", installer.name);
    } else {
        eprintln!("note: this build record names no installation VM (a --from-vm clone)");
    }

    // --- 2. clone it ---------------------------------------------------
    let name = format!("{}e2e-{}", cfg.build_prefix, std::process::id());
    let spec = VmSpec {
        name: name.clone(),
        image: ImageRef(image.id.clone()),
        resources: resources(),
        execution: execution(&record),
        network: network(),
    };
    let vm = provider
        .create(&spec)
        .await
        .unwrap_or_else(|e| panic!("cloning {} into {name} failed: {e}", image.name));
    eprintln!("cloned {} -> {} ({})", image.name, vm.name, vm.id);
    assert_ne!(vm.id, image.id, "a clone must be a different VM");

    // --- 3. the lifecycle, then cleanup no matter what -----------------
    let boot_timeout = env_num("WARGAME_E2E_UTM_BOOT_SECS")
        .map(Duration::from_secs)
        .unwrap_or_else(|| provider.boot_timeout(record.mode));
    let outcome = run_lifecycle(&provider, &vm, boot_timeout, network()).await;

    let deleted = provider.delete(&vm).await;
    if let Err(e) = &deleted {
        eprintln!("warning: could not delete the e2e VM {}: {e}", vm.name);
    }
    outcome.expect("UTM lifecycle failed");
    deleted.expect("delete failed");

    // --- 4. what survived ----------------------------------------------
    let after = provider.list().await.expect("utmctl list after the run");
    assert!(
        !after.iter().any(|listed| listed.id == vm.id),
        "the clone {} is still listed after delete",
        vm.id
    );
    assert!(
        after.iter().any(|listed| listed.id == source.id),
        "the published image {} is gone after the run",
        source.id
    );
    assert_eq!(
        provider.status(&source).await.expect("status of the image"),
        VmStatus::Stopped,
        "the image VM must still be stopped after the run"
    );
    if let Some(installer) = &installer {
        assert!(
            after.iter().any(|listed| listed.id == installer.id),
            "the installation VM {} is gone after the run",
            installer.id
        );
    }
}

/// UTM itself, without needing a built image: the two switches and
/// `utmctl`, and nothing else. For the tests that make their own throwaway
/// VMs.
fn utm_only(test: &str) -> Option<(UtmProvider, UtmConfig)> {
    if std::env::var("WARGAME_E2E").as_deref() != Ok("1") {
        eprintln!("skipping {test}: set WARGAME_E2E=1 to run this against a real UTM");
        return None;
    }
    let cfg = UtmConfig::default();
    let provider = UtmProvider::new(Arc::new(RealRunner), cfg.clone());
    if let Err(e) = provider.utmctl_path() {
        eprintln!("skipping {test}: no UTM on this host ({e})");
        return None;
    }
    Some((provider, cfg))
}

/// **The blast-radius test (D1).** `delete` used to be applied to a `repeat`
/// loop's reference, which UTM followed back to the whole collection: one
/// `wargame profile delete` on a throwaway profile deleted **every** virtual
/// machine on the host, on disk, and reported success
/// (task-7-b2b-report.md D1).
///
/// No fake can see that -- the fakes model `delete` as removing one map
/// entry -- so this is the test that has to run against real UTM. It makes
/// three empty throwaway VMs, deletes the **middle** one through the
/// project's own `VmProvider::delete`, and requires the registry afterwards
/// to differ by exactly that one VM. Every other VM UTM knows, including the
/// user's, is compared by UUID.
///
/// The three are tiny (a blank 1 GiB sparse disk, no ISO, never started) and
/// are deleted again whatever happens.
#[tokio::test]
#[ignore]
async fn utm_delete_removes_exactly_one_vm() {
    let _utm = UTM_LOCK.lock().await;
    let Some((provider, cfg)) = utm_only("utm_delete_removes_exactly_one_vm") else {
        return;
    };
    let automation = provider.automation();

    // Everything UTM holds before this test, by id, minus any throwaway
    // belonging to another run of this suite: those are the only VMs entitled
    // to come and go underneath us, and dropping them keeps the blast-radius
    // comparison exact without depending on a global count (N1). The VMs this
    // test makes are compared separately, so they are not lost by the filter.
    let e2e_prefix = format!("{}e2e-", cfg.build_prefix);
    let before: Vec<String> = automation
        .list()
        .await
        .expect("utmctl list")
        .into_iter()
        .filter(|vm| !vm.name.starts_with(&e2e_prefix))
        .map(|vm| vm.id)
        .collect();

    let mut made: Vec<VmRef> = Vec::new();
    for n in 1..=3 {
        let name = format!("{}e2e-del-{}-{n}", cfg.build_prefix, std::process::id());
        match automation
            .make_vm(&wargame_utm::MakeVmParams {
                name: &name,
                architecture: "aarch64",
                memory_mib: 512,
                cpu_cores: 1,
                hypervisor: true,
                uefi: true,
                disk_mib: 1024,
                iso: None,
            })
            .await
        {
            Ok((id, name)) => made.push(VmRef {
                backend: "utm".to_string(),
                id,
                name,
            }),
            Err(e) => {
                for vm in &made {
                    let _ = provider.delete(vm).await;
                }
                panic!("could not create the throwaway VM {name}: {e}");
            }
        }
    }
    let target = made[1].clone();
    eprintln!(
        "made {:?}; deleting the middle one: {} ({})",
        made.iter().map(|vm| vm.name.as_str()).collect::<Vec<_>>(),
        target.name,
        target.id
    );

    let deleted = provider.delete(&target).await;

    let after: Vec<String> = automation
        .list()
        .await
        .expect("utmctl list after the delete")
        .into_iter()
        .map(|vm| vm.id)
        .collect();

    // Clean up the survivors before asserting anything, so a failure here
    // never leaves VMs behind either.
    for vm in &made {
        if vm.id != target.id {
            if let Err(e) = provider.delete(vm).await {
                eprintln!(
                    "warning: could not delete the throwaway VM {}: {e}",
                    vm.name
                );
            }
        }
    }

    deleted.expect("delete failed");
    let gone: Vec<&String> = before
        .iter()
        .chain(made.iter().map(|vm| &vm.id))
        .filter(|id| !after.contains(id))
        .collect();
    assert_eq!(
        gone,
        vec![&target.id],
        "delete must remove exactly the VM it was given, and nothing else"
    );
}

/// FR-5: "앱 기록의 UUID와 소유권을 대조한다. 이름 prefix만으로 외부 VM을
/// 삭제하지 않는다." A UUID this app never created is `Missing`, and asking to
/// delete it removes nothing at all.
#[tokio::test]
#[ignore]
async fn utm_never_deletes_a_vm_it_does_not_own() {
    let _utm = UTM_LOCK.lock().await;
    let Some(Fixture { provider, .. }) = fixture("utm_never_deletes_a_vm_it_does_not_own").await
    else {
        return;
    };

    // Everything UTM knows -- including the user's own VMs, which are the
    // ones that must all still be there afterwards. `Automation::list` is the
    // unfiltered view; `VmProvider::list` is this app's namespace only.
    //
    // Compared by id, and only for *disappearances*: the old `len()` check was
    // a global invariant that any other test breaks simply by cloning a VM
    // while this one runs (N1), whereas "nothing that was here is gone" is
    // precisely what refusing a foreign UUID has to guarantee, and additions
    // cannot make it lie.
    let all_before: Vec<String> = provider
        .automation()
        .list()
        .await
        .expect("utmctl list")
        .into_iter()
        .map(|vm| vm.id)
        .collect();

    let foreign = VmRef {
        backend: "utm".to_string(),
        id: FOREIGN_UUID.to_string(),
        // Deliberately inside the app's prefix: the refusal must come from
        // the UUID not being there, never from the name looking wrong.
        name: "wg-build-not-a-real-vm".to_string(),
    };

    assert_eq!(
        provider.status(&foreign).await.expect("status"),
        VmStatus::Missing,
        "a UUID UTM does not have must be Missing"
    );

    // A delete of something that is not there is a no-op, not an error and
    // not a search by name.
    provider
        .delete(&foreign)
        .await
        .expect("deleting a VM UTM does not have must be a no-op");
    provider
        .stop(&foreign)
        .await
        .expect("stopping a VM UTM does not have must be a no-op");

    let all_after: Vec<String> = provider
        .automation()
        .list()
        .await
        .expect("utmctl list")
        .into_iter()
        .map(|vm| vm.id)
        .collect();
    let disappeared: Vec<&String> = all_before
        .iter()
        .filter(|id| !all_after.contains(id))
        .collect();
    assert!(
        disappeared.is_empty(),
        "refusing the foreign UUID {FOREIGN_UUID} removed {disappeared:?} from UTM"
    );
}

/// Nothing here may create, start or delete anything, so it is safe to run on
/// any host that has UTM: it only reads. Kept separate so a run that is only
/// interested in "does this host answer at all" needs no image.
#[tokio::test]
#[ignore]
async fn utm_reports_the_recorded_vms_read_only() {
    let _utm = UTM_LOCK.lock().await;
    let Some(Fixture {
        provider,
        record,
        image,
        ..
    }) = fixture("utm_reports_the_recorded_vms_read_only").await
    else {
        return;
    };

    let health = provider.healthcheck().await.expect("healthcheck");
    eprintln!(
        "UTM available={} version={:?} detail={}",
        health.available, health.version, health.detail
    );
    assert!(health.available, "UTM is not usable: {}", health.detail);

    for check in provider.diagnostics().await {
        eprintln!("  doctor {}: ok={} {}", check.name, check.ok, check.detail);
    }

    let image_ref = ImageRef(image.id.clone());
    assert!(
        provider
            .image_exists(&image_ref)
            .await
            .expect("image_exists"),
        "the published image {} is not reported as ready",
        image.id
    );

    let mut expected: Vec<VmRef> = vec![image_vm(&image)];
    expected.extend(record.installer_vm.clone());
    let listed = provider.list().await.expect("list");
    for vm in &expected {
        assert!(
            listed.iter().any(|l| l.id == vm.id),
            "{} ({}) is missing from `utmctl list`",
            vm.name,
            vm.id
        );
        eprintln!(
            "  {} ({}) -> {:?}",
            vm.name,
            vm.id,
            provider.status(vm).await.expect("status")
        );
    }

    // Every VM this app owns is named inside its prefix -- the invariant the
    // ownership refusal rests on.
    for vm in &listed {
        assert!(
            vm.name.starts_with(&provider.config().vm_prefix),
            "VmProvider::list returned {} which is outside the app prefix",
            vm.name
        );
    }

    // A UTM image is not pullable, and the error has to say what to run
    // instead rather than looking like a transient network failure.
    let err = provider
        .pull_image(&image_ref)
        .await
        .expect_err("UTM has no image registry");
    assert!(
        err.to_string().contains("profile create"),
        "pull_image's error should name `wargame profile create`: {err}"
    );
}
