// Copyright © 2017 Felix Obenhuber
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use crate::{
    cli::{BugReportOpts, ClearOpts, CliArguments, LogOpts, SubCommands},
    reader::stdin,
    utils::{self, adb},
    StreamData, DEFAULT_BUFFER,
};
use clap::{crate_name, CommandFactory};
use clap_complete::{generate, Generator};
use failure::{err_msg, Error};
use futures::{
    future::ready,
    sink::Sink,
    stream::StreamExt,
    task::{Context, Poll},
    TryStreamExt,
};
use indicatif::{ProgressBar, ProgressStyle};
use rogcat::record::Level;
use std::{
    borrow::ToOwned,
    fs::{DirBuilder, File},
    io::Write,
    path::{Path, PathBuf},
    pin::Pin,
    process::{exit, Stdio},
};
use time::{format_description, OffsetDateTime};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
use tokio_stream::wrappers::LinesStream;
use zip::{write::FileOptions, CompressionMethod, ZipWriter};

pub async fn run(args: &CliArguments) {
    match &args.subcommands {
        Some(SubCommands::BugReport(opts)) => {
            bugreport(opts.to_owned(), args.device.to_owned()).await
        }
        Some(SubCommands::Clear(opts)) => clear(opts.to_owned()).await,
        Some(SubCommands::Completions(opts)) => completions(opts.shell).await,
        Some(SubCommands::Devices) => devices().await,
        Some(SubCommands::Log(opts)) => log(opts.to_owned()).await.unwrap(),
        None => (),
    }
}

pub async fn completions<T: Generator>(shell: T) {
    let mut cmd = CliArguments::command();
    generate(shell, &mut cmd, crate_name!(), &mut std::io::stdout());
    exit(0);
}

struct ZipFile {
    zip: ZipWriter<File>,
}

impl ZipFile {
    fn create(filename: &PathBuf) -> Result<Self, Error> {
        let mut name = filename.to_owned().into_os_string();
        name.push(".zip");
        let path: PathBuf = name.into();
        let file = File::create(path)?;
        let options = FileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o644);
        let f = filename
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| err_msg("Failed to get filename"))?;
        let mut zip = ZipWriter::new(file);
        zip.start_file(f, options)?;
        Ok(ZipFile { zip })
    }
}

impl Write for ZipFile {
    fn write(&mut self, buf: &[u8]) -> ::std::io::Result<usize> {
        self.zip.write_all(buf).map(|_| buf.len())
    }

    fn flush(&mut self) -> ::std::io::Result<()> {
        self.zip
            .finish()
            .map_err(std::convert::Into::into)
            .map(|_| ())
    }
}

impl Drop for ZipFile {
    fn drop(&mut self) {
        self.flush().expect("Failed to close zipfile");
    }
}

fn report_file() -> Result<PathBuf, Error> {
    #[cfg(not(windows))]
    let sep = ":";
    #[cfg(windows)]
    let sep = "_";

    let format = format!("[month]-[day]_[hour]{sep}[minute]{sep}[second]-bugreport.txt");
    let desc = format_description::parse_borrowed::<2>(format.as_str())?;
    let now = OffsetDateTime::now_local()?;
    now.format(&desc).map_err(|x| x.into()).map(PathBuf::from)
}

