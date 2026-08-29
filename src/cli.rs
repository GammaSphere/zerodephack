//! Command-line parsing.
//!
//! Replaces `clap`. Subcommands, long and short flags, `--flag value` and
//! `--flag=value`, `--` to stop option parsing, and a help screen. About two
//! hundred lines, against a dependency tree that is usually the largest thing
//! in a small Rust binary.
//!
//! Exit codes follow the usual convention: 0 success, 1 a real failure, 2 the
//! command line was wrong. Scripts can tell "this repository has a problem"
//! from "you typed it wrong", which is the whole point of having two.

use std::path::PathBuf;

use crate::util::date;

pub const USAGE_EXIT_CODE: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Summary,
    Hotspots,
    Owners,
    Coupling,
    Age,
}

impl Command {
    fn parse(name: &str) -> Option<Command> {
        match name {
            "summary" => Some(Command::Summary),
            "hotspots" => Some(Command::Hotspots),
            "owners" => Some(Command::Owners),
            "coupling" => Some(Command::Coupling),
            "age" => Some(Command::Age),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Command::Summary => "summary",
            Command::Hotspots => "hotspots",
            Command::Owners => "owners",
            Command::Coupling => "coupling",
            Command::Age => "age",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Table,
    Json,
    Csv,
}

#[derive(Debug, Clone)]
pub struct Args {
    pub command: Command,
    pub repository: PathBuf,
    pub top: usize,
    pub since: Option<i64>,
    pub path_filter: Option<String>,
    pub format: Format,
    pub no_color: bool,
    pub include_merges: bool,
    pub min_co_changes: usize,
    pub dormant_days: i64,
    pub verify: bool,
}

impl Default for Args {
    fn default() -> Args {
        Args {
            command: Command::Summary,
            repository: PathBuf::from("."),
            top: 20,
            since: None,
            path_filter: None,
            format: Format::Table,
            no_color: false,
            include_merges: false,
            min_co_changes: 3,
            dormant_days: 180,
            verify: false,
        }
    }
}

/// What the command line asked for.
pub enum Parsed {
    Run(Box<Args>),
    Help,
    Version,
}

/// Parse arguments, excluding the program name.
pub fn parse<I>(argv: I, now: i64) -> Result<Parsed, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = Args::default();
    let mut command_seen = false;
    let mut positional: Option<PathBuf> = None;
    let mut only_positional = false;

    let mut items = argv.into_iter().peekable();

    while let Some(item) = items.next() {
        if only_positional {
            positional = Some(PathBuf::from(item));
            continue;
        }

        // `--flag=value` is split before dispatch so both spellings land in the
        // same place.
        let (flag, inline) = match item.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => {
                (flag.to_string(), Some(value.to_string()))
            }
            _ => (item.clone(), None),
        };

        // Fetch a flag's value from `--flag=value` or the next argument.
        let mut value = |flag: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| items.next())
                .ok_or_else(|| format!("{flag} needs a value"))
        };

        match flag.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "--" => only_positional = true,

            "--json" => args.format = Format::Json,
            "--csv" => args.format = Format::Csv,
            "--no-color" | "--no-colour" => args.no_color = true,
            "--include-merges" => args.include_merges = true,
            "--verify" => args.verify = true,

            "--top" | "-n" => {
                let raw = value(&flag)?;
                args.top = raw
                    .parse()
                    .map_err(|_| format!("--top wants a whole number, got {raw:?}"))?;
                if args.top == 0 {
                    return Err("--top must be at least 1".to_string());
                }
            }

            "--since" => {
                let raw = value(&flag)?;
                args.since = Some(date::parse_since(&raw, now).ok_or_else(|| {
                    format!(
                        "--since wants a date like 2024-08-29 or an offset like 30d, got {raw:?}"
                    )
                })?);
            }

            "--path" => args.path_filter = Some(value(&flag)?),

            "--min-co" => {
                let raw = value(&flag)?;
                args.min_co_changes = raw
                    .parse()
                    .map_err(|_| format!("--min-co wants a whole number, got {raw:?}"))?;
            }

            "--dormant-days" => {
                let raw = value(&flag)?;
                args.dormant_days = raw
                    .parse()
                    .map_err(|_| format!("--dormant-days wants a whole number, got {raw:?}"))?;
            }

            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unknown option {other}"));
            }

            // The first bare word is the command, the second is the path.
            word => {
                if !command_seen {
                    match Command::parse(word) {
                        Some(command) => {
                            args.command = command;
                            command_seen = true;
                            continue;
                        }
                        None => {
                            // Allow `strata some/path` with no command at all.
                            if positional.is_none() {
                                positional = Some(PathBuf::from(word));
                                continue;
                            }
                            return Err(format!(
                                "unknown command {word:?}; expected one of summary, hotspots, owners, coupling, age"
                            ));
                        }
                    }
                }

                if positional.is_some() {
                    return Err(format!("unexpected extra argument {word:?}"));
                }
                positional = Some(PathBuf::from(word));
            }
        }
    }

    if let Some(path) = positional {
        args.repository = path;
    }

    Ok(Parsed::Run(Box::new(args)))
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn help() -> String {
    format!(
        "\
strata {VERSION} - git repository archaeology with zero dependencies

USAGE
    strata [COMMAND] [OPTIONS] [PATH]

COMMANDS
    summary     Headline numbers for the repository (default)
    hotspots    Files that change often and are large
    owners      Code ownership, bus factor, and files whose owner has gone
    coupling    Files that keep changing together
    age         How long each file has been left alone

OPTIONS
    -n, --top <N>            Rows to show [default: 20]
        --since <WHEN>       Only commits since a date (2024-08-29) or an
                             offset (30d, 6w, 3m, 1y)
        --path <GLOB>        Only paths matching a glob (*.rs, src/**/mod.rs)
        --min-co <N>         Coupling: least co-changes to report [default: 3]
        --dormant-days <N>   Owners: silence before an owner counts as gone
                             [default: 180]
        --include-merges     Count merge commits, which are skipped by default
        --json               Emit JSON
        --csv                Emit CSV
        --no-color           Never colour output (NO_COLOR is also honoured)
        --verify             Check every object's SHA-1 while reading
    -h, --help               Show this help
    -V, --version            Show the version

EXIT CODES
    0   success
    1   the repository could not be read
    2   the command line was wrong

EXAMPLES
    strata                             Summary of the repository you are in
    strata hotspots --top 10           The ten files most worth looking at
    strata owners --since 1y           Ownership over the last year only
    strata coupling --path 'src/**'    Coupling inside src, ignoring the rest
    strata hotspots --json > out.json  Machine-readable, no colour
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(words: &[&str]) -> Result<Args, String> {
        let argv = words.iter().map(|w| w.to_string());
        match parse(argv, 1_724_889_600)? {
            Parsed::Run(args) => Ok(*args),
            Parsed::Help => Err("help".to_string()),
            Parsed::Version => Err("version".to_string()),
        }
    }

    #[test]
    fn defaults_to_a_summary_of_the_current_directory() {
        let args = parse_args(&[]).unwrap();
        assert_eq!(args.command, Command::Summary);
        assert_eq!(args.repository, PathBuf::from("."));
        assert_eq!(args.top, 20);
        assert_eq!(args.format, Format::Table);
    }

    #[test]
    fn takes_a_command_and_a_path() {
        let args = parse_args(&["hotspots", "/some/repo"]).unwrap();
        assert_eq!(args.command, Command::Hotspots);
        assert_eq!(args.repository, PathBuf::from("/some/repo"));
    }

    #[test]
    fn a_lone_path_needs_no_command() {
        let args = parse_args(&["/some/repo"]).unwrap();
        assert_eq!(args.command, Command::Summary);
        assert_eq!(args.repository, PathBuf::from("/some/repo"));
    }

    #[test]
    fn accepts_both_flag_spellings() {
        assert_eq!(parse_args(&["--top", "5"]).unwrap().top, 5);
        assert_eq!(parse_args(&["--top=5"]).unwrap().top, 5);
        assert_eq!(parse_args(&["-n", "5"]).unwrap().top, 5);
    }

    #[test]
    fn parses_since_in_both_forms() {
        let absolute = parse_args(&["--since", "2024-08-29"]).unwrap();
        assert_eq!(absolute.since, Some(1_724_889_600));

        let relative = parse_args(&["--since", "30d"]).unwrap();
        assert_eq!(relative.since, Some(1_724_889_600 - 30 * 86_400));
    }

    #[test]
    fn rejects_bad_values_with_a_useful_message() {
        let err = parse_args(&["--top", "many"]).unwrap_err();
        assert!(err.contains("whole number"), "{err}");

        let err = parse_args(&["--since", "yesterday"]).unwrap_err();
        assert!(err.contains("2024-08-29"), "{err}");

        let err = parse_args(&["--top"]).unwrap_err();
        assert!(err.contains("needs a value"), "{err}");

        assert_eq!(
            parse_args(&["--top", "0"]).unwrap_err(),
            "--top must be at least 1"
        );
    }

    #[test]
    fn rejects_unknown_options_and_commands() {
        assert!(
            parse_args(&["--nope"])
                .unwrap_err()
                .contains("unknown option")
        );
        assert!(
            parse_args(&["hotspots", "a", "b"])
                .unwrap_err()
                .contains("unexpected extra argument")
        );
    }

    #[test]
    fn double_dash_stops_option_parsing() {
        // A repository directory that begins with a dash is unusual but legal.
        let args = parse_args(&["hotspots", "--", "--weird-dir"]).unwrap();
        assert_eq!(args.repository, PathBuf::from("--weird-dir"));
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert_eq!(parse_args(&["--help"]).unwrap_err(), "help");
        assert_eq!(parse_args(&["-h"]).unwrap_err(), "help");
        assert_eq!(parse_args(&["-V"]).unwrap_err(), "version");
        // Even alongside otherwise invalid input.
        assert_eq!(parse_args(&["--help", "--nope"]).unwrap_err(), "help");
    }

    #[test]
    fn collects_the_remaining_switches() {
        let args = parse_args(&[
            "coupling",
            "--json",
            "--no-color",
            "--include-merges",
            "--verify",
            "--min-co=7",
            "--dormant-days=90",
            "--path",
            "src/**",
        ])
        .unwrap();

        assert_eq!(args.format, Format::Json);
        assert!(args.no_color);
        assert!(args.include_merges);
        assert!(args.verify);
        assert_eq!(args.min_co_changes, 7);
        assert_eq!(args.dormant_days, 90);
        assert_eq!(args.path_filter.as_deref(), Some("src/**"));
    }
}
