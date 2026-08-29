//! The four questions strata answers.
//!
//! Each takes the single history pass plus the snapshot of what exists now, and
//! returns rows ready to render. Nothing here touches the filesystem.
//!
//! Every metric here is a proxy, and the honest framing matters: revision count
//! stands in for churn, line count stands in for complexity, and co-change
//! stands in for coupling. They point at places worth a human's attention, not
//! at defects.

use std::collections::HashMap;

use crate::analysis::history::{ChangeKind, History};
use crate::analysis::snapshot::Snapshot;

pub const SECONDS_PER_DAY: i64 = 86_400;

/// How long an author must be silent before their files count as orphaned.
pub const DEFAULT_DORMANT_DAYS: i64 = 180;

/// Per-file counts, the shared basis for every report below.
struct FileStats {
    revisions: usize,
    last_change: i64,
    /// Revisions by author index.
    by_author: HashMap<u32, usize>,
    last_by_author: HashMap<u32, i64>,
}

fn tally(history: &History) -> HashMap<u32, FileStats> {
    let mut stats: HashMap<u32, FileStats> = HashMap::new();

    for commit in &history.commits {
        for &(path, kind) in &commit.touched {
            // A deletion is not work done *on* a file, it is the end of it.
            // Counting it would make removed files look freshly maintained.
            if kind == ChangeKind::Deleted {
                continue;
            }

            let entry = stats.entry(path).or_insert_with(|| FileStats {
                revisions: 0,
                last_change: i64::MIN,
                by_author: HashMap::new(),
                last_by_author: HashMap::new(),
            });

            entry.revisions += 1;
            entry.last_change = entry.last_change.max(commit.time);
            *entry.by_author.entry(commit.author).or_insert(0) += 1;
            let seen = entry
                .last_by_author
                .entry(commit.author)
                .or_insert(i64::MIN);
            *seen = (*seen).max(commit.time);
        }
    }

    stats
}

/// The most recent commit by each author, anywhere in the repository. Used to
/// tell a file whose owner is still around from one whose owner has gone.
fn author_last_seen(history: &History) -> HashMap<u32, i64> {
    let mut last: HashMap<u32, i64> = HashMap::new();
    for commit in &history.commits {
        let entry = last.entry(commit.author).or_insert(i64::MIN);
        *entry = (*entry).max(commit.time);
    }
    last
}

// ---------------------------------------------------------------- hotspots

#[derive(Debug, Clone)]
pub struct Hotspot {
    pub path: String,
    pub revisions: usize,
    pub authors: usize,
    pub lines: usize,
    /// Revisions times size, each normalised against the highest in the
    /// repository, so the result is comparable within one report and
    /// meaningless across two. Size is taken on a log scale; see [`hotspots`].
    pub score: f64,
    pub last_change: i64,
}

/// Files that change often *and* are large.
///
/// Either signal alone misleads. A changelog has hundreds of revisions and no
/// complexity; a vendored library is enormous and never touched. The product is
/// what points at code that is both intricate and unsettled.
///
/// Size enters on a log scale, and that choice is load-bearing. With a linear
/// product a single enormous file wins outright: a 48,000-line generated test
/// fixture touched twice outranked a 3,700-line source file touched thirteen
/// times, which is precisely backwards. Maintenance risk does not grow linearly
/// with line count, so compressing size lets revisions matter again.
///
/// Revisions stay linear. Doubling how often a file is edited really does
/// double the number of chances to get it wrong.
pub fn hotspots(history: &History, snapshot: &Snapshot, limit: usize) -> Vec<Hotspot> {
    let stats = tally(history);

    let mut rows: Vec<Hotspot> = stats
        .iter()
        .filter_map(|(&path, stat)| {
            let path = history.path(path);
            // Only files that still exist can be refactored.
            let file = snapshot.get(path)?;
            Some(Hotspot {
                path: path.to_string(),
                revisions: stat.revisions,
                authors: stat.by_author.len(),
                lines: file.lines,
                score: 0.0,
                last_change: stat.last_change,
            })
        })
        .collect();

    let max_revisions = rows.iter().map(|r| r.revisions).max().unwrap_or(1).max(1) as f64;
    let max_lines = rows.iter().map(|r| r.lines).max().unwrap_or(1).max(1) as f64;
    for row in &mut rows {
        row.score = hotspot_score(row.revisions, row.lines, max_revisions, max_lines);
    }

    rows.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    rows.truncate(limit);
    rows
}

/// Churn times log-scaled size, both relative to the repository's maximum.
fn hotspot_score(revisions: usize, lines: usize, max_revisions: f64, max_lines: f64) -> f64 {
    let churn = revisions as f64 / max_revisions;
    let size = (1.0 + lines as f64).ln() / (1.0 + max_lines).ln();
    churn * size
}

// --------------------------------------------------------------- ownership

