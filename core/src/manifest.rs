// SPDX-License-Identifier: GPL-3.0-only
//! The boot manifest: the list of operating systems shown in the menu.
//!
//! ```text
//! # Main manifest
//! timeout = 10
//! default = irix7
//! include = http://os.example.org/indy/indyboot.cfg
//!
//! [irix7]
//! title  = IRIX 7
//! image  = http://os.example.org/indy/irix7
//! sha256 = 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
//! size   = 1598392
//! args   = console=d
//! ```
//!
//! Top-level keys come before the first `[section]`. Each section is one menu
//! entry. `image` and `sha256` are required. An `include` names another
//! manifest whose sections are appended; an included file cannot include
//! further files, and its `timeout` and `default` are ignored.

use crate::url::{self, UrlBuf, UrlError};
use heapless::{String, Vec};

/// Menu entries across the main manifest and its includes.
pub const MAX_ENTRIES: usize = 16;
/// `include` lines in the main manifest.
pub const MAX_INCLUDES: usize = 8;
/// Largest manifest file, in bytes.
pub const MANIFEST_MAX: usize = 8192;

pub const NAME_MAX: usize = 32;
pub const TITLE_MAX: usize = 60;
pub const ARGS_MAX: usize = 120;

/// One bootable image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Section name: letters, digits, `.`, `_` and `-`.
    pub name: String<NAME_MAX>,
    /// Menu text. Defaults to the name.
    pub title: String<TITLE_MAX>,
    /// Absolute `http://` URL of the ECOFF image.
    pub image: UrlBuf,
    pub sha256: [u8; 32],
    /// Image size in bytes, when the manifest states it.
    pub size: Option<u32>,
    /// Words passed to the image as `argv[1..]`.
    pub args: String<ARGS_MAX>,
}

#[derive(Debug, Default)]
pub struct Manifest {
    pub entries: Vec<Entry, MAX_ENTRIES>,
    pub includes: Vec<UrlBuf, MAX_INCLUDES>,
    /// Name of the entry booted when the countdown ends.
    pub default: String<NAME_MAX>,
    /// Countdown in seconds. `None` waits for a key.
    pub timeout: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The line is neither `key = value`, `[name]`, a comment nor blank.
    Syntax,
    /// A section name is empty, too long or has a character outside
    /// `A-Z a-z 0-9 . _ -`.
    Name,
    /// Two sections share a name.
    Duplicate,
    /// A section has no `image`.
    MissingImage,
    /// A section has no `sha256`.
    MissingSha256,
    /// `sha256` is not 64 hexadecimal digits.
    Sha256,
    /// `size` or `timeout` is not a decimal number.
    Number,
    /// A value is longer than its field.
    TooLong,
    /// More than [`MAX_ENTRIES`] sections or [`MAX_INCLUDES`] includes.
    TooMany,
    /// `include` inside a section, or inside an included file.
    Include,
    /// An unknown key before the first section.
    Key,
    Url(UrlError),
}

impl ErrorKind {
    pub fn message(self) -> &'static str {
        match self {
            ErrorKind::Syntax => "expected `key = value` or `[name]`",
            ErrorKind::Name => "section name must be 1 to 32 of A-Z a-z 0-9 . _ -",
            ErrorKind::Duplicate => "section name is already used",
            ErrorKind::MissingImage => "section has no image",
            ErrorKind::MissingSha256 => "section has no sha256",
            ErrorKind::Sha256 => "sha256 must be 64 hexadecimal digits",
            ErrorKind::Number => "expected a decimal number",
            ErrorKind::TooLong => "value is too long",
            ErrorKind::TooMany => "too many sections or includes",
            ErrorKind::Include => "include must be a top-level key of the main manifest",
            ErrorKind::Key => "unknown top-level key",
            ErrorKind::Url(error) => error.message(),
        }
    }
}

/// A manifest error and the 1-based line it was found on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError {
    pub line: u32,
    pub kind: ErrorKind,
}

/// Which file is being parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The manifest named by the boot URL.
    Main,
    /// A file named by an `include` line.
    Included,
}

struct Pending {
    line: u32,
    name: String<NAME_MAX>,
    title: String<TITLE_MAX>,
    image: Option<UrlBuf>,
    sha256: Option<[u8; 32]>,
    size: Option<u32>,
    args: String<ARGS_MAX>,
}

