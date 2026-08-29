//! End-to-end tests against a committed repository fixture.
//!
//! `tests/fixtures/repo/dot_git` is a real git object database, packed, holding
//! 19 commits by two authors with a rename, a deletion, a merge, an empty
//! commit, a binary blob and a non-ASCII filename. It was built by
//! `tests/generate_repo_fixture.sh` and committed, so these tests need nothing
//! but cargo - **git is never invoked here**.
//!
//! It is stored as `dot_git` rather than `.git` because git records a nested
//! `.git` directory as a gitlink instead of tracking its files.
//!
//! Every expected number below was taken from git itself at authoring time; the
//! command that produced each one is named in the comment beside it.

use std::path::PathBuf;

use strata::analysis::history::{self, ChangeKind};
use strata::analysis::reports;
use strata::analysis::snapshot;
use strata::git::Repository;
use strata::git::refs::Head;
use strata::git::sha1;

/// 2024-04-01, comfortably after the fixture's last commit, so age and dormancy
/// figures are fixed rather than drifting with the wall clock.
const NOW: i64 = 1_711_929_600;

fn fixture() -> Repository {
    let git_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/repo/dot_git");
    Repository::open(git_dir, None).expect("fixture opens")
}

fn walk(include_merges: bool) -> history::History {
    let repo = fixture();
    let tip = repo.head().expect("head").target().expect("has commits");
    let options = history::Options {
        since: None,
        include_merges,
        max_files_per_commit: history::Options::DEFAULT_MAX_FILES,
    };
    history::walk(&repo, tip, &options).expect("walk succeeds")
}

#[test]
fn opens_a_packed_repository_and_finds_head() {
    let repo = fixture();
    match repo.head().expect("head resolves") {
        Head::Branch { name, .. } => assert_eq!(name, "refs/heads/main"),
        other => panic!("expected a branch, got {other:?}"),
    }
    assert!(!repo.is_shallow());
    // `git count-objects -v` reports 76 in-pack.
    assert_eq!(repo.packed_object_count(), 76);
}

/// The strongest check available: every reconstructed object must hash to the
/// id the pack index filed it under. The fixture holds 18 delta records with
/// chains up to depth 9, so this exercises delta resolution end to end.
#[test]
fn every_packed_object_hashes_to_its_own_id() {
    let repo = fixture();
    let mut reader = repo.reader().expect("reader");

    let oids = repo.packed_oids();
    assert_eq!(oids.len(), 76);

    for oid in oids {
        let object = reader.object(oid).expect("object reads");
        assert_eq!(
            sha1::object_id(object.kind, &object.data),
            oid,
            "reconstructed content does not match the id it was filed under"
        );
    }
}

#[test]
fn skips_merge_commits_by_default() {
    // `git rev-list --count --no-merges HEAD --` reports 18, and 19 with merges.
    assert_eq!(walk(false).commits.len(), 18);
    assert_eq!(walk(true).commits.len(), 19);
}

#[test]
fn finds_both_authors() {
    let history = walk(false);
    let mut emails: Vec<&str> = history.authors.iter().map(|a| a.email.as_str()).collect();
    emails.sort_unstable();
    assert_eq!(emails, ["ada@example.com", "grace@example.com"]);
}

#[test]
fn change_events_match_what_git_reports() {
    // `git log --no-merges --name-only` yields 22 (commit, path) pairs.
    let history = walk(false);
    let events: usize = history.commits.iter().map(|c| c.touched.len()).sum();
    assert_eq!(events, 22);
}

#[test]
fn counts_revisions_per_file() {
    let history = walk(false);
    let mut engine = 0;
    for commit in &history.commits {
        for &(path, _) in &commit.touched {
            if history.path(path) == "src/engine.py" {
                engine += 1;
            }
        }
    }
    // `git log --no-merges --oneline -- src/engine.py` reports 11.
    assert_eq!(engine, 11);
}

#[test]
fn collapses_an_exact_rename_into_one_event() {
    let history = walk(false);

    let mut renames = Vec::new();
    let mut parser_deletions = 0;
    for commit in &history.commits {
        for &(path, kind) in &commit.touched {
            let path = history.path(path);
            if kind == ChangeKind::Renamed {
                renames.push(path.to_string());
            }
            if path == "src/parser.py" && kind == ChangeKind::Deleted {
                parser_deletions += 1;
            }
        }
    }

    // git reports this as R100 src/parser.py -> src/parsing.py, a single event
    // at the new path.
    assert_eq!(renames, ["src/parsing.py"]);
    assert_eq!(
        parser_deletions, 0,
        "the departure was consumed by the rename"
    );
}

#[test]
fn an_empty_commit_touches_nothing() {
    let history = walk(false);
    let empty = history
        .commits
        .iter()
        .find(|c| c.summary == "an empty commit")
        .expect("the empty commit is in history");
    assert!(empty.touched.is_empty());
}

#[test]
fn the_snapshot_holds_only_what_still_exists() {
    let repo = fixture();
    let tip = repo.head().unwrap().target().unwrap();
    let current = snapshot::take(&repo, tip).expect("snapshot");

    let mut paths: Vec<&str> = current.files.iter().map(|f| f.path.as_str()).collect();
    paths.sort_unstable();

    // `git ls-tree -r --name-only HEAD` lists exactly these seven.
    assert_eq!(
        paths,
        [
            "README.md",
            "docs/café.md",
            "docs/design.md",
            "docs/main.md",
            "docs/side.md",
            "src/engine.py",
            "src/parsing.py",
        ]
    );

    // Deleted in the fixture's history, so it must not appear.
    assert!(!current.contains("docs/logo.png"));
    // Renamed away, so the old path is gone too.
    assert!(!current.contains("src/parser.py"));
}

