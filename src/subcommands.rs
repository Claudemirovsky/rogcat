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
    cli::{ClearOpts, CliArguments, LogOpts, SubCommands},
    profiles::profiles_list,
    reader::stdin,
    utils::{self, adb},
    StreamData, DEFAULT_BUFFER,
};
use clap::{crate_name, CommandFactory};
use clap_complete::{generate, Generator};
use failure::Error;
use futures::{
    future::ready,
    sink::Sink,
    stream::StreamExt,
    task::{Context, Poll},
};
use rogcat::record::Level;
use std::{
    borrow::ToOwned,
    path::PathBuf,
    pin::Pin,
    process::{exit, Stdio},
};
use tabled::{
    builder::Builder,
    settings::{object::Rows, Alignment, Style, Width},
};

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
use tokio_stream::wrappers::LinesStream;

pub async fn parse_subcommand(command: SubCommands) {
    match command {
        SubCommands::Clear(opts) => clear(opts).await,
        SubCommands::Completions(opts) => completions(opts.shell).await,
        SubCommands::Devices => devices().await,
        SubCommands::Log(opts) => log(opts).await.unwrap(),
        SubCommands::Profiles(opts) => profiles(opts.profiles_path).unwrap(),
    }
}

pub async fn completions<T: Generator>(shell: T) {
    let mut cmd = CliArguments::command();
    generate(shell, &mut cmd, crate_name!(), &mut std::io::stdout());
    exit(0);
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
    let level = Level::from(args.level);
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

pub fn profiles(path: Option<PathBuf>) -> Result<(), Error> {
    let list = profiles_list(path.as_ref())?;
    if list.is_empty() {
        let profiles_path = utils::config_dir().join("profiles.toml");
        eprintln!("No profiles found! Check your profiles file ({profiles_path:?}) or set a ROGCAT_PROFILES environment variable.");
        exit(0)
    }

    // Table header
    let mut items = vec![vec![String::from("PROFILE NAME"), String::from("COMMENT")]];
    let mut values = list
        .iter()
        .map(|(name, profile)| {
            let comment = match profile.comment.as_ref() {
                Some(s) => s.as_str(),
                None => "No comment",
            };
            vec![name.to_string(), comment.to_string()]
        })
        .collect::<Vec<Vec<String>>>();
    values.sort_by(|a, b| a.first().partial_cmp(&b.first()).unwrap());

    items.append(&mut values);

    let mut table = Builder::from(items).build();
    table
        .with(Style::modern_rounded())
        .with(Alignment::center())
        .modify(Rows::new(1..), Width::wrap(50).keep_words());

    println!("{table}");
    Ok(())
}
