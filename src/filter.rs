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

use std::{collections::HashSet, iter::FromIterator};

use crate::{cli::CliArguments, profiles::Profile, reader::get_processes_pids};
use failure::{format_err, Error};
use regex::Regex;
use rogcat::record::{Level, Record};

/// Configured filters
#[derive(Debug, Default)]
pub struct Filter {
    level: Level,
    tag: FilterGroup,
    tag_ignore_case: FilterGroup,
    message: FilterGroup,
    message_ignore_case: FilterGroup,
    pid: FilterGroup,
    process_name: FilterGroup,
    regex: FilterGroup,
}

async fn get_all_pids(procs: Option<Vec<String>>, profile: &mut Profile) {
    if let Some(processes) = procs {
        profile.process_name.extend(processes);
    }
    if !profile.process_name.is_empty() {
        profile
            .pid
            .extend(get_processes_pids(&profile.process_name).await);
    }
}

pub async fn from_args_profile(args: CliArguments, profile: &mut Profile) -> Result<Filter, Error> {
    get_all_pids(args.process_name, profile).await;
    let pid = profile.pid.iter();
    let process_name = profile.process_name.iter();
    let tag = profile.tag.iter();
    let tag_ignorecase = profile.tag_ignore_case.iter();
    let message = profile.message.iter();
    let message_ignorecase = profile.message_ignore_case.iter();
    let regex = profile.regex.iter();
    let filter = Filter {
        level: Level::from(args.level),
        tag: FilterGroup::from_args(&args.tag, tag, false)?,
        tag_ignore_case: FilterGroup::from_args(&args.tag_ignore_case, tag_ignorecase, true)?,
        message: FilterGroup::from_args(&args.message, message, false)?,
        message_ignore_case: FilterGroup::from_args(
            &args.message_ignore_case,
            message_ignorecase,
            true,
        )?,
        pid: FilterGroup::from_args(&args.pid, pid, false)?,
        process_name: FilterGroup::from_args(&Vec::new(), process_name, false)?,
        regex: FilterGroup::from_args(&args.regex_filter, regex, false)?,
    };

    Ok(filter)
}

impl Filter {
    pub fn filter(&mut self, record: &Record) -> bool {
        if record.level < self.level {
            return false;
        }

        match record.tag.as_ref() {
            "am_proc_start" if !self.process_name.is_empty() => {
                let parts = record.message.splitn(5, ',').collect::<Vec<&str>>();
                let pid = parts[1];
                let name = parts[3];
                if self.process_name.filter(name)
                    && !self
                        .pid
                        .positive
                        .iter()
                        // Prevents adding duplicates
                        .any(|x| x.is_match(pid))
                {
                    self.pid.positive.push(Regex::new(pid).unwrap());
                    return true;
                }
            }
            "am_kill" | "am_proc_died" => {
                let parts = record.message.splitn(3, ',').collect::<Vec<&str>>();
                let pid = parts[1];
                if self.pid.filter(pid) {
                    if let Ok(index) = self
                        .pid
                        .positive
                        .binary_search_by_key(&pid.to_string(), |x| x.to_string())
                    {
                        self.pid.positive.remove(index);
                        return true;
                    }
                }
            }
            _ => {}
        }

        if !self.process_name.positive.is_empty() && self.pid.positive.is_empty() {
            return false;
        }

        self.message.filter(&record.message)
            && self.message_ignore_case.filter(&record.message)
            && self.tag.filter(&record.tag)
            && self.tag_ignore_case.filter(&record.tag)
            && self.pid.filter(&record.process)
            && (self.regex.filter(&record.process)
                || self.regex.filter(&record.thread)
                || self.regex.filter(&record.tag)
                || self.regex.filter(&record.message))
    }
}

#[derive(Debug, Default)]
struct FilterGroup {
    ignore_case: bool,
    positive: Vec<Regex>,
    negative: Vec<Regex>,
}

impl FilterGroup {
    fn from_args<'a, T: Iterator<Item = &'a String>>(
        args: &'a [String],
        merge: T,
        ignore_case: bool,
    ) -> Result<FilterGroup, Error> {
        let mut filters: HashSet<&String> = HashSet::from_iter(args.iter());
        filters.extend(merge);

        let mut positive = vec![];
        let mut negative = vec![];
        for r in filters.iter().map(|f| {
            if ignore_case {
                f.to_lowercase()
            } else {
                (*f).to_string()
            }
        }) {
            if let Some(r) = r.strip_prefix('!') {
                let r =
                    Regex::new(r).map_err(|e| format_err!("Invalid regex string: {}: {}", r, e))?;
                negative.push(r);
            } else {
                let r = Regex::new(&r)
                    .map_err(|e| format_err!("Invalid regex string: {}: {}", r, e))?;
                positive.push(r);
            }
        }

        Ok(FilterGroup {
            ignore_case,
            positive,
            negative,
        })
    }

