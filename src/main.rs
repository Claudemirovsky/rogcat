// Copyright © 2016 Felix Obenhuber
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

use clap::Parser;
use failure::Error;
use futures::{future::ready, Sink, Stream, StreamExt};
use rogcat::{parser, record::Record};
use std::process::exit;
use url::Url;

mod cli;
mod filewriter;
mod filter;
mod lossy_lines;
mod profiles;
mod reader;
mod subcommands;
mod terminal;
mod utils;

const DEFAULT_BUFFER: [&str; 4] = ["main", "events", "crash", "kernel"];

#[derive(Debug, Clone)]
pub enum StreamData {
    Record(Record),
    Line(String),
}

type LogStream = Box<dyn Stream<Item = StreamData> + Send>;
type LogSink = Box<dyn Sink<Record, Error = Error> + Send>;

async fn run() -> Result<(), Error> {
    let args = cli::CliArguments::parse();
    utils::config_init();
    if let Some(subcommand) = args.subcommands {
        subcommands::parse_subcommand(subcommand, args.device).await;
        exit(0);
    }

    let source = {
        if !args.input.is_empty() {
            reader::files(args.input.clone()).await?
        } else {
            match args.command.clone() {
                Some(command) => {
                    if command == "-" {
                        reader::stdin()
                    } else if let Ok(url) = Url::parse(command.as_str()) {
                        match url.scheme() {
                            #[cfg(target_os = "linux")]
                            "can" => reader::can(url.host_str().expect("Invalid can device"))?,
                            "tcp" => reader::tcp(&url).await?,
                            "serial" => reader::serial(),
                            _ => reader::process(command, args.restart)?,
                        }
                    } else {
                        reader::process(command, args.restart)?
                    }
                }
                None => reader::logcat(&args)?,
            }
        }
    };

    let mut profile = profiles::from_args(&args)?;
    let sink = Box::into_pin(if args.output.is_some() {
        filewriter::try_from(args.clone())?
    } else {
        terminal::try_from(&args, &profile)?
    });

    // Stop process after n records if argument head is passed
    let mut head = args.head;

    let mut filter = filter::from_args_profile(args, &mut profile).await?;
    let mut parser = parser::Parser::default();

    let future = Box::into_pin(source)
        .map(move |a| match a {
            StreamData::Line(line) => parser.parse(&line),
            StreamData::Record(rec) => rec,
        })
        .filter(move |r| ready(filter.filter(r)))
        .take_while(move |_| {
            ready(match head {
                Some(0) => false,
                Some(n) => {
                    head = Some(n - 1);
                    true
                }
                None => true,
            })
        })
        .map(Ok)
        .forward(sink);

    tokio::spawn(async move { parse_result(future.await) });
    tokio::signal::ctrl_c().await.unwrap();
    Ok(())
}

#[tokio::main]
async fn main() {
    parse_result(run().await)
}

#[inline]
fn parse_result(res: Result<(), Error>) {
    match res {
        Err(e) => {
            eprintln!("{e}");
            exit(1)
        }
        Ok(_) => exit(0),
    }
}
