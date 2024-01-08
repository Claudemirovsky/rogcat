use std::path::PathBuf;

use crate::utils;
use clap::{crate_authors, Args, Parser, Subcommand, ValueHint};
use clap_complete::Shell;
use lazy_static::lazy_static;
use rogcat::record::{Format, Level};

lazy_static! {
    static ref ABOUT: String = {
        format!(
            "A 'adb logcat' wrapper and log processor. Your config directory is \"{}\".",
            utils::config_dir().display()
        )
    };
}

#[derive(Parser, Clone)]
#[clap(author = crate_authors!(), version, about = ABOUT.as_str())]
pub(crate) struct CliArguments {
    /// Select specific logd buffers. Defaults to main, events, kernel and crash.
    #[clap(long, long, conflicts_with_all = &["input", "COMMAND"])]
    pub(crate) buffer: Option<Vec<String>>,

    // Terminal coloring option
    #[clap(long, conflicts_with_all = &["highlight", "output"], value_parser = ["always", "auto", "never"])]
    pub(crate) color: Option<String>,

    /// Dump the log and then exit (don't block)
    #[clap(long, short, conflicts_with_all = &["input", "COMMAND", "restart"])]
    pub(crate) dump: bool,

    /// Output format. Defaults to human on stdout and raw on file output
    #[clap(long, short, value_enum)]
    pub(crate) format: Option<Format>,

    /// Select a format for output file names.
    /// By passing 'single' the filename provided with the '-o' option is used (default).
    /// 'enumerate' appends a file sequence number after the filename passed
    /// with '-o' option whenever a new file is created (see 'records-per-file' option).
    /// 'date' will prefix the output filename with the current local date when a new file is created.
    #[clap(long, short = 'a', requires = "output", value_parser = ["single", "enumerate", "date"])]
    pub(crate) filename_format: Option<String>,

    /// Read n records and exit.
    #[clap(short = 'H', long, conflicts_with_all = &["tail", "restart"])]
    pub(crate) head: Option<usize>,

    /// Highlight messages that match this pattern in RE2.
    /// The prefix '!' inverts the match.
    #[clap(short, long, conflicts_with = "output")]
    pub(crate) highlight: Vec<String>,

    /// Read from file instead of a adb command.
    /// Use 'serial://COM0@115200,8N1 or similiar for reading a serial port.
    #[clap(short, long, value_hint = ValueHint::FilePath)]
    pub(crate) input: Vec<PathBuf>,

    /// Dump the logs prior to the last reboot.
    #[clap(short = 'L', long, conflicts_with_all = &["input", "COMMAND"])]
    pub(crate) last: bool,

    /// Minimum level
    #[clap(short, long, value_parser = Level::values())]
    pub(crate) level: Option<String>,

    /// Message filters in RE2. The prefix '!' inverts the match.
    #[clap(short, long)]
    pub(crate) message: Vec<String>,

    /// Same as -m/--message but case insensitive.
    #[clap(short = 'M', long = "Message")]
    pub(crate) message_ignore_case: Vec<String>,

    /// Use white as dimm color.
    #[clap(long, conflicts_with = "output")]
    pub(crate) no_dimm: bool,

    /// Use intense colors in terminal output.
    #[clap(long, conflicts_with = "output")]
    pub(crate) bright_colors: bool,

    /// Hide timestamp in terminal output.
    #[clap(long, conflicts_with = "output")]
    pub(crate) hide_timestamp: bool,

    /// Write output to file.
    #[clap(long, short, conflicts_with = "color", value_hint = ValueHint::FilePath)]
    pub(crate) output: Option<PathBuf>,

    /// Overwrite output file if present.
    #[clap(long, requires = "output")]
    pub(crate) overwrite: bool,

    /// Filter by process ID.
    #[clap(long)]
    pub(crate) pid: Vec<String>,

    /// Filter by process names.
    #[clap(long, short = 'N')]
    pub(crate) process_name: Option<Vec<String>>,

