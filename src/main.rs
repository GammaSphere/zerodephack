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
    let head = repo.head()?;
    println!("git dir : {}", repo.git_dir().display());
    println!("head    : {}", head.describe());
    println!("shallow : {}", repo.is_shallow());
    println!("refs    : {}", repo.refs()?.len());

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
        let commit = repo.commit(oid)?;
        commits += 1;
        authors.insert(commit.author.identity_key());
        oldest = oldest.min(commit.author.time);
        newest = newest.max(commit.author.time);
        queue.extend(commit.parents.iter().copied());
    }

    println!("commits : {commits}");
    println!("authors : {}", authors.len());
    println!("span    : {oldest} .. {newest}");

    let tree = repo.tree(repo.commit(tip)?.tree)?;
    println!("tree    : {} entries at head", tree.entries.len());
    Ok(())
}
