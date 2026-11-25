//! Tools the agent makes for itself while it runs. A tool is one Rust source file that
//! defines
//!
//!     fn run(args: &Json) -> Result<String, String>
//!
//! using only the standard library. cwbcode adds a small JSON type and a `main`, compiles
//! the file with `rustc` and keeps the program; the source is saved in the store, so a tool
//! made once is there in every later session. Compiling and running both happen inside the
//! sandbox (no network, writes only in the workspace).

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::sandbox::{Policy, run};
use crate::store::{SavedTool, Store};

/// Appended to every tool's source: the `Json` type the tool takes and the program's `main`.
pub const RUNTIME: &str = r#"
// ---- added by cwbcode ----
pub use cwb_runtime::Json;
mod cwb_runtime {
    #[derive(Debug, Clone, PartialEq)]
    pub enum Json { Null, Bool(bool), Num(f64), Str(String), Arr(Vec<Json>), Obj(Vec<(String, Json)>) }

    impl Json {
        pub fn get(&self, key: &str) -> Option<&Json> {
            match self { Json::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v).filter(|v| **v != Json::Null), _ => None }
        }
        pub fn as_str(&self) -> Option<&str> { match self { Json::Str(s) => Some(s), _ => None } }
        pub fn as_f64(&self) -> Option<f64> { match self { Json::Num(n) => Some(*n), Json::Str(s) => s.trim().parse().ok(), _ => None } }
        pub fn as_i64(&self) -> Option<i64> { self.as_f64().map(|n| n as i64) }
        pub fn as_bool(&self) -> Option<bool> { match self { Json::Bool(b) => Some(*b), _ => None } }
        pub fn as_array(&self) -> Option<&[Json]> { match self { Json::Arr(a) => Some(a), _ => None } }
        pub fn as_object(&self) -> Option<&[(String, Json)]> { match self { Json::Obj(m) => Some(m), _ => None } }
        /// A required string argument.
        pub fn str(&self, key: &str) -> Result<&str, String> {
            self.get(key).and_then(Json::as_str).ok_or_else(|| format!("missing string argument \"{key}\""))
        }
        /// A required number argument.
        pub fn num(&self, key: &str) -> Result<f64, String> {
            self.get(key).and_then(Json::as_f64).ok_or_else(|| format!("missing number argument \"{key}\""))
        }
        pub fn int(&self, key: &str) -> Result<i64, String> { self.num(key).map(|n| n as i64) }
        /// An optional boolean argument (false when absent).
        pub fn flag(&self, key: &str) -> bool { self.get(key).and_then(Json::as_bool).unwrap_or(false) }
        pub fn opt_str(&self, key: &str) -> Option<&str> { self.get(key).and_then(Json::as_str) }
        pub fn opt_num(&self, key: &str) -> Option<f64> { self.get(key).and_then(Json::as_f64) }
        /// A required array argument.
        pub fn array(&self, key: &str) -> Result<&[Json], String> {
            self.get(key).and_then(Json::as_array).ok_or_else(|| format!("missing array argument \"{key}\""))
        }