#[derive(Debug, Clone)]
pub struct Owner {
    pub path: String,
    pub revisions: usize,
    pub authors: usize,
    pub main_author: String,
    /// Share of revisions made by the main author, 0.0 to 1.0.
    pub main_share: f64,
    /// How many people hold the majority of a file's history between them.
    /// One means a single person carries it.
    pub bus_factor: usize,
    /// Days since the main author last committed anywhere in the repository.
    pub owner_silent_days: i64,
    /// True when the main author has been dormant past the threshold, so the
    /// knowledge behind this file has probably left the building.
    pub orphaned: bool,
}

/// Who knows each file, and whether they are still here.
pub fn owners(
    history: &History,
    snapshot: &Snapshot,
    now: i64,
    dormant_days: i64,
    limit: usize,
) -> Vec<Owner> {
    let stats = tally(history);
    let last_seen = author_last_seen(history);

    let mut rows: Vec<Owner> = stats
        .iter()
        .filter_map(|(&path, stat)| {
            let path = history.path(path);
            if !snapshot.contains(path) {
                return None;
            }

            // Ties break on author index so the output is stable run to run.
            let (&main, &main_revisions) = stat
                .by_author
                .iter()
                .max_by_key(|(author, count)| (**count, std::cmp::Reverse(**author)))?;

            let silent_days = last_seen
                .get(&main)
                .map(|&seen| (now - seen).max(0) / SECONDS_PER_DAY)
                .unwrap_or(0);

            Some(Owner {
                path: path.to_string(),
                revisions: stat.revisions,
                authors: stat.by_author.len(),
                main_author: history.author(main).name.clone(),
                main_share: main_revisions as f64 / stat.revisions as f64,
                bus_factor: bus_factor(stat),
                owner_silent_days: silent_days,
                orphaned: silent_days >= dormant_days,
            })
        })
        .collect();

    // Riskiest first: orphaned files, then the ones fewest people know, then
    // the most active among those.
    rows.sort_by(|a, b| {
        b.orphaned
            .cmp(&a.orphaned)
            .then_with(|| a.bus_factor.cmp(&b.bus_factor))
            .then_with(|| b.revisions.cmp(&a.revisions))
            .then_with(|| a.path.cmp(&b.path))
    });
    rows.truncate(limit);
    rows
}

/// The smallest number of authors whose combined revisions exceed half a
/// file's history. This is the usual reading of "bus factor": lose these
/// people and most of what is known about the file goes with them.
fn bus_factor(stat: &FileStats) -> usize {
    let mut contributions: Vec<usize> = stat.by_author.values().copied().collect();
    contributions.sort_unstable_by(|a, b| b.cmp(a));

    let majority = stat.revisions / 2;
    let mut running = 0;
    for (count, contribution) in contributions.iter().enumerate() {
        running += contribution;
        if running > majority {
            return count + 1;
        }
    }
    contributions.len().max(1)
}

// ---------------------------------------------------------------- coupling

#[derive(Debug, Clone)]
pub struct Coupling {
    pub a: String,
    pub b: String,
    /// Commits that touched both files.
    pub co_changes: usize,
    pub revisions_a: usize,
    pub revisions_b: usize,
    /// Co-changes over the union of both files' revisions, as a fraction.
    /// Symmetric, so neither file is privileged as the cause.
    pub degree: f64,
    /// True when the two files sit in different top-level directories, which
    /// is where coupling is most likely to be a surprise.
    pub crosses_directories: bool,
}

/// Files that keep changing together.
///
/// Two files edited in the same commit again and again share something the
/// directory layout does not show. The interesting rows are the ones that cross
/// a directory boundary, because those are the dependencies nobody drew.
pub fn coupling(
    history: &History,
    snapshot: &Snapshot,
    min_co_changes: usize,
    limit: usize,
) -> Vec<Coupling> {
    let stats = tally(history);
    let mut pairs: HashMap<(u32, u32), usize> = HashMap::new();

    for commit in &history.commits {
        // A commit that rewrote half the tree is not evidence that its files
        // belong together.
        if commit.too_broad_for_coupling {
            continue;
        }

        let mut touched: Vec<u32> = commit
            .touched
            .iter()
            .filter(|(_, kind)| *kind != ChangeKind::Deleted)
            .map(|&(path, _)| path)
            .collect();
        touched.sort_unstable();
        touched.dedup();

        for (i, &a) in touched.iter().enumerate() {
            for &b in &touched[i + 1..] {
                *pairs.entry((a, b)).or_insert(0) += 1;
            }
        }
    }

    let mut rows: Vec<Coupling> = pairs
        .into_iter()
        .filter(|&(_, count)| count >= min_co_changes)
        .filter_map(|((a, b), count)| {
            let (path_a, path_b) = (history.path(a), history.path(b));
            if !snapshot.contains(path_a) || !snapshot.contains(path_b) {
                return None;
            }

            let revisions_a = stats.get(&a)?.revisions;
            let revisions_b = stats.get(&b)?.revisions;
            let union = revisions_a + revisions_b - count;

            Some(Coupling {
                a: path_a.to_string(),
                b: path_b.to_string(),
                co_changes: count,
                revisions_a,
                revisions_b,
                degree: if union == 0 {
                    0.0
                } else {
                    count as f64 / union as f64
                },
                crosses_directories: top_level(path_a) != top_level(path_b),
            })
        })
        .collect();

    rows.sort_by(|a, b| {
        b.degree
            .partial_cmp(&a.degree)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.co_changes.cmp(&a.co_changes))
            .then_with(|| a.a.cmp(&b.a))
            .then_with(|| a.b.cmp(&b.b))
    });
    rows.truncate(limit);
    rows
}