impl Manifest {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse `text`, fetched from `base`, and append its entries.
    ///
    /// Relative `image` and `include` values are resolved against `base`.
    pub fn parse(&mut self, text: &str, base: &str, level: Level) -> Result<(), ParseError> {
        let mut pending: Option<Pending> = None;
        for (index, raw) in text.lines().enumerate() {
            let line_no = index as u32 + 1;
            let at = |kind| ParseError {
                line: line_no,
                kind,
            };
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                let name = rest.strip_suffix(']').ok_or(at(ErrorKind::Syntax))?.trim();
                if let Some(done) = pending.take() {
                    self.finish(done)?;
                }
                if !valid_name(name) {
                    return Err(at(ErrorKind::Name));
                }
                if self.entries.iter().any(|entry| entry.name.as_str() == name) {
                    return Err(at(ErrorKind::Duplicate));
                }
                pending = Some(Pending {
                    line: line_no,
                    name: copy(name).map_err(at)?,
                    title: String::new(),
                    image: None,
                    sha256: None,
                    size: None,
                    args: String::new(),
                });
                continue;
            }
            let (key, value) = line.split_once('=').ok_or(at(ErrorKind::Syntax))?;
            let (key, value) = (key.trim(), value.trim());
            match &mut pending {
                None => match key {
                    "include" => {
                        if level == Level::Included {
                            return Err(at(ErrorKind::Include));
                        }
                        let target =
                            url::resolve(base, value).map_err(|e| at(ErrorKind::Url(e)))?;
                        self.includes
                            .push(target)
                            .map_err(|_| at(ErrorKind::TooMany))?;
                    }
                    "timeout" => {
                        let seconds = value.parse().map_err(|_| at(ErrorKind::Number))?;
                        if level == Level::Main {
                            self.timeout = Some(seconds);
                        }
                    }
                    "default" => {
                        if level == Level::Main {
                            self.default = copy(value).map_err(at)?;
                        }
                    }
                    _ => return Err(at(ErrorKind::Key)),
                },
                Some(entry) => match key {
                    "title" => entry.title = copy(value).map_err(at)?,
                    "image" => {
                        entry.image =
                            Some(url::resolve(base, value).map_err(|e| at(ErrorKind::Url(e)))?);
                    }
                    "sha256" => {
                        entry.sha256 =
                            Some(crate::parse_sha256(value).ok_or(at(ErrorKind::Sha256))?);
                    }
                    "size" => {
                        entry.size = Some(value.parse().map_err(|_| at(ErrorKind::Number))?);
                    }
                    "args" => entry.args = copy(value).map_err(at)?,
                    "include" => return Err(at(ErrorKind::Include)),
                    // Keys added by a later manifest revision are skipped.
                    _ => {}
                },
            }
        }
        if let Some(done) = pending.take() {
            self.finish(done)?;
        }
        Ok(())
    }

    fn finish(&mut self, done: Pending) -> Result<(), ParseError> {
        let at = |kind| ParseError {
            line: done.line,
            kind,
        };
        let image = done.image.ok_or(at(ErrorKind::MissingImage))?;
        let sha256 = done.sha256.ok_or(at(ErrorKind::MissingSha256))?;
        let title = if done.title.is_empty() {
            copy(done.name.as_str()).map_err(at)?
        } else {
            done.title
        };
        self.entries
            .push(Entry {
                name: done.name,
                title,
                image,
                sha256,
                size: done.size,
                args: done.args,
            })
            .map_err(|_| at(ErrorKind::TooMany))
    }

    /// Index of the entry named by `default`.
    pub fn default_index(&self) -> Option<usize> {
        if self.default.is_empty() {
            return None;
        }
        self.entries
            .iter()
            .position(|entry| entry.name == self.default)
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn copy<const N: usize>(text: &str) -> Result<String<N>, ErrorKind> {
    let mut out = String::new();
    out.push_str(text).map_err(|_| ErrorKind::TooLong)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "http://boot.example.org/indy/boot.cfg";
    const SUM: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    fn parse(text: &str, level: Level) -> Result<Manifest, ParseError> {
        let mut manifest = Manifest::new();
        manifest.parse(text, BASE, level)?;
        Ok(manifest)
    }

    #[test]
    fn full_manifest() {
        let text = format!(
            "# list\ntimeout = 10\ndefault = redox\ninclude = http://os.example.org/indyboot.cfg\n\
             include = haiku/indyboot.cfg\n\n[irix7]\ntitle = IRIX 7\nimage = irix7\n\
             sha256 = {SUM}\nsize = 1598392\nargs = console=d verbose\n\n[redox]\n\
             image = http://redox.example.org/redox\nsha256 = {SUM}\nfuture-key = 1\n"
        );
        let manifest = parse(&text, Level::Main).unwrap();
        assert_eq!(manifest.timeout, Some(10));
        assert_eq!(manifest.default_index(), Some(1));
        assert_eq!(manifest.includes.len(), 2);
        assert_eq!(
            manifest.includes[1].as_str(),
            "http://boot.example.org/indy/haiku/indyboot.cfg"
        );
        let irix = &manifest.entries[0];
        assert_eq!(irix.title.as_str(), "IRIX 7");
        assert_eq!(irix.image.as_str(), "http://boot.example.org/indy/irix7");
        assert_eq!(irix.size, Some(1598392));
        assert_eq!(irix.args.as_str(), "console=d verbose");
        assert_eq!(irix.sha256[0], 0x9f);
        let redox = &manifest.entries[1];
        assert_eq!(redox.title.as_str(), "redox");
        assert_eq!(redox.size, None);
    }

    #[test]
    fn included_file_appends_and_ignores_menu_keys() {
        let mut manifest = parse("timeout = 5\ndefault = a\n", Level::Main).unwrap();
        let fragment = format!("timeout = 99\ndefault = b\n[b]\nimage = b.img\nsha256 = {SUM}\n");
        manifest
            .parse(
                &fragment,
                "http://b.example/x/indyboot.cfg",
                Level::Included,
            )
            .unwrap();
        assert_eq!(manifest.timeout, Some(5));
        assert_eq!(manifest.default.as_str(), "a");
        assert_eq!(
            manifest.entries[0].image.as_str(),
            "http://b.example/x/b.img"
        );
        assert_eq!(manifest.default_index(), None);
    }

    fn kind(text: &str, level: Level) -> (u32, ErrorKind) {
        let error = parse(text, level).unwrap_err();
        (error.line, error.kind)
    }

    #[test]
    fn errors_carry_the_line() {
        assert_eq!(
            kind("timeout = 5\nwhat\n", Level::Main),
            (2, ErrorKind::Syntax)
        );
        assert_eq!(kind("[a b]\n", Level::Main), (1, ErrorKind::Name));
        assert_eq!(kind("[a\n", Level::Main), (1, ErrorKind::Syntax));
        assert_eq!(kind("colour = red\n", Level::Main), (1, ErrorKind::Key));
        assert_eq!(
            kind("timeout = soon\n", Level::Main),
            (1, ErrorKind::Number)
        );
        assert_eq!(
            kind(&format!("\n[a]\nsha256 = {SUM}\n"), Level::Main),
            (2, ErrorKind::MissingImage)
        );
        assert_eq!(
            kind("[a]\nimage = a\n", Level::Main),
            (1, ErrorKind::MissingSha256)
        );
        assert_eq!(
            kind("[a]\nsha256 = abc\n", Level::Main),
            (2, ErrorKind::Sha256)
        );
        assert_eq!(
            kind("[a]\nimage = https://x/a\n", Level::Main),
            (2, ErrorKind::Url(UrlError::Https))
        );
        assert_eq!(
            kind("[a]\ninclude = x\n", Level::Main),
            (2, ErrorKind::Include)
        );
        assert_eq!(
            kind("include = x\n", Level::Included),
            (1, ErrorKind::Include)
        );
    }

    #[test]
    fn duplicate_names_and_limits() {
        let one = format!("[a]\nimage = a\nsha256 = {SUM}\n");
        let twice = format!("{one}{one}");
        assert_eq!(kind(&twice, Level::Main), (4, ErrorKind::Duplicate));

        let mut many = std::string::String::new();
        for i in 0..=MAX_ENTRIES {
            many.push_str(&format!("[e{i}]\nimage = a\nsha256 = {SUM}\n"));
        }
        assert_eq!(
            parse(&many, Level::Main).unwrap_err().kind,
            ErrorKind::TooMany
        );

        let long_title = format!("[a]\ntitle = {}\n", "t".repeat(TITLE_MAX + 1));
        assert_eq!(kind(&long_title, Level::Main), (2, ErrorKind::TooLong));
    }
}
