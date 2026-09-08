//! Graphical console for a UTM VM (docs/utm-compatibility.md §3).
//!
//! There is no `utmctl console` verb and no working `attach`: `utmctl
//! attach` and `utmctl start --attach` both print `WARNING: attach command
//! is not implemented yet!`, exit 0 and stream nothing. What *is* verified
//! is that a VM with a display device gets its own UTM window titled with
//! the VM name, and that `utmctl start` launches/foregrounds the app, so
//! "open the console" is `utmctl start <uuid>` (never with `--hide`, which
//! is broken -- T7).

use wargame_core::ports::ConsoleDescriptor;

/// The argv that puts `vm_id`'s window in front of the user: a plain
/// `utmctl start`, which starts (or resumes) the VM and shows the app.
pub fn console(utmctl: &str, vm_id: &str) -> ConsoleDescriptor {
    ConsoleDescriptor::Window {
        argv: vec![utmctl.to_string(), "start".to_string(), vm_id.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_starts_the_vm_by_uuid_and_never_hides_the_window() {
        let ConsoleDescriptor::Window { argv } = console(
            "/Applications/UTM.app/Contents/MacOS/utmctl",
            "A8E9F979-A198-433A-B2DE-74A6D7302D2F",
        ) else {
            panic!("expected a Window descriptor");
        };
        assert_eq!(
            argv,
            vec![
                "/Applications/UTM.app/Contents/MacOS/utmctl",
                "start",
                "A8E9F979-A198-433A-B2DE-74A6D7302D2F",
            ]
        );
        // T7: `--hide` emits -10004 twice and hides nothing.
        assert!(!argv.iter().any(|a| a == "--hide"));
        // §3: `--attach` is a stub that streams nothing.
        assert!(!argv.iter().any(|a| a == "--attach"));
    }
}
