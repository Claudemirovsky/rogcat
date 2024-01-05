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
    cli::cli,
    reader::stdin,
    utils::{self, adb},
    StreamData, DEFAULT_BUFFER,
};
use clap::{crate_name, value_t, ArgMatches};
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

pub async fn run(args: &ArgMatches<'_>) {
    match args.subcommand() {
        ("bugreport", Some(sub_matches)) => bugreport(sub_matches).await,
        ("clear", Some(sub_matches)) => clear(sub_matches).await,
        ("completions", Some(sub_matches)) => completions(sub_matches).await,
        ("devices", _) => devices().await,
        ("log", Some(sub_matches)) => log(sub_matches).await.unwrap(),
        (_, _) => (),
    }
}

pub async fn completions(args: &ArgMatches<'_>) {
    if let Err(e) = args
        .value_of("shell")
        .ok_or_else(|| err_msg("Required shell argument is missing"))
        .map(str::parse)
        .map(|s| {
            cli().gen_completions_to(crate_name!(), s.unwrap(), &mut std::io::stdout());
        })
    {
        eprintln!("Failed to get shell argument: {e}");
        exit(1);
    } else {
        exit(0);
    }
}

struct ZipFile {
    zip: ZipWriter<File>,
}

impl ZipFile {
    fn create(filename: &str) -> Result<Self, Error> {
        let file = File::create(format!("{filename}.zip"))?;
        let options = FileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o644);
        let filename_path = PathBuf::from(&filename);
        let f = filename_path
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

fn report_filename() -> Result<String, Error> {
    #[cfg(not(windows))]
    let sep = ":";
    #[cfg(windows)]
    let sep = "_";

    let format = format!("[month]-[day]_[hour]{sep}[minute]{sep}[second]-bugreport.txt");
    let desc = format_description::parse_borrowed::<2>(format.as_str())?;
    let now = OffsetDateTime::now_local()?;
    now.format(&desc).map_err(|x| x.into())
}

/// Performs a dumpstate and write to fs. Note: The Android 7+ dumpstate is not supported.
pub async fn bugreport(args: &ArgMatches<'_>) {
    let filename = value_t!(args.value_of("file"), String)
        .unwrap_or_else(|_| report_filename().expect("Failed to generate filename"));
    let filename_path = PathBuf::from(&filename);
    if !args.is_present("overwrite") && filename_path.exists() {
        eprintln!("File {filename} already exists");
        exit(1);
    }
    let mut adb = adb().expect("Failed to find adb");

    if args.is_present("dev") {
        let device = value_t!(args, "dev", String).unwrap_or_else(|e| e.exit());
        adb.push::<String>("-s".into());
        adb.push(device);
    }

    let child = Command::new(adb)
        .arg("bugreport")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to launch adb");
    let stdout = BufReader::new(child.stdout.unwrap());

    let dir = filename_path.parent().unwrap_or_else(|| Path::new(""));
    if !dir.is_dir() {
        DirBuilder::new()
            .recursive(true)
            .create(dir)
            .expect("Failed to create outfile parent directory");
    }

    let progress = ProgressBar::new(::std::u64::MAX);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.yellow} {msg:.dim.bold} {pos:>7.dim} {elapsed_precise:.dim}")
            .progress_chars(" • "),
    );
    progress.set_message("Connecting");

    let mut write = if args.is_present("zip") {
        Box::new(ZipFile::create(&filename).expect("Failed to create zip file")) as Box<dyn Write>
    } else {
        Box::new(File::create(&filename).expect("Failed to craete file")) as Box<dyn Write>
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
            progress.set_style(ProgressStyle::default_bar().template("{msg:.dim.bold}"));
            progress.finish_with_message(&format!("Finished {}.", filename_path.display()));
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
pub async fn log(args: &ArgMatches<'_>) -> Result<(), Error> {
    let message = args.value_of("MESSAGE").unwrap_or("");
    let tag = args.value_of("tag").unwrap_or("Rogcat").to_owned();
    let level = Level::from(args.value_of("level").unwrap_or(""));
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
                .await
                .unwrap();
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
pub async fn clear(args: &ArgMatches<'_>) {
    let buffer = args
        .values_of("buffer")
        .map(|m| m.map(ToOwned::to_owned).collect::<Vec<String>>())
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
