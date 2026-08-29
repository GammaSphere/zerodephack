//! Temporary vertical slice: prove the object layer reads a real repository.

use std::collections::HashSet;
use std::path::PathBuf;

use strata::git::Repository;
use strata::git::oid::Oid;

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    if let Err(e) = run(&path) {
        eprintln!("strata: {e}");
        std::process::exit(1);
    }
}

fn run(path: &std::path::Path) -> strata::git::Result<()> {
    let repo = Repository::discover(path)?;
    let mut reader = repo.reader()?;

    // Temporary verification hook: dump every packed object the way
    // `git cat-file --batch-check` does, so the two can be diffed.
    if std::env::args().any(|a| a == "--dump") {
        let verify = std::env::args().any(|a| a == "--verify");
        let mut mismatches = 0usize;
        for oid in repo.packed_oids() {
            let object = reader.object(oid)?;
            if verify {
                let computed = strata::git::sha1::object_id(object.kind, &object.data);
                if computed != oid {
                    eprintln!("MISMATCH: index says {oid}, content hashes to {computed}");
                    mismatches += 1;
                }
            } else {
                println!("{oid} {} {}", object.kind, object.data.len());
            }
        }
        if verify {
            eprintln!("verified {} objects, {mismatches} mismatches", repo.packed_oids().len());
        }
        return Ok(());
    }
    let head = repo.head()?;
    println!("git dir : {}", repo.git_dir().display());
    println!("head    : {}", head.describe());
    println!("shallow : {}", repo.is_shallow());
    println!("refs    : {}", repo.refs()?.len());
    println!("packed  : {} objects", repo.packed_object_count());

    let Some(tip) = head.target() else {
        println!("no commits to walk");
        return Ok(());
    };

    let mut seen: HashSet<Oid> = HashSet::new();
    let mut queue = vec![tip];
    let mut commits = 0usize;
    let mut authors: HashSet<String> = HashSet::new();
    let mut oldest = i64::MAX;
    let mut newest = i64::MIN;

    while let Some(oid) = queue.pop() {
        if !seen.insert(oid) {
            continue;
        }
        let commit = reader.commit(oid)?;
        commits += 1;
        authors.insert(commit.author.identity_key());
        oldest = oldest.min(commit.author.time);
        newest = newest.max(commit.author.time);
        queue.extend(commit.parents.iter().copied());
    }

    println!("commits : {commits}");
    println!("authors : {}", authors.len());
    println!("span    : {oldest} .. {newest}");

    let options = strata::analysis::history::Options::new();
    let history = strata::analysis::history::walk(&repo, tip, &options)?;
    println!("walked  : {} commits, {} files, {} authors",
        history.commits.len(), history.paths.len(), history.authors.len());
    let total_changes: usize = history.commits.iter().map(|c| c.touched.len()).sum();
    println!("changes : {total_changes} file touches");

    if std::env::args().any(|a| a == "--dump-changes") {
        for commit in &history.commits {
            for (path, _kind) in &commit.touched {
                println!("{} {}", commit.oid, history.path(*path));
            }
        }
    }

    let tree_oid = reader.commit(tip)?.tree;
    let tree = reader.tree(tree_oid)?;
    println!("tree    : {} entries at head", tree.entries.len());
    Ok(())
}