#[test]
fn a_deleted_file_never_reaches_the_reports() {
    let repo = fixture();
    let tip = repo.head().unwrap().target().unwrap();
    let current = snapshot::take(&repo, tip).expect("snapshot");
    let history = walk(false);

    let hotspots = reports::hotspots(&history, &current, usize::MAX);
    assert!(
        hotspots.iter().all(|h| h.path != "docs/logo.png"),
        "you cannot refactor a file that is gone"
    );

    let ages = reports::age(&history, &current, NOW, usize::MAX);
    assert!(ages.iter().all(|a| a.path != "docs/logo.png"));
}

#[test]
fn the_busiest_file_is_the_top_hotspot() {
    let repo = fixture();
    let tip = repo.head().unwrap().target().unwrap();
    let current = snapshot::take(&repo, tip).expect("snapshot");
    let history = walk(false);

    let hotspots = reports::hotspots(&history, &current, 3);
    // engine.py has 11 revisions and 3000 lines; nothing else comes close on
    // either axis.
    assert_eq!(hotspots[0].path, "src/engine.py");
    assert_eq!(hotspots[0].revisions, 11);
    assert_eq!(hotspots[0].authors, 2, "Ada wrote it, Grace edited it");
    assert!(hotspots[0].score > hotspots[1].score);
}

#[test]
fn ownership_identifies_the_majority_author() {
    let repo = fixture();
    let tip = repo.head().unwrap().target().unwrap();
    let current = snapshot::take(&repo, tip).expect("snapshot");
    let history = walk(false);

    let owners = reports::owners(&history, &current, NOW, 180, usize::MAX);
    let engine = owners
        .iter()
        .find(|o| o.path == "src/engine.py")
        .expect("engine is attributed");

    // Ada made the initial commit and eight rounds of edits; Grace made two.
    assert_eq!(engine.main_author, "Ada Lovelace");
    assert_eq!(engine.authors, 2);
    assert_eq!(
        engine.bus_factor, 1,
        "Ada alone holds the majority of its history"
    );
    assert!(engine.main_share > 0.5);
}

#[test]
fn dormancy_is_measured_against_the_supplied_clock() {
    let repo = fixture();
    let tip = repo.head().unwrap().target().unwrap();
    let current = snapshot::take(&repo, tip).expect("snapshot");
    let history = walk(false);

    // The last commit is 2024-03-04 and NOW is 2024-04-01, so at a 180-day
    // threshold nobody is dormant, and at a 7-day threshold everybody is.
    let patient = reports::owners(&history, &current, NOW, 180, usize::MAX);
    assert!(patient.iter().all(|o| !o.orphaned));

    let impatient = reports::owners(&history, &current, NOW, 7, usize::MAX);
    assert!(impatient.iter().all(|o| o.orphaned));
}

#[test]
fn coupling_finds_files_that_change_together() {
    let repo = fixture();
    let tip = repo.head().unwrap().target().unwrap();
    let current = snapshot::take(&repo, tip).expect("snapshot");
    let history = walk(false);

    // engine.py and parser.py shared two commits, but parser.py was renamed
    // away, so the pair cannot appear - both sides must still exist.
    let pairs = reports::coupling(&history, &current, 1, usize::MAX);
    assert!(
        pairs.iter().all(|p| p.a != "src/parser.py" && p.b != "src/parser.py"),
        "a file that no longer exists cannot be coupled to anything"
    );

    // docs/café.md arrived alongside the binary logo in one commit; the logo is
    // deleted, so that pair is gone too. What remains must be well formed.
    for pair in &pairs {
        assert!(pair.co_changes >= 1);
        assert!(pair.degree > 0.0 && pair.degree <= 1.0);
        assert!(current.contains(&pair.a) && current.contains(&pair.b));
    }
}

#[test]
fn a_since_filter_narrows_the_walk() {
    let repo = fixture();
    let tip = repo.head().unwrap().target().unwrap();

    // 2024-02-01, which excludes the initial commit and the eight edit rounds.
    let options = history::Options {
        since: Some(1_706_745_600),
        include_merges: false,
        max_files_per_commit: history::Options::DEFAULT_MAX_FILES,
    };
    let narrowed = history::walk(&repo, tip, &options).expect("walk");

    assert!(narrowed.commits.len() < 18);
    assert!(
        narrowed.commits.iter().all(|c| c.time >= 1_706_745_600),
        "no commit older than the cutoff survives"
    );
}

#[test]
fn handles_a_non_ascii_filename() {
    let repo = fixture();
    let tip = repo.head().unwrap().target().unwrap();
    let current = snapshot::take(&repo, tip).expect("snapshot");

    let file = current
        .get("docs/café.md")
        .expect("a non-ASCII path survives the round trip");
    assert!(!file.is_binary);
    assert_eq!(file.lines, 1);
}

#[test]
fn summarises_the_repository() {
    let repo = fixture();
    let tip = repo.head().unwrap().target().unwrap();
    let current = snapshot::take(&repo, tip).expect("snapshot");
    let history = walk(false);

    let summary = reports::summarise(&history, &current, NOW, 180);
    assert_eq!(summary.commits, 18);
    assert_eq!(summary.authors, 2);
    assert_eq!(summary.files_now, 7);
    assert_eq!(summary.orphaned_files, 0);
    // The fixture's first commit is 2024-01-01 and its last is 2024-03-03.
    assert_eq!(summary.first_commit, 1_704_067_200);
}
