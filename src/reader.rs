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

use crate::{
    cli::CliArguments,
    lossy_lines::{lossy_lines, LossyLinesCodec},
    utils::{adb, config_get},
    LogStream, StreamData, DEFAULT_BUFFER,
};
use failure::{err_msg, format_err, Error};
use futures::{
    stream::{iter, select},
    task::{Context, Poll},
    Stream, StreamExt, TryStreamExt,
};
#[cfg(target_os = "linux")]
use rogcat::record::Record;
use std::{
    borrow::ToOwned, convert::Into, net::ToSocketAddrs, path::PathBuf, pin::Pin, process::Stdio,
};
use time::{macros::format_description, OffsetDateTime};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, BufReader},
    net::TcpStream,
    process::{Child, Command},
};
use tokio_stream::wrappers::LinesStream;
use tokio_util::codec::{Decoder, FramedRead};
use url::Url;

/// A spawned child process that implements LogStream
struct Process {
    cmd: Vec<String>,
    /// Respawn cmd upon termination
    respawn: bool,
    child: Option<Child>,
    stream: Option<Pin<LogStream>>,
}

/// Open a file and provide a stream of lines
pub async fn files(files: Vec<PathBuf>) -> Result<LogStream, Error> {
    let f = iter::<_>(files)
        .map(|f| async move {
            let file = File::open(f.clone())
                .await
                .map_err(move |e| format_err!("Failed to open {}: {}", f.display(), e))
                .unwrap();
            Decoder::framed(LossyLinesCodec::new(), file)
                .map_ok(StreamData::Line)
                .map_err(move |e| format_err!("Failed to read file: {}", e))
                .filter_map(|x| async move { x.ok() })
        })
        .filter_map(|x| async move { Some(x.await) })
        .flatten();

    Ok(Box::new(f))
}

/// Open stdin and provide a stream of lines
pub fn stdin() -> LogStream {
    let s = FramedRead::new(tokio::io::stdin(), LossyLinesCodec::new())
        .map_ok(StreamData::Line)
        .filter_map(|x| async move { x.ok() });
    Box::new(s)
}

/// Open a serial port and provide a stream of lines
pub fn serial() -> LogStream {
    unimplemented!()
}

#[cfg(target_os = "linux")]
pub fn can(dev: &str) -> Result<LogStream, Error> {
    let process = dev.to_string();
    let now = OffsetDateTime::now_local()?;
    let format = format_description!("[unix_timestamp].[subsecond]");
    let stream = tokio_socketcan::CANSocket::open(dev)?
        .map_ok(move |s| {
            let data = s
                .data()
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<String>>();
            let extended = if s.is_extended() { "E" } else { " " };
            let time = now.format(format).ok();

            StreamData::Record(Record {
                time: time.to_owned(),
                message: format!("{} {} ", extended, data.join(" ")),
                tag: format!("0x{:x}", s.id()),
                raw: format!(
                    "({}) {} {}#{}",
                    &time.unwrap(),
                    process,
                    if s.is_extended() {
                        format!("{:08X}", s.id())
                    } else {
                        format!("{:X}", s.id())
                    },
                    data.join("")
                ),
                process: process.clone(),
                ..Default::default()
            })
        })
        .filter_map(|r| async move { r.ok() });
    Ok(Box::new(stream))
}

/// Connect to tcp socket and profile a stream of lines
pub async fn tcp(addr: &Url) -> Result<LogStream, Error> {
    let addr = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| err_msg("Failed to parse addr"))?;
    let tcp = TcpStream::connect(&addr)
        .await
        .map_err(|e| format_err!("Failed to connect: {}", e))
        .unwrap();

    let stream = Decoder::framed(LossyLinesCodec::new(), tcp)
        .map_ok(StreamData::Line)
        .filter_map(|x| async move { x.ok() });

    Ok(Box::new(stream))
}

pub async fn get_processes_pids(processes: &[String]) -> Vec<String> {
    let command = Command::new(adb().expect("Failed to find adb"))
        .arg("shell")
        .arg("ps")
        .arg("-Ao")
        .arg("pid,args")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to launch adb");
    let stdout = BufReader::new(command.stdout.unwrap());
    let future = LinesStream::new(stdout.lines())
        .skip(1)
        .filter_map(|x| async move {
            match x {
                Ok(line) if line.is_empty() => None,
                Ok(line) if line.starts_with("* daemon") => None,
                Ok(line) => Some(line),
                _ => None,
            }
        })
        .filter_map(|line| async move {
            let mut split = line.split_whitespace();
            let pid: &str = split.next().unwrap_or("unknown");
            let name: &str = split.next().unwrap_or("unknown");
            if processes.contains(&name.to_string()) {
                Some(pid.to_string())
            } else {
                None
            }
        });

    future.collect::<Vec<String>>().await
}

/// Start a process and stream it stdout
pub fn logcat(args: &CliArguments) -> Result<LogStream, Error> {
    let mut cmd = vec![adb()?.display().to_string()];

    if let Some(device) = args.device.as_ref() {
        cmd.push("-s".into());
        cmd.push(device.to_owned());
    }

    cmd.push("logcat".into());
    let mut respawn = args.restart | config_get::<bool>("restart").unwrap_or(true);

    if let Some(count) = args.tail {
        cmd.push("-t".into());
        cmd.push(count.to_string());
        respawn = false;
    };

    if args.dump {
        cmd.push("-d".into());
        respawn = false;
    }

    if args.last {
        cmd.push("--last".into());
        respawn = false;
    }

    for buffer in args
        .buffer
        .as_ref()
        .map(|v| v.to_owned())
        .or_else(|| config_get("buffer"))
        .unwrap_or_else(|| DEFAULT_BUFFER.iter().map(|&s| s.to_string()).collect())
    {
        cmd.push("-b".into());
        cmd.push(buffer.to_owned());
    }

    Ok(Box::new(Process::with_cmd(cmd, respawn)))
}

/// Start a process and stream it stdout
pub fn process(cmd: String, respawn: bool) -> Result<LogStream, Error> {
    let cmd = cmd.split_whitespace().map(ToOwned::to_owned).collect();
    Ok(Box::new(Process::with_cmd(cmd, respawn)))
}

impl Process {
    fn with_cmd(cmd: Vec<String>, respawn: bool) -> Process {
        Process {
            cmd,
            respawn,
            child: None,
            stream: None,
        }
    }

    fn spawn(&mut self, ctx: &mut Context<'_>) -> Poll<Option<StreamData>> {
        let mut child = Command::new(self.cmd[0].clone())
            .args(&self.cmd[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                eprintln!("Failed to spawn process ({:?}): {e}", self.cmd);
                std::process::exit(1);
            })
            .unwrap();

        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stderr = BufReader::new(child.stderr.take().unwrap());
        self.child = Some(child);

        let stdout = lossy_lines(stdout).map(StreamData::Line);
        let stderr = lossy_lines(stderr).map(StreamData::Line);

        let mut stream = select(stdout, stderr);
        let poll = stream.poll_next_unpin(ctx);
        self.stream = Some(Box::pin(stream));
        poll
    }
}

impl Stream for Process {
    type Item = StreamData;

    fn poll_next(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Option<StreamData>> {
        if let Some(ref mut inner) = self.stream {
            match inner.poll_next_unpin(ctx) {
                Poll::Ready(None) if self.respawn => self.spawn(ctx),
                poll => poll,
            }
        } else {
            self.spawn(ctx)
        }
    }
}