    /// Manually specify profile file (overrules ROGCAT_PROFILES).
    #[clap(short = 'P', long, value_hint = ValueHint::FilePath)]
    pub(crate) profiles_path: Option<PathBuf>,

    /// Select profile.
    #[clap(short, long)]
    pub(crate) profile: Option<String>,

    /// Write n records per file. Use k, M, G suffixes or a plain number.
    #[clap(long, short = 'n', requires = "output")]
    pub(crate) records_per_file: Option<String>,

    /// Regex filter on tag, pid, thread and message.
    #[clap(long = "regex", short)]
    pub(crate) regex_filter: Vec<String>,

    /// Restart command on exit.
    #[clap(long, conflicts_with_all = &["dump", "input", "tail"])]
    pub(crate) restart: bool,

    /// Show month and day in terminal output.
    #[clap(long, conflicts_with = "output")]
    pub(crate) show_date: bool,

    /// Forwards the device selector to adb.
    #[clap(long = "serial", short = 's')]
    pub(crate) device: Option<String>,

    /// Tag filters in RE2. The prefix '!' inverts the match.
    #[clap(long, short)]
    pub(crate) tag: Vec<String>,

    /// Same as -t/--tag but case insensitive.
    #[clap(long = "Tag", short = 'T')]
    pub(crate) tag_ignore_case: Vec<String>,

    /// Dump only the most recent <COUNT> lines (implies --dump).
    #[clap(long, conflicts_with_all = &["input", "COMMAND", "restart"])]
    pub(crate) tail: Option<usize>,

    /// Optional command to run and capture stdout and stdderr from.
    /// Pass "-" to capture stdin. If omitted, rogcat will run
    /// "adb logcat -b all" and restarts this commmand if 'adb' terminates.
    #[clap(name = "COMMAND")]
    pub(crate) command: Option<String>,

    #[clap(subcommand)]
    pub(crate) subcommands: Option<SubCommands>,
}

#[derive(Subcommand, Clone, Debug, PartialEq)]
pub(crate) enum SubCommands {
    /// Capture bugreport. This is only works for Android versions < 7.
    #[clap(name = "bugreport")]
    BugReport(BugReportOpts),

    /// Generates completion scripts.
    #[clap(name = "completions")]
    Completions(CompletionsOpts),

    /// Clears logd buffers.
    #[clap(name = "clear")]
    Clear(ClearOpts),

    /// Lists available devices.
    #[clap(name = "devices")]
    Devices,

    /// Add log message to logcat buffer.
    Log(LogOpts),
}

#[derive(Args, Debug, Clone, PartialEq)]
pub(crate) struct BugReportOpts {
    /// Zip report.
    #[clap(long, short)]
    pub(crate) zip: bool,

    /// Overwrite report file if present.
    #[clap(long)]
    pub(crate) overwrite: bool,

    /// Output file name - defaults to <now>-bugreport.
    #[clap(name = "file", value_hint = ValueHint::FilePath)]
    pub(crate) file: Option<PathBuf>,
}

#[derive(Args, Debug, Clone, PartialEq)]
pub(crate) struct CompletionsOpts {
    /// The shell to generate the script for.
    #[clap(required = true, value_enum)]
    pub(crate) shell: Shell,
}

#[derive(Args, Debug, Clone, PartialEq)]
pub(crate) struct ClearOpts {
    /// Select specific log buffers to clear. Defaults to main, events, kernel and crash.  
    #[clap(long, short)]
    pub(crate) buffer: Option<Vec<String>>,
}

#[derive(Args, Debug, Clone, PartialEq)]
pub(crate) struct LogOpts {
    /// Log tag.
    #[clap(short, long)]
    pub(crate) tag: Option<String>,

    /// Log level.
    #[clap(short, long, value_parser = Level::values())]
    pub(crate) level: Option<String>,

    #[clap(name = "MESSAGE", required = true)]
    pub(crate) message: String,
}