/// Performs a dumpstate and write to fs. Note: The Android 7+ dumpstate is not supported.
pub async fn bugreport(opts: BugReportOpts, device: Option<String>) {
    let file_path = opts
        .file
        .unwrap_or_else(|| report_file().expect("Failed to generate filename"));

    if !opts.overwrite && file_path.exists() {
        eprintln!("File {} already exists", file_path.display());
        exit(1);
    }
    let mut adb = adb().expect("Failed to find adb");

    if let Some(device) = device {
        adb.push("-s");
        adb.push(device);
    }

    let child = Command::new(adb)
        .arg("bugreport")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to launch adb");
    let stdout = BufReader::new(child.stdout.unwrap());

    let dir = file_path.parent().unwrap_or_else(|| Path::new(""));
    if !dir.is_dir() {
        DirBuilder::new()
            .recursive(true)
            .create(dir)
            .expect("Failed to create outfile parent directory");
    }

    let progress = ProgressBar::new(::std::u64::MAX);
    if let Ok(style) = ProgressStyle::default_bar()
        .template("{spinner:.yellow} {msg:.dim.bold} {pos:>7.dim} {elapsed_precise:.dim}")
    {
        progress.set_style(style.progress_chars(" • "))
    }
    progress.set_message("Connecting");

    let mut write = if opts.zip {
        Box::new(ZipFile::create(&file_path).expect("Failed to create zip file")) as Box<dyn Write>
    } else {
        Box::new(File::create(&file_path).expect("Failed to craete file")) as Box<dyn Write>
    };

    progress.set_message("Pulling bugreport line");

    // TODO: Migrate to tokio::fs::File
    let output = LinesStream::new(stdout.lines()).try_for_each(|line| {
        write.write_all(line.as_bytes()).expect("Failed to write");
        write.write_all(b"\n").expect("Failed to write");
        progress.inc(1);
        ready(Ok(()))
    });

    match output.await {
        Ok(_) => {
            if let Ok(style) = ProgressStyle::default_bar().template("{msg:.dim.bold}") {
                progress.set_style(style);
            }
            progress.finish_with_message(format!("Finished {}.", file_path.display()));
            exit(0);
        }
        Err(e) => {
            eprintln!("Failed to create bugreport: {e}");
            exit(1);
        }
    }
}

pub async fn devices() {
    let child = Command::new(adb().expect("Failed to find adb"))
        .arg("devices")
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to run adb devices");

    let lines = BufReader::new(child.stdout.unwrap()).lines();
    let result = LinesStream::new(lines)
        .skip(1)
        .filter_map(|x| async move {
            match x {
                Ok(line) if line.is_empty() => None,
                Ok(line) if line.starts_with("* daemon") => None,
                Ok(line) => Some(line),
                _ => None,
            }
        })
        .for_each(|line| {
            let mut split = line.split_whitespace();
            let id = split.next().unwrap_or("unknown");
            let name = split.next().unwrap_or("unknown");
            println!("{id} {name}");
            ready(())
        });

    result.await;
    exit(0);
}

struct Logger {
    tag: String,
    level: Level,
}

impl Logger {
    fn level(level: &Level) -> &str {
        match *level {
            Level::Trace | Level::Verbose => "v",
            Level::Debug | Level::None => "d",
            Level::Info => "i",
            Level::Warn => "w",
            Level::Error | Level::Fatal | Level::Assert => "e",
        }
    }
}

impl Sink<String> for Logger {
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: String) -> Result<(), Self::Error> {
        let child = Command::new(adb()?)
            .arg("shell")
            .arg("log")
            .arg("-p")
            .arg(Self::level(&self.level))
            .arg("-t")
            .arg(format!("\"{}\"", &self.tag))
            .arg(&item)
            .stdout(Stdio::piped())
            .output();
        tokio::spawn(child);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

/// Call something like adb shell log <message>
pub async fn log(args: LogOpts) -> Result<(), Error> {
    let message = args.message.as_str();
    let tag = args.tag.unwrap_or("Rogcat".to_string());
    let level = Level::from(args.level.unwrap_or("".to_string()).as_str());
    match message {
        "-" => {
            let sink = Logger { tag, level };
            let stdin = Box::into_pin(stdin());
            stdin
                .map(|d| match d {
                    StreamData::Line(l) => l,
                    _ => panic!("Received non line item during log"),
                })
                .map(Ok)
                .forward(sink)
                .await?;
        }
        _ => {
            Command::new(adb().expect("Failed to find adb"))
                .arg("shell")
                .arg("log")
                .arg("-p")
                .arg(Logger::level(&level))
                .arg("-t")
                .arg(&tag)
                .arg(format!("\"{message}\""))
                .stdout(Stdio::piped())
                .output()
                .await?;
        }
    }

    exit(0);
}

/// Call adb logcat -c -b BUFFERS
pub async fn clear(args: ClearOpts) {
    let buffer = args
        .buffer
        .or_else(|| utils::config_get("buffer"))
        .unwrap_or_else(|| DEFAULT_BUFFER.iter().map(|&s| s.to_owned()).collect())
        .join(" -b ");

    let mut child = Command::new(adb().expect("Failed to find adb"))
        .arg("logcat")
        .arg("-c")
        .arg("-b")
        .args(buffer.split(' '))
        .spawn()
        .expect("Failed to run adb");

    exit(
        child
            .wait()
            .await
            .expect("Failed to run")
            .code()
            .unwrap_or(1),
    );
}
