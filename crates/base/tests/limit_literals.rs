//! No design limit is spelled as a literal outside
//! `crates/base/src/constants.rs`.
//!
//! Every Rust file in the repository is scanned with comments and string
//! literals removed. An integer literal in any spelling (`4096`, `4_096`,
//! `0x1000`, `4096u64`), or a chain of literals joined by `*` or by `<<`
//! (`4 * 1024`, `1 << 12`), whose value equals a limit is rejected.
//!
//! shortcut: limits below `CHECKED_FROM` (8, 10, 76, and the durations in
//! milliseconds) are not checked, because a grep cannot tell them from
//! ordinary small integers; upgrade to a typed lint if that proves too weak.

use granite_base::constants::{FORMAT, PROCESS_CONFIG_DEFAULT};
use std::fs;
use std::path::{Path, PathBuf};

// Values that fit in a byte are everywhere in ordinary code.
const CHECKED_FROM: u64 = u8::MAX as u64 + 1;
const MODULE: &str = "crates/base/src/constants.rs";

#[derive(Debug, PartialEq)]
enum Token {
    Int(u64),
    Op(&'static str),
    Other,
}

/// Splits Rust source into tokens with their line numbers, dropping
/// comments, strings and character literals.
fn tokens(source: &str) -> Vec<(usize, Token)> {
    let s: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line) = (0, 1);
    let at = |i: usize, text: &str| {
        text.chars()
            .enumerate()
            .all(|(k, c)| s.get(i + k) == Some(&c))
    };
    while i < s.len() {
        let c = s[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if at(i, "//") {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if at(i, "/*") {
            let mut depth = 0;
            loop {
                if at(i, "/*") {
                    depth += 1;
                    i += 2;
                } else if at(i, "*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else if i >= s.len() {
                    break;
                } else {
                    line += usize::from(s[i] == '\n');
                    i += 1;
                }
            }
        } else if c == 'r' || c == 'b' || c == 'c' || c == '"' {
            // A raw string r"..", r#".."#, br"..", or a plain string "..", b"..", c"..".
            let mut j = i;
            if s[j] != '"' {
                j += 1;
                if s[i] == 'b' && s.get(j) == Some(&'r') {
                    j += 1;
                }
            }
            let raw = j > i && s[j - 1] == 'r';
            let mut hashes = 0;
            while raw && s.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if s.get(j) != Some(&'"') {
                // An identifier or keyword that starts with that letter.
                while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                    i += 1;
                }
                out.push((line, Token::Other));
                continue;
            }
            j += 1;
            loop {
                match s.get(j) {
                    None => break,
                    Some('\\') if !raw => {
                        line += usize::from(s.get(j + 1) == Some(&'\n'));
                        j += 2;
                    }
                    Some('"') if (1..=hashes).all(|k| s.get(j + k) == Some(&'#')) => {
                        j += 1 + hashes;
                        break;
                    }
                    Some(&ch) => {
                        line += usize::from(ch == '\n');
                        j += 1;
                    }
                }
            }
            i = j;
            out.push((line, Token::Other));
        } else if c == '\'' {
            // A character literal ('a', '\n', '\u{1F600}'), or a lifetime ('a).
            if s.get(i + 1) == Some(&'\\') {
                i += 3;
                while i < s.len() && s[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if s.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                i += 1;
            }
            out.push((line, Token::Other));
        } else if c.is_ascii_digit() {
            let start = i;
            while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                i += 1;
            }
            let is_float = s.get(i) == Some(&'.') && s.get(i + 1).is_some_and(char::is_ascii_digit);
            let text: String = s[start..i].iter().filter(|&&c| c != '_').collect();
            out.push((
                line,
                if is_float {
                    Token::Other
                } else {
                    int(&text).map_or(Token::Other, Token::Int)
                },
            ));
        } else if c.is_alphabetic() || c == '_' {
            while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                i += 1;
            }
            out.push((line, Token::Other));
        } else if at(i, "<<=") || at(i, "*=") {
            i += if c == '<' { 3 } else { 2 };
            out.push((line, Token::Other));
        } else if at(i, "<<") {
            i += 2;
            out.push((line, Token::Op("<<")));
        } else if c == '*' {
            i += 1;
            out.push((line, Token::Op("*")));
        } else {
            i += 1;
            out.push((line, Token::Other));
        }
    }
    out
}

