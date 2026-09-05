//! Graphical console descriptor (DESIGN.md §5): Tart's window is just the
//! same `tart run` invocation without `--no-graphics`, so "opening a
//! console" for the CLI/TUI is running this argv in a foreground process.

use wargame_core::ports::ConsoleDescriptor;

/// The argv that shows `vm_name`'s window (network flag is appended by the
/// caller through `args::run_args`, since only it knows the session's
/// `NetworkMode`).
pub fn console(binary: &str, vm_name: &str) -> ConsoleDescriptor {
    ConsoleDescriptor::Window {
        argv: vec![
            binary.to_string(),
            "run".to_string(),
            vm_name.to_string(),
            "--no-clipboard".to_string(),
            "--no-audio".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_argv_has_no_graphics_flag() {
        let ConsoleDescriptor::Window { argv } = console("tart", "wg-abc") else {
            panic!("expected Window descriptor");
        };
        assert_eq!(
            argv,
            vec!["tart", "run", "wg-abc", "--no-clipboard", "--no-audio"]
        );
        assert!(!argv.iter().any(|a| a == "--no-graphics"));
    }
}
