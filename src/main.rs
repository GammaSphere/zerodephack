//! strata - git repository archaeology with zero dependencies.
//!
//! Reads a `.git` directory directly and reports where a codebase's risk is
//! concentrated. Never invokes git, never touches the network.

use std::io::{self, Write};
use std::process::ExitCode;

use strata::analysis::history::{self, History};
use strata::analysis::reports::{self, SECONDS_PER_DAY};
use strata::analysis::snapshot::{self, Snapshot};
use strata::cli::{self, Args, Command, Format, Parsed};
use strata::git::Repository;
use strata::render::ansi::{Palette, Style};
use strata::render::table::{Align, Table};
use strata::render::{csv, json};
use strata::util::date;
use strata::util::glob::Pattern;

fn main() -> ExitCode {
    let now = date::now();

    let parsed = match cli::parse(std::env::args().skip(1), now) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("strata: {message}");
            eprintln!("try `strata --help`");
            return ExitCode::from(cli::USAGE_EXIT_CODE as u8);
        }
    };

    match parsed {
        Parsed::Help => {
            print!("{}", cli::help());
            ExitCode::SUCCESS
        }
        Parsed::Version => {
            println!("strata {}", cli::VERSION);
            ExitCode::SUCCESS
        }
        Parsed::Run(args) => match run(&args, now) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("strata: {message}");
                ExitCode::FAILURE
            }
        },
    }
}

