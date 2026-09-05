//! Security-sensitive plumbing shared by every asset download and log line
//! (DESIGN.md §3.6): filename sanitisation, hashing, staged writes, and log
//! redaction.

pub mod filename;
pub mod hash;
pub mod redact;
pub mod staging;

pub use filename::sanitize_filename;
pub use hash::{sha256_file, Sha256Writer};
pub use redact::{init_tracing, redact};
pub use staging::{StagingDir, StagingSink};
