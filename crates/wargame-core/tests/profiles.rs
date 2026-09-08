//! Both shipped profile definitions parse via `Profile::from_yaml` (PLAN.md
//! Task 6; DESIGN.md §6, MVP.md §13).

use std::path::PathBuf;

use wargame_core::domain::profile::Profile;

fn read_profile(filename: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles")
        .join(filename);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

#[test]
fn macos_minimal_arm64_parses() {
    let profile = Profile::from_yaml(&read_profile("macos-minimal-arm64.yaml")).unwrap();
    assert_eq!(profile.name, "macos-minimal-arm64");
    assert_eq!(profile.provision.scripts, vec!["scripts/macos/common.sh"]);
    assert_eq!(profile.image_name(), "wargame-profile-macos-minimal-arm64");
}

#[test]
fn macos_re_arm64_parses() {
    let profile = Profile::from_yaml(&read_profile("macos-re-arm64.yaml")).unwrap();
    assert_eq!(profile.name, "macos-re-arm64");
    assert_eq!(profile.provision.scripts.len(), 2);
    assert_eq!(
        profile.provision.scripts,
        vec!["scripts/macos/common.sh", "scripts/macos/reversing.sh"]
    );
    assert!(profile.tools.verify.len() >= 3);
    assert!(profile
        .tools
        .verify
        .iter()
        .any(|cmd| cmd.contains("ghidraRun")));
    assert!(profile
        .tools
        .verify
        .iter()
        .any(|cmd| cmd.contains("llvm-objdump")));
    assert_eq!(profile.image_name(), "wargame-profile-macos-re-arm64");
}

// --- the multi-OS templates (spec.md §4) ---------------------------------

fn read_script(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/scripts")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

#[test]
fn kali_arm64_parses_with_the_default_tool_set() {
    let profile = Profile::from_yaml(&read_profile("kali-arm64.yaml")).unwrap();
    assert_eq!(profile.name, "kali-arm64");
    assert_eq!(profile.provision.scripts, vec!["scripts/kali/common.sh"]);
    assert_eq!(profile.workspace.root, "/home/kali/wargame");
    assert_eq!(profile.guest.user, "kali");

    // spec.md §3 FR-3's Kali list: Python 3, pip/venv, Git, curl, unzip,
    // file, GDB, binutils, build-essential -- one verify command each.
    for expected in [
        "python3 --version",
        "python3 -m pip --version",
        "python3 -m venv --help",
        "git --version",
        "curl --version",
        "unzip -v",
        "file --version",
        "gdb --version",
        "objdump --version",
        "gcc --version",
        "make --version",
    ] {
        assert!(
            profile.tools.verify.iter().any(|cmd| cmd == expected),
            "kali-arm64 does not verify {expected:?}: {:?}",
            profile.tools.verify
        );
    }
}

#[test]
fn kali_x86_64_differs_from_the_arm64_template_only_in_arch_and_mode() {
    let arm = Profile::from_yaml(&read_profile("kali-arm64.yaml")).unwrap();
    let x86 = Profile::from_yaml(&read_profile("kali-x86_64.yaml")).unwrap();
    assert_eq!(x86.tools.verify, arm.tools.verify);
    assert_eq!(x86.provision.scripts, arm.provision.scripts);
    assert_eq!(x86.workspace.root, arm.workspace.root);
    assert_eq!(x86.resources, arm.resources);
    assert_eq!(x86.guest.user, arm.guest.user);
}

#[test]
fn windows_x86_64_parses_with_the_default_tool_set() {
    let profile = Profile::from_yaml(&read_profile("windows-x86_64.yaml")).unwrap();
    assert_eq!(
        profile.provision.scripts,
        vec!["scripts/windows/common.ps1"]
    );
    assert_eq!(profile.workspace.root, "C:\\wargame");

    // spec.md §3 FR-3's Windows list: PowerShell, OpenSSH Server, Python 3,
    // Git, 7-Zip.
    for expected in [
        "powershell -NoProfile -Command $PSVersionTable.PSVersion.ToString()",
        "sc.exe query sshd",
        "python --version",
        "git --version",
        "7z i",
    ] {
        assert!(
            profile.tools.verify.iter().any(|cmd| cmd == expected),
            "windows-x86_64 does not verify {expected:?}: {:?}",
            profile.tools.verify
        );
    }
}

/// spec.md §3 FR-3: "설치 출처와 검증 명령을 스크립트에 명시한다" -- every
/// command a profile promises to verify has to be named in the script that
/// installs the thing, next to where that install comes from.
#[test]
fn every_provisioning_script_names_its_install_source_and_verify_commands() {
    for (yaml, script_rel, source) in [
        ("kali-arm64.yaml", "kali/common.sh", "apt-get"),
        ("kali-x86_64.yaml", "kali/common.sh", "apt-get"),
        ("windows-x86_64.yaml", "windows/common.ps1", "winget"),
    ] {
        let profile = Profile::from_yaml(&read_profile(yaml)).unwrap();
        let script = read_script(script_rel);
        assert!(
            script.contains(source),
            "{script_rel} does not name its install source ({source})"
        );
        for check in &profile.tools.verify {
            assert!(
                script.contains(check.as_str()),
                "{script_rel} does not name the verify command {check:?}"
            );
        }
    }
}

/// The example spec.md §4's flow tells users to copy. It is shipped, but no
/// built-in profile runs it: it is a starting point, not part of the
/// baseline.
#[test]
fn the_custom_script_example_ships_and_no_builtin_profile_runs_it() {
    for example in ["custom/my-tools.sh", "custom/my-tools.ps1"] {
        let script = read_script(example);
        assert!(
            script.contains("wargame profile build"),
            "{example} should say how it is run"
        );
    }
    for yaml in [
        "macos-minimal-arm64.yaml",
        "macos-re-arm64.yaml",
        "kali-arm64.yaml",
        "kali-x86_64.yaml",
        "windows-x86_64.yaml",
    ] {
        let profile = Profile::from_yaml(&read_profile(yaml)).unwrap();
        assert!(
            !profile
                .provision
                .scripts
                .iter()
                .any(|s| s.starts_with("scripts/custom/")),
            "{yaml} runs the custom example"
        );
    }
}