fn run(args: &Args, now: i64) -> Result<(), String> {
    let repo = Repository::discover(&args.repository).map_err(|e| e.to_string())?;
    let palette = Palette::detect(args.no_color || args.format != Format::Table);

    for problem in repo.pack_problems() {
        eprintln!("strata: warning: {problem}");
    }

    let head = repo.head().map_err(|e| e.to_string())?;
    let Some(tip) = head.target() else {
        println!("This repository has no commits yet ({}).", head.describe());
        return Ok(());
    };

    if args.verify {
        verify(&repo)?;
    }

    let options = history::Options {
        since: args.since,
        include_merges: args.include_merges,
        max_files_per_commit: history::Options::DEFAULT_MAX_FILES,
    };

    let mut walked = history::walk(&repo, tip, &options).map_err(|e| e.to_string())?;
    let mut current = snapshot::take(&repo, tip).map_err(|e| e.to_string())?;

    if let Some(filter) = &args.path_filter {
        let pattern = Pattern::new(filter);
        restrict(&mut walked, &mut current, &pattern);
    }

    if walked.commits.is_empty() {
        println!("No commits matched. Try widening --since, or dropping --path.");
        return Ok(());
    }

    // Warnings that qualify every number below, said once and up front.
    if repo.is_shallow() {
        eprintln!(
            "strata: warning: shallow clone, so every figure here is a lower bound"
        );
    }
    if walked.unreadable > 0 {
        eprintln!(
            "strata: warning: {} commits referenced but not present",
            walked.unreadable
        );
    }

    let out = io::stdout();
    let mut out = out.lock();

    let rendered = match args.command {
        Command::Summary => render_summary(&repo, &head.describe(), &walked, &current, args, now),
        Command::Hotspots => render_hotspots(&walked, &current, args, &palette),
        Command::Owners => render_owners(&walked, &current, args, now, &palette),
        Command::Coupling => render_coupling(&walked, &current, args, &palette),
        Command::Age => render_age(&walked, &current, args, now, &palette),
    };

    out.write_all(rendered.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Check every packed object against the id the index filed it under.
///
/// This is the strongest correctness check strata can make on itself: if a
/// delta chain were applied wrongly by one byte, the hash would not match.
fn verify(repo: &Repository) -> Result<(), String> {
    let mut reader = repo.reader().map_err(|e| e.to_string())?;
    let oids = repo.packed_oids();
    let mut mismatched = Vec::new();

    for oid in &oids {
        let object = reader.object(*oid).map_err(|e| e.to_string())?;
        let computed = strata::git::sha1::object_id(object.kind, &object.data);
        if computed != *oid {
            mismatched.push((*oid, computed));
        }
    }

    if !mismatched.is_empty() {
        for (expected, actual) in mismatched.iter().take(10) {
            eprintln!("strata: object {expected} hashes to {actual}");
        }
        return Err(format!(
            "{} of {} objects failed verification",
            mismatched.len(),
            oids.len()
        ));
    }

    eprintln!("strata: verified {} packed objects", oids.len());
    Ok(())
}

/// Drop everything outside a `--path` filter, in both history and the snapshot.
fn restrict(walked: &mut History, current: &mut Snapshot, pattern: &Pattern) {
    let keep: Vec<bool> = walked.paths.iter().map(|p| pattern.matches(p)).collect();

    for commit in &mut walked.commits {
        commit.touched.retain(|&(path, _)| keep[path as usize]);
    }
    walked.commits.retain(|c| !c.touched.is_empty());

    current.retain(|file| pattern.matches(&file.path));
}

// ------------------------------------------------------------------ summary

fn render_summary(
    repo: &Repository,
    head: &str,
    walked: &History,
    current: &Snapshot,
    args: &Args,
    now: i64,
) -> String {
    let summary = reports::summarise(walked, current, now, args.dormant_days);
    let span_days = (summary.last_commit - summary.first_commit).max(0) / SECONDS_PER_DAY;

    if args.format == Format::Json {
        return json::to_string(&json::Value::object(vec![
            ("head", head.into()),
            ("shallow", repo.is_shallow().into()),
            ("commits", summary.commits.into()),
            ("authors", summary.authors.into()),
            ("files_now", summary.files_now.into()),
            ("files_ever", summary.files_ever.into()),
            ("lines_now", summary.lines_now.into()),
            (
                "first_commit",
                date::from_epoch(summary.first_commit).to_string().into(),
            ),
            (
                "last_commit",
                date::from_epoch(summary.last_commit).to_string().into(),
            ),
            ("single_author_files", summary.single_author_files.into()),
            ("orphaned_files", summary.orphaned_files.into()),
        ]));
    }

    let name = repo
        .work_tree()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repository".to_string());

    let mut out = format!("{name} · {head}\n\n");

    out.push_str(&format!(
        "  {} commits by {} over {}\n",
        summary.commits,
        plural(summary.authors, "author", "authors"),
        date::humanise_days(span_days)
    ));
    out.push_str(&format!(
        "  {} files, {} lines, first commit {}\n",
        summary.files_now,
        summary.lines_now,
        date::from_epoch(summary.first_commit)
    ));

    let single = percent(summary.single_author_files, summary.files_now);
    out.push_str(&format!(
        "\n  {} of files have exactly one author ({})\n",
        single, summary.single_author_files
    ));
    out.push_str(&format!(
        "  {} files whose main author has been quiet for {} days or more\n",
        summary.orphaned_files, args.dormant_days
    ));

    out.push_str("\nNext: strata hotspots · strata owners · strata coupling\n");
    out
}

// ----------------------------------------------------------------- hotspots

fn render_hotspots(
    walked: &History,
    current: &Snapshot,
    args: &Args,
    palette: &Palette,
) -> String {
    let rows = reports::hotspots(walked, current, args.top);

    match args.format {
        Format::Json => json::to_string(&json::Value::Array(
            rows.iter()
                .map(|row| {
                    json::Value::object(vec![
                        ("path", row.path.clone().into()),
                        ("revisions", row.revisions.into()),
                        ("authors", row.authors.into()),
                        ("lines", row.lines.into()),
                        ("score", row.score.into()),
                        (
                            "last_change",
                            date::from_epoch(row.last_change).to_string().into(),
                        ),
                    ])
                })
                .collect(),
        )),

        Format::Csv => {
            let mut writer =
                csv::Writer::new(&["path", "revisions", "authors", "lines", "score", "last_change"]);
            for row in &rows {
                writer.row(&[
                    row.path.clone(),
                    row.revisions.to_string(),
                    row.authors.to_string(),
                    row.lines.to_string(),
                    format!("{:.4}", row.score),
                    date::from_epoch(row.last_change).to_string(),
                ]);
            }
            writer.finish()
        }

        Format::Table => {
            if rows.is_empty() {
                return "No files to rank.\n".to_string();
            }

            let mut table = Table::new(
                &["file", "revs", "authors", "lines", "score", "last change"],
                &[
                    Align::Left,
                    Align::Right,
                    Align::Right,
                    Align::Right,
                    Align::Right,
                    Align::Right,
                ],
            )
            .flexible_column(0);

            for row in &rows {
                table.push(vec![
                    row.path.clone(),
                    row.revisions.to_string(),
                    row.authors.to_string(),
                    row.lines.to_string(),
                    palette.severity(&format!("{:.2}", row.score), row.score),
                    date::from_epoch(row.last_change).to_string(),
                ]);
            }

            format!(
                "{}\n{}",
                table.render(palette),
                palette.paint(
                    Style::Dim,
                    "score = revisions x size, each relative to this repository's own maximum\n"
                )
            )
        }
    }
}

// ------------------------------------------------------------------- owners

fn render_owners(
    walked: &History,
    current: &Snapshot,
    args: &Args,
    now: i64,
    palette: &Palette,
) -> String {
    let rows = reports::owners(walked, current, now, args.dormant_days, args.top);

    match args.format {
        Format::Json => json::to_string(&json::Value::Array(
            rows.iter()
                .map(|row| {
                    json::Value::object(vec![
                        ("path", row.path.clone().into()),
                        ("revisions", row.revisions.into()),
                        ("authors", row.authors.into()),
                        ("main_author", row.main_author.clone().into()),
                        ("main_share", row.main_share.into()),
                        ("bus_factor", row.bus_factor.into()),
                        ("owner_silent_days", row.owner_silent_days.into()),
                        ("orphaned", row.orphaned.into()),
                    ])
                })
                .collect(),
        )),

        Format::Csv => {
            let mut writer = csv::Writer::new(&[
                "path",
                "revisions",
                "authors",
                "main_author",
                "main_share",
                "bus_factor",
                "owner_silent_days",
                "orphaned",
            ]);
            for row in &rows {
                writer.row(&[
                    row.path.clone(),
                    row.revisions.to_string(),
                    row.authors.to_string(),
                    row.main_author.clone(),
                    format!("{:.4}", row.main_share),
                    row.bus_factor.to_string(),
                    row.owner_silent_days.to_string(),
                    row.orphaned.to_string(),
                ]);
            }
            writer.finish()
        }

        Format::Table => {
            if rows.is_empty() {
                return "No files to attribute.\n".to_string();
            }

            let mut table = Table::new(
                &["file", "revs", "authors", "main author", "share", "bus", "owner last seen"],
                &[
                    Align::Left,
                    Align::Right,
                    Align::Right,
                    Align::Left,
                    Align::Right,
                    Align::Right,
                    Align::Right,
                ],
            )
            .flexible_column(0);

            for row in &rows {
                // A bus factor of one is the number worth colouring: exactly
                // one person holds the majority of that file's history.
                let bus = if row.bus_factor == 1 {
                    palette.paint(Style::Red, "1")
                } else {
                    row.bus_factor.to_string()
                };

                let seen = date::humanise_days(row.owner_silent_days);
                let seen = if row.orphaned {
                    palette.paint(Style::Yellow, &seen)
                } else {
                    seen
                };

                table.push(vec![
                    row.path.clone(),
                    row.revisions.to_string(),
                    row.authors.to_string(),
                    row.main_author.clone(),
                    format!("{:.0}%", row.main_share * 100.0),
                    bus,
                    seen,
                ]);
            }

            let orphaned = rows.iter().filter(|r| r.orphaned).count();
            format!(
                "{}\n{}",
                table.render(palette),
                palette.paint(
                    Style::Dim,
                    &format!(
                        "bus = people holding the majority of a file's history · \
                         {orphaned} of {} shown are owned by someone quiet for {}+ days\n",
                        rows.len(),
                        args.dormant_days
                    )
                )
            )
        }
    }
}

// ----------------------------------------------------------------- coupling

fn render_coupling(
    walked: &History,
    current: &Snapshot,
    args: &Args,
    palette: &Palette,
) -> String {
    let rows = reports::coupling(walked, current, args.min_co_changes, args.top);

    match args.format {
        Format::Json => json::to_string(&json::Value::Array(
            rows.iter()
                .map(|row| {
                    json::Value::object(vec![
                        ("a", row.a.clone().into()),
                        ("b", row.b.clone().into()),
                        ("co_changes", row.co_changes.into()),
                        ("revisions_a", row.revisions_a.into()),
                        ("revisions_b", row.revisions_b.into()),
                        ("degree", row.degree.into()),
                        ("crosses_directories", row.crosses_directories.into()),
                    ])
                })
                .collect(),
        )),

        Format::Csv => {
            let mut writer = csv::Writer::new(&[
                "a",
                "b",
                "co_changes",
                "revisions_a",
                "revisions_b",
                "degree",
                "crosses_directories",
            ]);
            for row in &rows {
                writer.row(&[
                    row.a.clone(),
                    row.b.clone(),
                    row.co_changes.to_string(),
                    row.revisions_a.to_string(),
                    row.revisions_b.to_string(),
                    format!("{:.4}", row.degree),
                    row.crosses_directories.to_string(),
                ]);
            }
            writer.finish()
        }

        Format::Table => {
            if rows.is_empty() {
                return format!(
                    "No file pairs changed together at least {} times. Lower --min-co to widen it.\n",
                    args.min_co_changes
                );
            }

            let mut table = Table::new(
                &["file", "changes with", "together", "degree", ""],
                &[Align::Left, Align::Left, Align::Right, Align::Right, Align::Left],
            )
            .flexible_column(0);

            for row in &rows {
                table.push(vec![
                    row.a.clone(),
                    row.b.clone(),
                    row.co_changes.to_string(),
                    palette.severity(&format!("{:.0}%", row.degree * 100.0), row.degree),
                    if row.crosses_directories {
                        palette.paint(Style::Cyan, "crosses dirs")
                    } else {
                        String::new()
                    },
                ]);
            }

            format!(
                "{}\n{}",
                table.render(palette),
                palette.paint(
                    Style::Dim,
                    "degree = commits touching both, over commits touching either\n"
                )
            )
        }
    }
}

// ---------------------------------------------------------------------- age

fn render_age(
    walked: &History,
    current: &Snapshot,
    args: &Args,
    now: i64,
    palette: &Palette,
) -> String {
    let rows = reports::age(walked, current, now, args.top);

    match args.format {
        Format::Json => json::to_string(&json::Value::Array(
            rows.iter()
                .map(|row| {
                    json::Value::object(vec![
                        ("path", row.path.clone().into()),
                        ("days_since_change", row.days_since_change.into()),
                        ("revisions", row.revisions.into()),
                    ])
                })
                .collect(),
        )),

        Format::Csv => {
            let mut writer = csv::Writer::new(&["path", "days_since_change", "revisions"]);
            for row in &rows {
                writer.row(&[
                    row.path.clone(),
                    row.days_since_change.to_string(),
                    row.revisions.to_string(),
                ]);
            }
            writer.finish()
        }

        Format::Table => {
            if rows.is_empty() {
                return "No files to age.\n".to_string();
            }

            let mut table = Table::new(
                &["file", "untouched for", "revs"],
                &[Align::Left, Align::Right, Align::Right],
            )
            .flexible_column(0);

            for row in &rows {
                table.push(vec![
                    row.path.clone(),
                    date::humanise_days(row.days_since_change),
                    row.revisions.to_string(),
                ]);
            }

            format!(
                "{}\n{}",
                table.render(palette),
                palette.paint(
                    Style::Dim,
                    "stable is not the same as abandoned; this only tells you which is which\n"
                )
            )
        }
    }
}

// ------------------------------------------------------------------ helpers

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("{count} {one}")
    } else {
        format!("{count} {many}")
    }
}

fn percent(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "0%".to_string();
    }
    format!("{:.0}%", part as f64 / whole as f64 * 100.0)
}
