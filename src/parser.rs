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

use crate::record::{Level, Record};
use csv::ReaderBuilder;
use failure::Fail;

use nom::{bytes::complete::take_until, character::complete::char, IResult};

use serde_json::from_str;
use std::{
    convert::Into,
    io::{Cursor, Read},
};

#[derive(Fail, Debug)]
#[fail(display = "{}", _0)]
pub struct ParserError(String);

pub trait FormatParser: Send + Sync {
    fn try_parse_str(&self, line: &str) -> Result<Record, ParserError>;
}

#[inline]
fn level(level: &str) -> Result<Level, ParserError> {
    match level {
        "V" => Ok(Level::Verbose),
        "D" => Ok(Level::Debug),
        "I" => Ok(Level::Info),
        "W" => Ok(Level::Warn),
        "E" => Ok(Level::Error),
        "F" => Ok(Level::Fatal),
        "A" => Ok(Level::Assert),
        _ => Err(ParserError(format!("Invalid level: {}", level))),
    }
}

// date, hour, pid, thread, level, tag
const MIN_PARTS_COUNT: usize = 6;
fn printable(line: &str) -> Result<Record, ParserError> {
    if line.split_ascii_whitespace().count() < MIN_PARTS_COUNT {
        return Err(ParserError("Invalid line size".into()));
    }

    let mut items = line.split_ascii_whitespace();
    let (date, hour) = (
        items.next().unwrap_or("01-01"),
        items.next().unwrap_or("00:00"),
    );

    let (process, thread) = (items.next().unwrap(), items.next().unwrap());
    if !(process.chars().all(char::is_numeric) && process.chars().all(char::is_numeric)) {
        return Err(ParserError(
            "Invalid Process/Thread ID: Pid {process}, Thread {thread}".into(),
        ));
    }
    let level = level(items.next().unwrap())?;
    let tag = {
        // Basically a take_while(':') but considering the failing match too.
        let mut list: Vec<&str> = vec![];
        for part in items.by_ref() {
            if let Some(fixed) = part.strip_suffix(':') {
                list.push(fixed);
                break;
            } else {
                list.push(part);
            }
        }
        list.join(" ")
    };
    let message = items.collect::<Vec<&str>>().join(" ");

    let rec = Record {
        raw: line.into(),
        time: Some(format!("{date} {hour}")),
        message: message.trim().to_owned(),
        level,
        tag: tag.trim().to_owned(),
        process: process.trim().to_owned(),
        thread: thread.trim().to_owned(),
    };

    Ok(rec)
}

pub fn bugreport_section(line: &str) -> IResult<&str, (String, String)> {
    let (line, logtag) = take_until("(")(line)?;
    let (line, _) = char('(')(line)?;
    let (line, msg) = take_until(")")(line)?;
    let (line, _) = char(')')(line)?;

    Ok((line, (logtag.to_string(), msg.to_string())))
}

pub struct DefaultParser;

impl FormatParser for DefaultParser {
    fn try_parse_str(&self, line: &str) -> Result<Record, ParserError> {
        printable(line).map_err(|e| ParserError(format!("{e}")))
    }
}

pub struct CsvParser;

impl FormatParser for CsvParser {
    fn try_parse_str(&self, line: &str) -> Result<Record, ParserError> {
        let reader = Cursor::new(line).chain(Cursor::new([b'\n']));
        let mut rdr = ReaderBuilder::new().has_headers(false).from_reader(reader);
        if let Some(result) = rdr.deserialize().next() {
            result.map_err(|e| ParserError(format!("{e}")))
        } else {
            Err(ParserError("Failed to parse csv".to_string()))
        }
    }
}

pub struct JsonParser;

impl FormatParser for JsonParser {
    fn try_parse_str(&self, line: &str) -> Result<Record, ParserError> {
        from_str(line).map_err(|e| ParserError(format!("Failed to deserialize json: {e}")))
    }
}

pub struct Parser {
    parsers: Vec<Box<dyn FormatParser>>,
    last: Option<usize>,
}

impl Default for Parser {
    fn default() -> Self {
        Parser {
            parsers: vec![
                Box::new(DefaultParser),
                Box::new(CsvParser),
                Box::new(JsonParser),
            ],
            last: None,
        }
    }
}

impl Parser {
    pub fn new() -> Self {
        Parser {
            parsers: Vec::new(),
            last: None,
        }
    }

