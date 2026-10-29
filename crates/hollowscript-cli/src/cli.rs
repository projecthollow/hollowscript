//! Command-line argument parsing, kept separate from [`std::env::args`] and
//! process exit so that it is plain, unit-testable Rust.

use crate::grants::{Grant, parse_grant};

/// A parsed invocation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Invocation {
    /// What to do.
    pub command: Command,
    /// `--json`: machine-readable output, one document to standard output.
    pub json: bool,
    /// Every `--allow` flag, in order, already split into a [`Grant`]. A
    /// syntax error in one is [`Command::Error`] instead.
    pub grants: Vec<Grant>,
    /// The words after the file, which are the program's own arguments
    /// (section 16.6: "not including the name of the program itself").
    pub program_args: Vec<String>,
}

/// What the invocation asks for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// `check FILE`.
    Check {
        /// The source file.
        file: String,
    },
    /// `format FILE`.
    Format {
        /// The source file.
        file: String,
    },
    /// `run FILE`.
    Run {
        /// The source file.
        file: String,
    },
    /// `build FILE [-o OUT]`: compiles to a bytecode container.
    Build {
        /// The source file.
        file: String,
        /// Where to write the container. Defaults to the source file's own
        /// name with `.hsb` in place of its extension.
        out: Option<String>,
    },
    /// `lsp`: the language server, over standard input and output.
    /// `hollowscript-lsp` is a separate binary (`hollowscript-cli` does not
    /// depend on `hollowscript-lsp`'s crate, `docs/design/architecture.md`),
    /// so this spawns it as a child process rather than running it in
    /// process; see `main.rs`'s `run_lsp`.
    Lsp,
    /// `--version` or `version`.
    Version,
    /// `help`, `--help` or `-h`, optionally naming a command
    /// (`help run`, `run --help`, `run -h`): that command's own usage, or
    /// the general usage for `None`. Printed to standard output, exit 0.
    Help(Option<&'static str>),
    /// No arguments at all: the general usage, printed to standard error
    /// with exit code 2, since nothing was asked for.
    Usage,
    /// The arguments could not be parsed; the message explains why.
    Error(String),
}

/// Parses the arguments after the program name.
#[must_use]
pub(crate) fn parse_args(args: &[String]) -> Invocation {
    let mut json = false;
    let mut grants = Vec::new();
    let mut rest = Vec::new();
    let mut out_path: Option<String> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--allow" => match iter.next() {
                Some(value) => match parse_grant(value) {
                    Ok(g) => grants.push(g),
                    Err(_) => {
                        return err(
                            json,
                            grants,
                            format!("'{value}' is not a valid --allow grant"),
                        );
                    }
                },
                None => return err(json, grants, "--allow needs a grant".to_owned()),
            },
            "-o" | "--out" => match iter.next() {
                Some(value) => out_path = Some(value.clone()),
                None => return err(json, grants, "-o needs a file".to_owned()),
            },
            _ => rest.push(arg.clone()),
        }
    }
    let first = rest.first().map(String::as_str);
    let second = rest.get(1).map(String::as_str);
    let command = match (first, second) {
        (None, _) => Command::Usage,
        (Some("help" | "--help" | "-h"), None | Some("help" | "--help" | "-h")) => {
            Command::Help(None)
        }
        (Some("help" | "--help" | "-h"), Some(topic)) | (Some(topic), Some("--help" | "-h")) => {
            match command_name(topic) {
                Some(name) => Command::Help(Some(name)),
                None => Command::Error(format!("unknown command '{topic}'; try 'help'")),
            }
        }
        (Some("version" | "--version" | "-V"), _) => Command::Version,
        (Some("lsp"), _) => Command::Lsp,
        (Some("check"), _) => file_command(
            &rest,
            Command::Check {
                file: String::new(),
            },
        ),
        (Some("format"), _) => file_command(
            &rest,
            Command::Format {
                file: String::new(),
            },
        ),
        (Some("run"), _) => file_command(
            &rest,
            Command::Run {
                file: String::new(),
            },
        ),
        (Some("build"), _) => file_command(
            &rest,
            Command::Build {
                file: String::new(),
                out: out_path.clone(),
            },
        ),
        (Some(other), _) => Command::Error(format!("unknown command '{other}'; try 'help'")),
    };
    // Everything after the command and its file belongs to the program.
    let program_args = rest.get(2..).map(<[String]>::to_vec).unwrap_or_default();
    Invocation {
        command,
        json,
        grants,
        program_args,
    }
}

fn err(json: bool, grants: Vec<Grant>, message: String) -> Invocation {
    Invocation {
        command: Command::Error(message),
        json,
        grants,
        program_args: Vec::new(),
    }
}

/// The commands `help` knows, as the static name [`Command::Help`] holds.
const COMMANDS: [&str; 7] = ["check", "format", "run", "build", "lsp", "version", "help"];

fn command_name(word: &str) -> Option<&'static str> {
    COMMANDS.iter().copied().find(|name| *name == word)
}