/// Parses an integer literal without underscores, with an optional type suffix.
fn int(text: &str) -> Option<u64> {
    let (radix, digits) = match text.get(..2) {
        Some("0x") => (16, &text[2..]),
        Some("0o") => (8, &text[2..]),
        Some("0b") => (2, &text[2..]),
        _ => (10, text),
    };
    let end = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    let suffix = &digits[end..];
    let suffixes = [
        "", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
    ];
    if !suffixes.contains(&suffix) {
        return None;
    }
    u64::from_str_radix(&digits[..end], radix).ok()
}

/// Values of every literal and every `*` or `<<` chain of literals, with lines.
fn literal_values(source: &str) -> Vec<(usize, u64)> {
    let tokens = tokens(source);
    let mut values = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let Token::Int(first) = tokens[i].1 else {
            i += 1;
            continue;
        };
        values.push((tokens[i].0, first));
        let mut value = Some(first);
        let mut j = i + 1;
        while let (Some((_, Token::Op(op))), Some((line, Token::Int(next)))) =
            (tokens.get(j), tokens.get(j + 1))
        {
            values.push((*line, *next));
            value = value.and_then(|v| match *op {
                "*" => v.checked_mul(*next),
                _ => u32::try_from(*next).ok().and_then(|n| v.checked_shl(n)),
            });
            if let Some(v) = value {
                values.push((*line, v));
            }
            j += 2;
            // Chains mix operators of different precedence rarely; stop at a change.
            if tokens.get(j).is_some_and(|t| t.1 != Token::Op(op)) {
                break;
            }
        }
        i = j.max(i + 1);
    }
    values
}

fn limits() -> Vec<(&'static str, u64)> {
    FORMAT
        .entries()
        .into_iter()
        .chain(PROCESS_CONFIG_DEFAULT.entries())
        .filter(|(_, v)| *v >= CHECKED_FROM)
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_limit_is_a_literal_outside_the_constants_module() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(
        files.iter().any(|f| f.ends_with(MODULE)),
        "the scan reaches the module"
    );
    let limits = limits();
    let mut found = Vec::new();
    for file in files.iter().filter(|f| !f.ends_with(MODULE)) {
        let source = fs::read_to_string(file).unwrap();
        for (line, value) in literal_values(&source) {
            for (name, _) in limits.iter().filter(|(_, v)| *v == value) {
                found.push(format!(
                    "{}:{line}: {value} is `{name}`",
                    file.strip_prefix(&root).unwrap().display()
                ));
            }
        }
    }
    assert!(
        found.is_empty(),
        "use the constant from {MODULE} instead of the literal:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_scanner_finds_every_spelling() {
    let target = FORMAT.sector_size; // spelled 4096 in every source below
    let flagged = |source: &str| literal_values(source).iter().any(|(_, v)| *v == target);
    for source in [
        "let x = 4096;",
        "4_096",
        "0x1000",
        "0x10_00",
        "0o10000",
        "4096u64",
        "4 * 1024",
        "1 << 12",
        "[0u8; 4096]",
    ] {
        assert!(flagged(source), "{source}");
    }
    for source in [
        "// 4096",
        "/* 4096 /* nested */ 4096 */",
        "\"4096\"",
        "r#\"4096 \" 4096\"#",
        "b\"4096\"",
        "let s = \"\\\" 4096\";",
        "4096.0",
        "4 + 1024",
        "x * 4",
    ] {
        assert!(!flagged(source), "{source}");
    }
    assert_eq!(
        literal_values("fn f<'a>(x: &'a u8) -> char {\n'x'\n}\n0x1000"),
        [(4, target)]
    );
    assert_eq!(literal_values("let c = '\\'';\n4096"), [(2, target)]);
}
