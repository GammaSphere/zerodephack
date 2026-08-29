//! strata - git repository archaeology with zero dependencies.
//!
//! The crate is layered, with dependencies pointing strictly downward:
//!
//! - [`util`] holds the primitives the standard library does not ship:
//!   DEFLATE decompression, civil-date arithmetic, glob matching, a thread pool.
//! - [`git`] reads the on-disk object database and exposes a single facade so
//!   callers never learn whether an object came from a loose file or from a
//!   delta chain inside a packfile.
//! - `analysis` walks history and computes the metrics.
//! - `render` turns results into tables, JSON, CSV or HTML.

pub mod git;
pub mod util;