fn file_command(rest: &[String], template: Command) -> Command {
    let name = rest.first().map_or("", String::as_str);
    match rest.get(1) {
        None => Command::Error(format!("'{name}' needs a file; try '{name} --help'")),
        // A word that looks like an option where the file should be is
        // almost certainly a mistyped option, not a file called `-x`.
        Some(file) if file.starts_with('-') && file.len() > 1 => Command::Error(format!(
            "'{name}' does not take the option '{file}'; try '{name} --help'"
        )),
        Some(file) => match template {
            Command::Check { .. } => Command::Check { file: file.clone() },
            Command::Format { .. } => Command::Format { file: file.clone() },
            Command::Run { .. } => Command::Run { file: file.clone() },
            Command::Build { out, .. } => Command::Build {
                file: file.clone(),
                out,
            },
            other => other,
        },
    }
}

/// The usage text for `help`, or for one command with `help COMMAND` or
/// `COMMAND --help`.
pub(crate) fn usage(command: Option<&str>) -> &'static str {
    match command {
        Some("check") => include_str!("help/check.txt"),
        Some("format") => include_str!("help/format.txt"),
        Some("run") => include_str!("help/run.txt"),
        Some("build") => include_str!("help/build.txt"),
        Some("lsp") => include_str!("help/lsp.txt"),
        _ => include_str!("usage.txt"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, parse_args};

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn no_arguments_is_usage() {
        assert_eq!(parse_args(&[]).command, Command::Usage);
    }

    #[test]
    fn help_can_name_a_command_either_way_round() {
        for words in [&["help"][..], &["--help"][..], &["-h"][..]] {
            assert_eq!(parse_args(&args(words)).command, Command::Help(None));
        }
        for command in ["check", "format", "run", "build", "lsp"] {
            for words in [
                [command, "--help"],
                [command, "-h"],
                ["help", command],
                ["--help", command],
            ] {
                assert_eq!(
                    parse_args(&args(&words)).command,
                    Command::Help(Some(command)),
                    "{words:?}"
                );
            }
        }
        assert!(matches!(
            parse_args(&args(&["help", "frobnicate"])).command,
            Command::Error(_)
        ));
    }

    /// `--help` after the file belongs to the program, like every other
    /// word there.
    #[test]
    fn help_after_the_file_is_the_programs_argument() {
        let inv = parse_args(&args(&["run", "main.hws", "--help"]));
        assert_eq!(
            inv.command,
            Command::Run {
                file: "main.hws".to_owned()
            }
        );
        assert_eq!(inv.program_args, ["--help"]);
    }

    #[test]
    fn an_option_where_the_file_goes_is_an_error_not_a_file_name() {
        for command in ["check", "format", "run", "build"] {
            assert!(
                matches!(
                    parse_args(&args(&[command, "--frobnicate"])).command,
                    Command::Error(_)
                ),
                "{command}"
            );
        }
    }

    #[test]
    fn repl_is_not_a_command() {
        assert!(matches!(
            parse_args(&args(&["repl"])).command,
            Command::Error(_)
        ));
    }

    #[test]
    fn lsp_takes_no_file() {
        assert_eq!(parse_args(&args(&["lsp"])).command, Command::Lsp);
    }

    #[test]
    fn check_needs_a_file() {
        let inv = parse_args(&args(&["check"]));
        assert!(matches!(inv.command, Command::Error(_)));
        let inv = parse_args(&args(&["check", "main.hws"]));
        assert_eq!(
            inv.command,
            Command::Check {
                file: "main.hws".to_owned()
            }
        );
    }

    #[test]
    fn json_and_allow_are_parsed_regardless_of_position() {
        let inv = parse_args(&args(&[
            "--json",
            "run",
            "--allow",
            "fs:read=./data",
            "main.hws",
        ]));
        assert!(inv.json);
        assert_eq!(inv.grants.len(), 1);
        assert_eq!(inv.grants[0].name, "fs");
        assert_eq!(
            inv.command,
            Command::Run {
                file: "main.hws".to_owned()
            }
        );
    }

    #[test]
    fn a_bad_grant_is_an_error() {
        let inv = parse_args(&args(&["check", "--allow", "", "main.hws"]));
        assert!(matches!(inv.command, Command::Error(_)));
    }

    #[test]
    fn unknown_commands_are_errors_not_panics() {
        assert!(matches!(
            parse_args(&args(&["frobnicate"])).command,
            Command::Error(_)
        ));
    }

    /// `build` takes a file, and `-o` says where the container goes.
    #[test]
    fn build_parses_its_file_and_output() {
        let inv = parse_args(&args(&["build", "main.hws"]));
        assert!(matches!(
            inv.command,
            Command::Build { ref file, out: None } if file == "main.hws"
        ));
        let inv = parse_args(&args(&["build", "main.hws", "-o", "game.hsb"]));
        assert!(matches!(
            inv.command,
            Command::Build { ref file, out: Some(ref out) }
                if file == "main.hws" && out == "game.hsb"
        ));
        let inv = parse_args(&args(&["build"]));
        assert!(matches!(inv.command, Command::Error(_)));
        let inv = parse_args(&args(&["build", "main.hws", "-o"]));
        assert!(matches!(inv.command, Command::Error(_)));
    }
}