    fn filter(&self, item: &str) -> bool {
        if !self.positive.is_empty() {
            if self.ignore_case {
                let item = item.to_lowercase();
                if !self.positive.iter().any(|m| m.is_match(&item)) {
                    return false;
                }
            } else if !self.positive.iter().any(|m| m.is_match(item)) {
                return false;
            }
        }

        if !self.negative.is_empty() {
            if self.ignore_case {
                let item = item.to_lowercase();
                return !self.negative.iter().any(|m| m.is_match(&item));
            } else {
                return !self.negative.iter().any(|m| m.is_match(item));
            }
        }

        true
    }

    fn is_empty(&self) -> bool {
        self.positive.is_empty() && self.negative.is_empty()
    }

    #[cfg(test)]
    #[inline]
    fn add_item(&mut self, regex: &str, positive: bool) {
        let parsed = Regex::new(regex).unwrap();
        if positive {
            self.positive.push(parsed);
        } else {
            self.negative.push(parsed);
        }
    }
}

#[test]
fn filtergroup_from_args() {
    let sensitive = FilterGroup::from_args(
        &[String::from("fish"), String::from("!pirarucu")],
        Vec::new().iter(),
        false,
    )
    .unwrap();
    assert!(!sensitive.ignore_case);
    assert!(!sensitive.positive.is_empty());
    assert!(!sensitive.negative.is_empty());

    let insensitive = FilterGroup::from_args(
        &[String::from("tilapia"), String::from("crustacean")],
        [
            String::from("crustacean"),
            String::from("tilapia"),
            String::from("carp"),
        ]
        .iter(),
        true,
    )
    .unwrap();
    assert!(insensitive.ignore_case);
    assert_eq!(insensitive.positive.len(), 3);
    assert!(insensitive.negative.is_empty());

    let invalid = FilterGroup::from_args(&[String::from(")(")], std::iter::empty(), true);
    assert!(invalid.is_err());
}

#[test]
fn level_filter() {
    let mut filter = Filter {
        level: Level::Warn,
        ..Default::default()
    };

    let mut record = Record {
        level: Level::Info,
        ..Default::default()
    };

    // Info < Warn
    assert!(!filter.filter(&record));

    record.level = Level::Warn;
    // Warn == Warn
    assert!(filter.filter(&record));

    record.level = Level::Fatal;
    // Fatal > Warn
    assert!(filter.filter(&record));
}

#[test]
fn process_filter() {
    let mut filter = Filter::default();

    let mut record = Record {
        message:
            "[0,22551,10201,com.termux,pre-top-activity,{com.termux/com.termux.app.TermuxActivity}]"
                .to_string(),
        tag: "am_proc_start".to_string(),
        ..Default::default()
    };

    // Default filter lets anything pass
    assert!(filter.filter(&record));
    // Prevent regression with the bug that added pids for no reason
    assert!(filter.pid.is_empty());

    // Filter the via browser
    filter.process_name.add_item("mark.via.gp", true);

    // Termux isn't what we want, so ignore it
    assert!(!filter.filter(&record));

    // Filter termux too
    filter.process_name.add_item("com.termux", true);

    assert!(filter.filter(&record));

    // Checks if the pid was successfully added to the filter list
    assert!(!filter.pid.is_empty());
    assert!(filter.pid.positive.first().unwrap().is_match("22551"));

    // Different pid = gtfo
    record.process = "69420".to_string();
    assert!(!filter.filter(&record));

    // Remove pid when the process dies
    record.tag = "am_proc_died".to_string();
    record.message = "[0,22551,com.termux,800,10]".to_string();
    assert!(filter.filter(&record));
    assert!(filter.pid.is_empty());

    filter.process_name.positive.clear();
    // Just to make sure everything its passing
    assert!(filter.filter(&record));

    // Purge logs with this pid
    filter.pid.add_item("69420", false);
    assert!(!filter.filter(&record));
}

#[test]
fn message_filter() {
    let mut filter = Filter::default();
    let mut record = Record {
        message: "i HATE the antichrist".to_string(),
        ..Default::default()
    };

    // yep it contains the message
    filter.message.add_item("antichrist$", true);
    assert!(filter.filter(&record));

    // Doesnt contain our beloved message = gtfo
    record.message = "i HATE java".to_string();
    assert!(!filter.filter(&record));

    filter.message.positive.clear();
    filter.message_ignore_case.add_item("i\\shate", false);
    filter.message_ignore_case.ignore_case = true;
    assert!(!filter.filter(&record));

    // Containing a positive and a negative match must return false
    filter.message.add_item("java", true);
    assert!(!filter.filter(&record));
}

#[test]
fn filter_tag() {
    let mut filter = Filter::default();

    let mut record = Record {
        tag: "rustmoment".to_string(),
        ..Default::default()
    };

    // Basic positive/negative match
    filter.tag.add_item("r[uU]st", true);
    assert!(filter.filter(&record));

    // Doesnt contain our wanted tag = gtfo
    record.tag = "Aluminium".to_string();
    assert!(!filter.filter(&record));

    filter.tag.positive.clear();
    // God I HATE aluminium
    filter.tag.add_item("Alumin.um", false);
    assert!(!filter.filter(&record));

    filter.tag_ignore_case.ignore_case = true;
    filter.tag_ignore_case.add_item("ur.+ium", true);
    record.tag = "UrAnIuM".to_string();
    assert!(filter.filter(&record));
}