        pub fn parse(text: &str) -> Result<Json, String> {
            let chars: Vec<char> = text.chars().collect();
            let mut at = 0;
            let value = parse_value(&chars, &mut at, 0)?;
            skip(&chars, &mut at);
            if at != chars.len() { return Err(format!("unexpected text after the JSON value at character {at}")); }
            Ok(value)
        }
    }

    impl std::fmt::Display for Json {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            match self {
                Json::Null => write!(f, "null"),
                Json::Bool(b) => write!(f, "{b}"),
                Json::Num(n) => write!(f, "{n}"),
                Json::Str(s) => write_str(f, s),
                Json::Arr(a) => {
                    write!(f, "[")?;
                    for (i, v) in a.iter().enumerate() { if i > 0 { write!(f, ",")?; } write!(f, "{v}")?; }
                    write!(f, "]")
                }
                Json::Obj(m) => {
                    write!(f, "{{")?;
                    for (i, (k, v)) in m.iter().enumerate() { if i > 0 { write!(f, ",")?; } write_str(f, k)?; write!(f, ":{v}")?; }
                    write!(f, "}}")
                }
            }
        }
    }

    fn write_str(f: &mut std::fmt::Formatter, s: &str) -> std::fmt::Result {
        write!(f, "\"")?;
        for c in s.chars() {
            match c {
                '"' => write!(f, "\\\"")?,
                '\\' => write!(f, "\\\\")?,
                '\n' => write!(f, "\\n")?,
                '\r' => write!(f, "\\r")?,
                '\t' => write!(f, "\\t")?,
                c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
                c => write!(f, "{c}")?,
            }
        }
        write!(f, "\"")
    }

    fn skip(c: &[char], at: &mut usize) { while *at < c.len() && c[*at].is_whitespace() { *at += 1; } }

    fn parse_value(c: &[char], at: &mut usize, depth: usize) -> Result<Json, String> {
        if depth > 200 { return Err("JSON nested too deeply".into()); }
        skip(c, at);
        match c.get(*at) {
            None => Err("unexpected end of JSON".into()),
            Some('{') => {
                *at += 1;
                let mut members = Vec::new();
                loop {
                    skip(c, at);
                    if c.get(*at) == Some(&'}') { *at += 1; break; }
                    if !members.is_empty() { expect(c, at, ',')?; skip(c, at); }
                    let key = parse_string(c, at)?;
                    skip(c, at);
                    expect(c, at, ':')?;
                    members.push((key, parse_value(c, at, depth + 1)?));
                }
                Ok(Json::Obj(members))
            }
            Some('[') => {
                *at += 1;
                let mut items = Vec::new();
                loop {
                    skip(c, at);
                    if c.get(*at) == Some(&']') { *at += 1; break; }
                    if !items.is_empty() { expect(c, at, ',')?; }
                    items.push(parse_value(c, at, depth + 1)?);
                }
                Ok(Json::Arr(items))
            }
            Some('"') => parse_string(c, at).map(Json::Str),
            Some(_) => {
                let start = *at;
                while *at < c.len() && !matches!(c[*at], ',' | '}' | ']') && !c[*at].is_whitespace() { *at += 1; }
                let word: String = c[start..*at].iter().collect();
                match word.as_str() {
                    "null" => Ok(Json::Null),
                    "true" => Ok(Json::Bool(true)),
                    "false" => Ok(Json::Bool(false)),
                    w => w.parse().map(Json::Num).map_err(|_| format!("bad JSON value {w:?}")),
                }
            }
        }
    }

    fn expect(c: &[char], at: &mut usize, want: char) -> Result<(), String> {
        if c.get(*at) == Some(&want) { *at += 1; Ok(()) } else { Err(format!("expected '{want}' at character {at}")) }
    }

    fn parse_string(c: &[char], at: &mut usize) -> Result<String, String> {
        expect(c, at, '"')?;
        let mut out = String::new();
        loop {
            let ch = *c.get(*at).ok_or("unterminated JSON string")?;
            *at += 1;
            match ch {
                '"' => return Ok(out),
                '\\' => {
                    let esc = *c.get(*at).ok_or("unterminated JSON string")?;
                    *at += 1;
                    match esc {
                        'n' => out.push('\n'), 't' => out.push('\t'), 'r' => out.push('\r'),
                        'b' => out.push('\u{8}'), 'f' => out.push('\u{c}'),
                        'u' => {
                            let mut code = hex4(c, at)?;
                            if (0xD800..0xDC00).contains(&code) && c.get(*at) == Some(&'\\') && c.get(*at + 1) == Some(&'u') {
                                *at += 2;
                                let low = hex4(c, at)?;
                                code = 0x10000 + ((code - 0xD800) << 10) + (low.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
    }

    fn hex4(c: &[char], at: &mut usize) -> Result<u32, String> {
        if *at + 4 > c.len() { return Err("bad \\u escape".into()); }
        let s: String = c[*at..*at + 4].iter().collect();
        *at += 4;
        u32::from_str_radix(&s, 16).map_err(|_| "bad \\u escape".to_string())
    }
}

fn main() {
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).expect("reading the arguments");
    let args = if input.trim().is_empty() { Ok(Json::Obj(Vec::new())) } else { Json::parse(&input) };
    let result = args.and_then(|a| run(&a));
    match result {
        Ok(text) => print!("{text}"),
        Err(error) => {
            eprint!("{error}");
            std::process::exit(1);
        }
    }
}
"#;

/// What the model is told about writing a tool.
pub const CONTRACT: &str = "The source is one Rust file (edition 2021, standard library only, no external crates) that defines \
`fn run(args: &Json) -> Result<String, String>`: the returned String is the tool's result, an Err its error message. \
Do not write `fn main` and do not define `Json`; both are added. \
`Json` is `enum Json { Null, Bool(bool), Num(f64), Str(String), Arr(Vec<Json>), Obj(Vec<(String, Json)>) }` with: \
args.str(\"key\")? -> &str, args.num(\"key\")? -> f64, args.int(\"key\")? -> i64, args.flag(\"key\") -> bool, \
args.opt_str(\"key\") -> Option<&str>, args.opt_num(\"key\") -> Option<f64>, args.array(\"key\")? -> &[Json], args.get(\"key\") -> Option<&Json>, \
value.as_str(), .as_f64(), .as_i64(), .as_bool(), .as_array(), .as_object(), Json::parse(text) -> Result<Json, String>, and Display (prints JSON). \
The tool runs in the workspace folder, confined like the bash tool's commands. \
It may use std::fs and std::process::Command.";

pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase()) && name.len() <= 40 && name.len() >= 2 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn rustc() -> Result<PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    path.split(':')
        .map(PathBuf::from)
        .chain([PathBuf::from(&home).join(".cargo/bin"), PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")])
        .map(|dir| dir.join("rustc"))
        .find(|p| p.is_file())
        .context("rustc was not found: install Rust (https://rustup.rs) to make tools")
}

/// The compiled program for a tool, compiling it if this source has not been compiled yet.
/// A compile error comes back as the error text, for the model to fix.
pub fn build(policy: &Policy, name: &str, source: &str) -> Result<PathBuf> {
    if source.contains("fn main(") {
        bail!("do not write `fn main`: define `fn run(args: &Json) -> Result<String, String>` only");
    }
    let dir = policy.home.join("tools");
    std::fs::create_dir_all(&dir)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (source, RUNTIME).hash(&mut hasher);
    let stem = format!("{name}-{:016x}", hasher.finish());
    let bin = dir.join(&stem);
    if bin.is_file() {
        return Ok(bin);
    }
    let src = dir.join(format!("{stem}.rs"));
    // The tool's own source comes first, so the compiler's line numbers are its line numbers.
    std::fs::write(&src, format!("{source}\n{RUNTIME}"))?;
    let rustc = rustc()?;
    let cmd = policy.command(&rustc, &["--edition", "2021", "-O", "-A", "warnings", "--crate-name", name, "-o", &bin.to_string_lossy(), &src.to_string_lossy()]);
    let out = run(cmd, None, Duration::from_secs(300), &|| false)?;
    if out.status != Some(0) || !bin.is_file() {
        let _ = std::fs::remove_file(&bin);
        let errors = out.stderr.replace(&format!("{}", src.display()), &format!("{name}.rs"));
        bail!("the tool did not compile:\n{}", errors.trim());
    }
    // Earlier builds of this tool are no longer needed.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if file.starts_with(&format!("{name}-")) && !file.starts_with(&stem) && file.len() == stem.len() + if file.ends_with(".rs") { 3 } else { 0 } {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(bin)
}

/// Compile a new tool and save it.
pub fn make(policy: &Policy, store: &Store, name: &str, description: &str, parameters: &Value, source: &str) -> Result<()> {
    if !valid_name(name) {
        bail!("a tool name is 2 to 40 characters: lowercase letters, digits and _, starting with a letter");
    }
    if !parameters.is_object() {
        bail!("\"parameters\" must be a JSON Schema object, for example {{\"type\":\"object\",\"properties\":{{\"path\":{{\"type\":\"string\"}}}},\"required\":[\"path\"]}}");
    }
    build(policy, name, source)?;
    store.save_tool(&SavedTool { name: name.to_string(), description: description.to_string(), parameters: parameters.to_string(), source: source.to_string() })
}

/// Run a saved tool with JSON arguments. Ok is its output; Err its error or a failure to run.
pub fn call(policy: &Policy, store: &Store, name: &str, args: &Value, stop: &dyn Fn() -> bool) -> Result<String> {
    let tool = store.tool(name)?.with_context(|| format!("there is no tool named \"{name}\" (see list_tools)"))?;
    let bin = build(policy, name, &tool.source)?;
    let out = run(policy.command(&bin, &[]), Some(args.to_string().as_bytes()), Duration::from_secs(120), stop)?;
    if out.interrupted {
        bail!("interrupted by the user");
    }
    if out.timed_out {
        bail!("the tool ran for more than 120 seconds and was stopped");
    }
    if out.status != Some(0) {
        let code = out.status.map_or("was killed".to_string(), |c| format!("exited with {c}"));
        bail!("the tool {code}: {}{}", out.stderr.trim(), if out.stdout.is_empty() { String::new() } else { format!("\n(output so far: {})", out.stdout.trim()) });
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::Mode;
    use serde_json::json;

    #[test]
    fn names_are_checked() {
        assert!(valid_name("word_count2"));
        for bad in ["", "a", "Bad", "2x", "has-dash", "sp ace", "../x", &"x".repeat(41)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_tool_is_compiled_saved_and_called_and_errors_come_back() {
        if rustc().is_err() {
            return;
        }
        let base = std::env::temp_dir().join(format!("cwbcode-dyntool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("ws")).unwrap();
        let policy = Policy::new(Mode::Auto, &base.join("ws"), &base.join("home")).unwrap();
        let store = Store::in_memory().unwrap();
        let source = r#"
fn run(args: &Json) -> Result<String, String> {
    let text = args.str("text")?;
    let times = args.opt_num("times").unwrap_or(1.0) as usize;
    if args.flag("fail") { return Err("asked to fail".into()); }
    let words: Vec<Json> = text.split_whitespace().map(|w| Json::Str(w.to_uppercase())).collect();
    Ok(format!("{} {}", words.len() * times, Json::Arr(words)))
}"#;
        let schema = json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]});
        make(&policy, &store, "shout", "Uppercase the words", &schema, source).unwrap();
        let go = |args: Value| call(&policy, &store, "shout", &args, &|| false);
        assert_eq!(go(json!({"text": "héllo \"wörld\"\n한글", "times": 2})).unwrap(), "6 [\"HÉLLO\",\"\\\"WÖRLD\\\"\",\"한글\"]");
        assert!(go(json!({})).unwrap_err().to_string().contains("missing string argument \"text\""));
        assert!(go(json!({"text": "x", "fail": true})).unwrap_err().to_string().contains("asked to fail"));
        assert!(call(&policy, &store, "nope", &json!({}), &|| false).is_err());
        // A source that does not compile is not saved, and the compiler's message names the line.
        let broken = make(&policy, &store, "broken", "", &schema, "fn run(args: &Json) -> Result<String, String> {\n    Ok(missing)\n}").unwrap_err().to_string();
        assert!(broken.contains("did not compile") && broken.contains("broken.rs:2"), "{broken}");
        assert!(store.tool("broken").unwrap().is_none());
        assert!(make(&policy, &store, "shout", "", &schema, "fn main() {}").is_err());
        // In the sandbox the tool cannot write outside the workspace.
        if policy.sandbox {
            let writer = "fn run(args: &Json) -> Result<String, String> { std::fs::write(args.str(\"path\")?, \"x\").map_err(|e| e.to_string())?; Ok(\"written\".into()) }";
            make(&policy, &store, "writer", "", &schema, writer).unwrap();
            assert_eq!(call(&policy, &store, "writer", &json!({"path": "in.txt"}), &|| false).unwrap(), "written");
            let outside = PathBuf::from(std::env::var("HOME").unwrap()).join(format!(".cwbcode-test-escape-{}.txt", std::process::id()));
            assert!(call(&policy, &store, "writer", &json!({"path": outside.to_string_lossy()}), &|| false).is_err());
            assert!(!outside.exists());
        }
    }
}