fn top_level(path: &str) -> &str {
    path.split_once('/').map(|(head, _)| head).unwrap_or("")
}

// --------------------------------------------------------------------- age

#[derive(Debug, Clone)]
pub struct AgeRow {
    pub path: String,
    pub days_since_change: i64,
    pub revisions: usize,
}

/// How long each file has been left alone.
///
/// Stable code is not a problem - it is the goal. This report exists to tell
/// stable from abandoned, and to show which parts of a tree are still moving.
pub fn age(history: &History, snapshot: &Snapshot, now: i64, limit: usize) -> Vec<AgeRow> {
    let stats = tally(history);

    let mut rows: Vec<AgeRow> = stats
        .iter()
        .filter_map(|(&path, stat)| {
            let path = history.path(path);
            if !snapshot.contains(path) {
                return None;
            }
            Some(AgeRow {
                path: path.to_string(),
                days_since_change: (now - stat.last_change).max(0) / SECONDS_PER_DAY,
                revisions: stat.revisions,
            })
        })
        .collect();

    rows.sort_by(|a, b| {
        b.days_since_change
            .cmp(&a.days_since_change)
            .then_with(|| a.path.cmp(&b.path))
    });
    rows.truncate(limit);
    rows
}

/// Headline numbers for the top of any report.
pub struct Summary {
    pub commits: usize,
    pub authors: usize,
    pub files_now: usize,
    pub files_ever: usize,
    pub lines_now: usize,
    pub first_commit: i64,
    pub last_commit: i64,
    /// Files whose main author has gone dormant.
    pub orphaned_files: usize,
    /// Files only one person has ever touched.
    pub single_author_files: usize,
}

pub fn summarise(history: &History, snapshot: &Snapshot, now: i64, dormant_days: i64) -> Summary {
    let owners = owners(history, snapshot, now, dormant_days, usize::MAX);

    Summary {
        commits: history.commits.len(),
        authors: history.authors.len(),
        files_now: snapshot.files.len(),
        files_ever: history.paths.len(),
        lines_now: snapshot.total_lines(),
        first_commit: history.oldest_time(),
        last_commit: history.newest_time(),
        orphaned_files: owners.iter().filter(|o| o.orphaned).count(),
        single_author_files: owners.iter().filter(|o| o.authors == 1).count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats_with(contributions: &[usize]) -> FileStats {
        let by_author: HashMap<u32, usize> = contributions
            .iter()
            .enumerate()
            .map(|(i, &n)| (i as u32, n))
            .collect();
        FileStats {
            revisions: contributions.iter().sum(),
            last_change: 0,
            by_author,
            last_by_author: HashMap::new(),
        }
    }

    #[test]
    fn a_sole_author_gives_a_bus_factor_of_one() {
        assert_eq!(bus_factor(&stats_with(&[10])), 1);
    }

    #[test]
    fn a_dominant_author_gives_a_bus_factor_of_one() {
        // Nine of ten commits: losing this person loses the file.
        assert_eq!(bus_factor(&stats_with(&[9, 1])), 1);
    }

    #[test]
    fn an_even_split_needs_two_people() {
        assert_eq!(bus_factor(&stats_with(&[5, 5])), 2);
        assert_eq!(bus_factor(&stats_with(&[4, 3, 3])), 2);
    }

    #[test]
    fn a_wide_spread_needs_more_people() {
        assert_eq!(bus_factor(&stats_with(&[1, 1, 1, 1, 1])), 3);
        assert_eq!(bus_factor(&stats_with(&[2, 2, 2, 2])), 3);
    }

    #[test]
    fn a_huge_rarely_touched_file_does_not_outrank_real_churn() {
        // The case that forced log scaling, taken from a real repository: a
        // 48,181-line generated fixture touched twice against a 3,720-line
        // source file touched thirteen times. A linear product ranks the
        // fixture first, which is exactly backwards.
        let fixture = hotspot_score(2, 48_181, 13.0, 48_181.0);
        let source = hotspot_score(13, 3_720, 13.0, 48_181.0);
        assert!(
            source > fixture,
            "source {source:.3} should outrank fixture {fixture:.3}"
        );
    }

    #[test]
    fn churn_still_dominates_between_similar_sizes() {
        let often = hotspot_score(20, 1000, 20.0, 1000.0);
        let rarely = hotspot_score(2, 1000, 20.0, 1000.0);
        assert!(often > rarely * 5.0, "ten times the churn must show");
    }

    #[test]
    fn an_empty_file_scores_zero() {
        assert_eq!(hotspot_score(50, 0, 50.0, 1000.0), 0.0);
    }

    #[test]
    fn top_level_directory_splits_paths() {
        assert_eq!(top_level("src/git/pack.rs"), "src");
        assert_eq!(top_level("README.md"), "");
        assert_eq!(top_level("a/b"), "a");
    }
}
