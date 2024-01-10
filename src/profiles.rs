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

use crate::{cli::CliArguments, utils};
use failure::{format_err, Error};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap, convert::Into, env::var, fs::File, io::Read, ops::AddAssign,
    path::PathBuf,
};
use toml::from_str;

const DEFAULT_PROFILE_NAME: &str = "default";

/// Profile definition with filters and misc
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    pub comment: Option<String>,
    pub extends: Vec<String>,
    pub highlight: Vec<String>,
    pub message: Vec<String>,
    pub message_ignore_case: Vec<String>,
    pub pid: Vec<String>,
    pub process_name: Vec<String>,
    pub regex: Vec<String>,
    pub tag: Vec<String>,
    pub tag_ignore_case: Vec<String>,
}

pub fn profiles_list(profiles_path: Option<&PathBuf>) -> Result<HashMap<String, Profile>, Error> {
    let file = file(profiles_path)?;
    if !file.exists() {
        Ok(HashMap::new())
    } else {
        let mut config = String::new();
        File::open(file.clone())
            .map_err(|e| format_err!("Failed to open {}: {}", file.display(), e))?
            .read_to_string(&mut config)?;

        let mut config_file: ConfigurationFile = from_str(&config)
            .map_err(|e| format_err!("Failed to parse {}: {}", file.display(), e))?;

        let profiles: HashMap<String, Profile> = config_file
            .profile
            .drain()
            .map(|(k, v)| (k, v.into()))
            .collect();
        Ok(profiles)
    }
}
/// Create a new Profiles instance from a give configuration file
/// and default if file is not present or readable
pub fn from_args(args: &CliArguments) -> Result<Profile, Error> {
    let profiles = profiles_list(args.profiles_path.as_ref())?;
    if profiles.is_empty() {
        Ok(Profile::default())
    } else {
        let mut profile = Profile::default();
        if let Some(selected) = args.profile.as_ref() {
            profile = profiles
                .get(selected.as_str())
                .ok_or_else(|| format_err!("Unknown profile {}", selected))?
                .clone();
            expand(selected.as_str(), &mut profile, &profiles)?;
        } else if let Some(default_profile) = profiles.get(DEFAULT_PROFILE_NAME) {
            profile = default_profile.clone();
            expand(DEFAULT_PROFILE_NAME, &mut profile, &profiles)?;
        }

        Ok(profile)
    }
}

/// Expand a profile with file content
fn expand(n: &str, p: &mut Profile, a: &HashMap<String, Profile>) -> Result<(), Error> {
    let mut recursion_limit = 100;
    while !p.extends.is_empty() {
        let extends = p.extends.clone();
        p.extends.clear();
        for e in &extends {
            let f = a
                .get(e)
                .ok_or_else(|| format_err!("Unknown extend profile name {} used in {}", e, n))?;
            *p += f.clone();
        }

        recursion_limit -= 1;
        if recursion_limit == 0 {
            return Err(format_err!(
                "Reached recursion limit while resolving profile {} extends",
                n
            ));
        }
    }
    Ok(())
}

/// Return path to profile file by checking cli argument, env and default to configdir
fn file(profile_path: Option<&PathBuf>) -> Result<PathBuf, Error> {
    if let Some(path) = profile_path {
        if path.exists() {
            return Ok(path.to_owned());
        } else {
            return Err(format_err!(
                "Cannot find {}. Use --profiles-path to specify the path manually!",
                path.display()
            ));
        }
    }

    if let Ok(f) = var("ROGCAT_PROFILES").map(PathBuf::from) {
        if f.exists() {
            Ok(f)
        } else {
            Err(format_err!(
                "Cannot find {} set in ROGCAT_PROFILES!",
                f.display()
            ))
        }
    } else {
        Ok(utils::config_dir().join("profiles.toml"))
    }
}

/// Configuration file
#[derive(Debug, Default, Deserialize, Serialize)]
struct ConfigurationFile {
    profile: HashMap<String, ProfileFile>,
}

/// Struct with exact layout as used in config file
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ProfileFile {
    comment: Option<String>,
    extends: Option<Vec<String>>,
    highlight: Option<Vec<String>>,
    message: Option<Vec<String>>,
    message_ignore_case: Option<Vec<String>>,
    pid: Option<Vec<String>>,
    process_name: Option<Vec<String>>,
    regex: Option<Vec<String>>,
    tag: Option<Vec<String>>,
    tag_ignore_case: Option<Vec<String>>,
}

impl From<ProfileFile> for Profile {
    fn from(f: ProfileFile) -> Profile {
        Profile {
            comment: f.comment,
            extends: f.extends.unwrap_or_default(),
            highlight: f.highlight.unwrap_or_default(),
            message: f.message.unwrap_or_default(),
            message_ignore_case: f.message_ignore_case.unwrap_or_default(),
            pid: f.pid.unwrap_or_default(),
            process_name: f.process_name.unwrap_or_default(),
            regex: f.regex.unwrap_or_default(),
            tag: f.tag.unwrap_or_default(),
            tag_ignore_case: f.tag_ignore_case.unwrap_or_default(),
        }
    }
}

impl AddAssign for Profile {
    fn add_assign(&mut self, other: Profile) {
        macro_rules! vec_extend {
            ($x:expr, $y:expr) => {
                $x.extend($y);
                $x.sort();
                $x.dedup();
            };
        }

        vec_extend!(self.extends, other.extends);
        vec_extend!(self.highlight, other.highlight);
        vec_extend!(self.message, other.message);
        vec_extend!(self.tag, other.tag);
    }
}
