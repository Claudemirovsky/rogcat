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
    profiles::Profile,
    utils::{config_get, terminal_width},
    LogSink,
};
use failure::{format_err, Error};
use futures::{
    sink::{Sink, SinkExt},
    task::{Context, Poll},
};
use regex::Regex;
use rogcat::record::{Format, Level, Record};
use std::{
    cmp::{max, min},
    convert::Into,
    io::{stdout, BufWriter, Write},
    pin::Pin,
};
use termcolor::{Buffer, BufferWriter, Color, ColorChoice, ColorSpec, WriteColor};

const DIMM_COLOR: Color = Color::Ansi256(243);

/// Construct a terminal sink for format from args with give profile
pub fn try_from(args: &CliArguments, profile: &Profile) -> Result<LogSink, Error> {
    let format = args
        .format
        .as_ref()
        .map(|it| it.to_owned())
        .unwrap_or(Format::Human);

    if format == Format::Html {
        return Err(format_err!("HTML format is only valid for file output"));
    }

    let sink = Box::into_pin(match format {
        Format::Human => Box::new(Human::from(args, profile, format)) as LogSink,
        format => Box::new(FormatSink::new(format, stdout())) as LogSink,
    });

    Ok(Box::new(sink.sink_map_err(|e| {
        failure::format_err!("Terminal error: {}", e)
    })))
}

#[derive(PartialEq)]
enum DateFormat {
    Complete,
    Nothing,
    HourOnly,
    DateOnly,
}
/// Human readable terminal output
struct Human {
    writer: BufferWriter,
    date_format: DateFormat,
    highlight: Vec<Regex>,
    process_width: usize,
    tag_width: Option<usize>,
    thread_width: usize,
    dimm_color: Option<Color>,
    bright_colors: bool,
}

impl Human {
    pub fn from(args: &CliArguments, profile: &Profile, _: Format) -> Human {
        let mut hl = profile.highlight.to_owned();
        if !args.highlight.is_empty() {
            hl.extend(args.highlight.to_owned());
        }
        let highlight = hl.iter().flat_map(|h| Regex::new(h)).collect();

        let color = {
            match args
                .color
                .as_deref()
                .unwrap_or_else(|| config_get("terminal_color").unwrap_or("auto"))
            {
                "always" => ColorChoice::Always,
                "never" => ColorChoice::Never,
                "auto" => {
                    if atty::is(atty::Stream::Stdout) {
                        ColorChoice::Auto
                    } else {
                        ColorChoice::Never
                    }
                }
                _ => ColorChoice::Auto,
            }
        };
        let no_dimm = args.no_dimm || config_get("terminal_no_dimm").unwrap_or(false);
        let tag_width = config_get("terminal_tag_width");
        let hide_timestamp =
            args.hide_timestamp || config_get("terminal_hide_timestamp").unwrap_or(false);
        let show_date = args.show_date || config_get("terminal_show_date").unwrap_or(false);
        let date_format = if show_date {
            if hide_timestamp {
                DateFormat::DateOnly
            } else {
                DateFormat::Complete
            }
        } else if hide_timestamp {
            DateFormat::Nothing
        } else {
            DateFormat::HourOnly
        };

        let bright_colors =
            args.bright_colors || config_get("terminal_bright_colors").unwrap_or(false);

        Human {
            writer: BufferWriter::stdout(color),
            dimm_color: if no_dimm { None } else { Some(DIMM_COLOR) },
            highlight,
            date_format,
            tag_width,
            process_width: 0,
            thread_width: 0,
            bright_colors,
        }
    }

    // Dynamic tag width estimation according to terminal width
    fn tag_width(&self) -> usize {
        self.tag_width.unwrap_or_else(|| match terminal_width() {
            Some(n) if n <= 80 => 15,
            Some(n) if n <= 90 => 20,
            Some(n) if n <= 100 => 25,
            Some(n) if n <= 110 => 30,
            _ => 35,
        })
    }