    pub fn parse(&mut self, line: &str) -> Record {
        if let Some(last) = self.last {
            let p = &self.parsers[last];
            if let Ok(r) = p.try_parse_str(line) {
                return r;
            }
        }

        for (i, p) in self.parsers.iter().map(Box::as_ref).enumerate() {
            if let Ok(r) = p.try_parse_str(line) {
                self.last = Some(i);
                return r;
            }
        }

        // Seems that we cannot parse this record
        // Treat the raw input as message
        Record {
            raw: String::from(line),
            message: String::from(line),
            ..Default::default()
        }
    }
}

#[test]
fn parse_level() -> Result<(), ParserError> {
    assert_eq!(level("V")?, Level::Verbose);
    assert_eq!(level("D")?, Level::Debug);
    assert_eq!(level("I")?, Level::Info);
    assert_eq!(level("W")?, Level::Warn);
    assert_eq!(level("E")?, Level::Error);
    assert_eq!(level("F")?, Level::Fatal);
    assert_eq!(level("A")?, Level::Assert);
    assert!(level("INEXISTENT").is_err());
    Ok(())
}

#[test]
fn parse_printable() {
    let t = "03-01 02:19:45.207     1     2 I EXT4-fs (mmcblk3p8): mounted filesystem with \
             ordered data mode. Opts: (null)";
    let p = DefaultParser {};
    let r = p.try_parse_str(t).unwrap();
    assert_eq!(r.level, Level::Info);
    assert_eq!(r.tag, "EXT4-fs (mmcblk3p8)");
    assert_eq!(r.process, "1");
    assert_eq!(r.thread, "2");
    assert_eq!(
        r.message,
        "mounted filesystem with ordered data mode. Opts: (null)"
    );

    let t = "03-01 02:19:42.868     0     0 D /soc/aips-bus@02100000/usdhc@0219c000: \
             voltage-ranges unspecified";
    let r = p.try_parse_str(t).unwrap();
    assert_eq!(r.level, Level::Debug);
    assert_eq!(r.tag, "/soc/aips-bus@02100000/usdhc@0219c000");
    assert_eq!(r.process, "0");
    assert_eq!(r.thread, "0");
    assert_eq!(r.message, "voltage-ranges unspecified");

    let t = "11-06 13:58:53.582 31359 31420 I GStreamer+amc: 0:00:00.326067533 0xb8ef2a00";
    let r = p.try_parse_str(t).unwrap();
    assert_eq!(r.time, Some("11-06 13:58:53.582".to_string()));
    assert_eq!(r.level, Level::Info);
    assert_eq!(r.tag, "GStreamer+amc");
    assert_eq!(r.process, "31359");
    assert_eq!(r.thread, "31420");
    assert_eq!(r.message, "0:00:00.326067533 0xb8ef2a00");

    let t = "11-06 13:58:53.582 31359 31420 A GStreamer+amc: 0:00:00.326067533 0xb8ef2a00";
    let r = p.try_parse_str(t).unwrap();
    assert_eq!(r.level, Level::Assert);
    assert_eq!(r.tag, "GStreamer+amc");
    assert_eq!(r.process, "31359");
    assert_eq!(r.thread, "31420");
    assert_eq!(r.message, "0:00:00.326067533 0xb8ef2a00");

    let t = "03-26 13:17:38.345     0     0 I [114416.534450,0] mdss_dsi_off-: ";
    let r = p.try_parse_str(t).unwrap();
    assert_eq!(r.level, Level::Info);
    assert_eq!(r.tag, "[114416.534450,0] mdss_dsi_off-");
    assert_eq!(r.message, "");
}

#[test]
fn test_parse_csv() {
    let t = "07-01 14:13:14.446000000,Sensor:batt_therm:29000 mC,Info,ThermalEngine,225,295,07-01 14:13:14.446   225   295 I ThermalEngine: Sensor:batt_therm:29000 mC";
    let p = CsvParser {};
    let r = p.try_parse_str(t).unwrap();
    assert_eq!(r.level, Level::Info);
    assert_eq!(r.tag, "ThermalEngine");
    assert_eq!(r.process, "225");
    assert_eq!(r.thread, "295");
    assert_eq!(r.message, "Sensor:batt_therm:29000 mC");
    assert_eq!(
        r.raw,
        "07-01 14:13:14.446   225   295 I ThermalEngine: Sensor:batt_therm:29000 mC"
    );
}