    #[cfg(target_os = "windows")]
    fn hashed_color(i: &str) -> Color {
        let v = i.bytes().fold(42u8, |c, x| c ^ x) % 7;
        match v {
            0 => Color::Blue,
            1 => Color::Green,
            2 => Color::Red,
            3 => Color::Cyan,
            4 => Color::Magenta,
            5 => Color::Yellow,
            _ => Color::White,
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn hashed_color(i: &str) -> Color {
        // Some colors are hard to read on (at least) dark terminals
        // and I consider some others as ugly.
        Color::Ansi256(match i.bytes().fold(42u8, |c, x| c ^ x) {
            c @ 0..=1 => c + 2,
            c @ 16..=21 => c + 6,
            c @ 52..=55 | c @ 126..=129 => c + 4,
            c @ 163..=165 | c @ 200..=201 => c + 3,
            c @ 207 => c + 1,
            c @ 232..=240 => c + 9,
            c => c,
        })
    }

    fn print(&mut self, record: &Record) -> Result<(), Error> {
        let timestamp = if self.date_format != DateFormat::Nothing {
            let time = record.time.to_owned().unwrap_or_default();
            match self.date_format {
                DateFormat::Complete => Some(time),
                DateFormat::DateOnly => time.get(0..5).map(str::to_string),
                DateFormat::HourOnly => time.get(6..).map(str::to_string),
                _ => None,
            }
        } else {
            None
        }
        .unwrap_or_default();

        let tag_width = self.tag_width();
        let tag_chars = record.tag.chars().count();
        let tag = format!(
            "{:>width$}",
            record
                .tag
                .chars()
                .take(min(tag_width, tag_chars))
                .collect::<String>(),
            width = tag_width
        );

        self.process_width = max(self.process_width, record.process.chars().count());
        let pid = if record.process.is_empty() {
            " ".repeat(self.process_width)
        } else {
            format!("{:<width$}", record.process, width = self.process_width)
        };
        self.thread_width = max(self.thread_width, record.thread.chars().count());
        let tid = if !record.thread.is_empty() {
            format!(" {:>width$}", record.thread, width = self.thread_width)
        } else if self.thread_width != 0 {
            " ".repeat(self.thread_width + 1)
        } else {
            String::new()
        };

        let highlight = !self.highlight.is_empty()
            && (self.highlight.iter().any(|r| r.is_match(&record.tag))
                || self.highlight.iter().any(|r| r.is_match(&record.message)));

        let preamble_width = timestamp.chars().count()
            + 1 // " "
            + tag.chars().count()
            + 2 // " ("
            + pid.chars().count() + tid.chars().count()
            + 2 // ") "
            + 3; // level

        let timestamp_color = if highlight {
            Some(Color::Yellow)
        } else {
            self.dimm_color
        };
        let tag_color = Self::hashed_color(&record.tag);
        let pid_color = Self::hashed_color(&pid);
        let tid_color = Self::hashed_color(&tid);
        let level_color = match record.level {
            Level::Debug => Some(Color::Cyan),
            Level::Info => Some(Color::Green),
            Level::Warn => Some(Color::Yellow),
            Level::Error | Level::Fatal | Level::Assert => Some(Color::Red),
            _ => self.dimm_color,
        };

        let write_preamble = |buffer: &mut Buffer| -> Result<(), Error> {
            let mut spec = ColorSpec::new();
            buffer.set_color(spec.set_fg(timestamp_color))?;
            buffer.write_all(timestamp.as_bytes())?;
            buffer.write_all(b" ")?;

            buffer.set_color(spec.set_fg(Some(tag_color)))?;
            buffer.write_all(tag.as_bytes())?;
            buffer.set_color(spec.set_fg(None))?;

            buffer.write_all(b" (")?;
            buffer.set_color(spec.set_fg(Some(pid_color)))?;
            buffer.write_all(pid.as_bytes())?;
            if !tid.is_empty() {
                buffer.set_color(spec.set_fg(Some(tid_color)))?;
                buffer.write_all(tid.as_bytes())?;
            }
            buffer.set_color(spec.set_fg(None))?;
            buffer.write_all(b") ")?;

            buffer.set_color(
                spec.set_bg(level_color)
                    .set_fg(level_color.map(|_| Color::Black)), // Set fg only if bg is set
            )?;
            write!(buffer, " {} ", record.level)?;
            buffer.set_color(&ColorSpec::new())?;

            Ok(())
        };

        let payload_len = terminal_width().unwrap_or(std::usize::MAX) - preamble_width - 3;
        let message = record.message.replace('\t', "");
        let message_len = message.chars().count();
        let chunks = message_len / payload_len + 1;

        let mut buffer = self.writer.buffer();

        for i in 0..chunks {
            write_preamble(&mut buffer)?;

            let c = if chunks == 1 {
                "   "
            } else if i == 0 {
                " ┌ "
            } else if i == chunks - 1 {
                " └ "
            } else {
                " ├ "
            };

            buffer.write_all(c.as_bytes())?;

            let chunk = message
                .chars()
                .skip(i * payload_len)
                .take(payload_len)
                .collect::<String>();
            buffer.set_color(
                ColorSpec::new()
                    .set_intense(self.bright_colors)
                    .set_fg(level_color),
            )?;
            buffer.write_all(chunk.as_bytes())?;
            buffer.write_all(b"\n")?;
        }

        self.writer.print(&buffer).map_err(Into::into)
    }
}

impl Drop for Human {
    fn drop(&mut self) {
        let mut buffer = self.writer.buffer();
        buffer.reset().and_then(|_| self.writer.print(&buffer)).ok();
    }
}

struct FormatSink<T: Write> {
    format: Format,
    sink: BufWriter<T>,
}

impl<T: Write> FormatSink<T> {
    fn new(format: Format, sink: T) -> FormatSink<T> {
        FormatSink {
            format,
            sink: BufWriter::new(sink),
        }
    }
}

impl<T: Write + std::marker::Unpin> Sink<Record> for FormatSink<T> {
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: Record) -> Result<(), Self::Error> {
        let this = self.get_mut();
        this.sink
            .write_all(this.format.fmt_record(&item)?.as_bytes())?;
        this.sink.write_all(&[b'\n'])?;
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl Sink<Record> for Human {
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(mut self: Pin<&mut Self>, item: Record) -> Result<(), Self::Error> {
        self.print(&item)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}
