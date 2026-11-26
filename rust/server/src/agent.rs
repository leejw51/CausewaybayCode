//! The agent: one conversation with the model, kept in its KV cache so that each step reads
//! only what is new, and the loop that runs the tools the model calls until it answers.

use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::Result;

use crate::inkling;
use crate::llm::{Engine, Stats};
use crate::prompt::{self, Call, ImageRef, Message, Parser, Piece, ToolSpec};
use crate::sandbox::Policy;
use crate::store::Store;
use crate::tools::{self, Ctx};
use crate::ui::Ui;

/// Set to stop what the model or a command is doing (the user's interrupt, or a client that
/// went away). One job runs at a time, so one flag is enough; a job clears it when it starts.
pub static INTERRUPT: AtomicBool = AtomicBool::new(false);

pub fn interrupted() -> bool {
    INTERRUPT.load(Ordering::Relaxed)
}

pub fn interrupt() {
    INTERRUPT.store(true, Ordering::Relaxed);
}

pub fn clear_interrupt() {
    INTERRUPT.store(false, Ordering::Relaxed);
}

/// One turn of a scripted model: what it "thinks", says and calls.
#[derive(Debug, Clone, Default)]
pub struct Turn {
    pub thinking: String,
    pub text: String,
    pub calls: Vec<Call>,
}

/// What writes the model's turns.
#[derive(Clone)]
pub enum Brain {
    /// Inkling 2 Mini on MLX.
    Model(Rc<Engine>),
    /// Turns read from a script instead of a model: for tests of everything around the model
    /// (transport, approvals, tools, storage) without loading 20 GB.
    Script(Rc<std::cell::RefCell<VecDeque<Turn>>>),
}

impl Brain {
    pub fn engine(&self) -> Option<&Engine> {
        match self {
            Self::Model(engine) => Some(engine),
            Self::Script(_) => None,
        }
    }

    /// A script from JSON: [{"thinking": "...", "text": "...", "calls": [{"name": "...", "args": {...}}]}, ...]
    pub fn script(json: &str) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_str(json)?;
        let turns = value
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("a script is a JSON array of turns"))?
            .iter()
            .map(|t| Turn {
                thinking: t["thinking"].as_str().unwrap_or("").to_string(),
                text: t["text"].as_str().unwrap_or("").to_string(),
                calls: t["calls"].as_array().map(|c| c.iter().map(|c| Call { name: c["name"].as_str().unwrap_or("").to_string(), args: c["args"].clone(), error: None }).collect()).unwrap_or_default(),
            })
            .collect();
        Ok(Self::Script(Rc::new(std::cell::RefCell::new(turns))))
    }
}

pub struct Options {
    /// The thinking effort told to the model ("0" to "0.99").
    pub effort: String,
    /// Most tokens the model writes in one step (thinking included).
    pub max_tokens: usize,
    /// Most model steps for one message of the user.
    pub max_steps: usize,
    /// Tokens in the cache from which the conversation is summarized.
    pub context_limit: usize,
}

/// What a turn used.
#[derive(Default)]
pub struct Usage {
    pub steps: usize,
    pub tool_calls: usize,
    pub generated: usize,
    pub gen_seconds: f64,
    pub read: usize,
    /// Seconds spent reading prompts (what `read` took).
    pub read_seconds: f64,
    pub seconds: f64,
}

pub struct Agent {
    pub brain: Brain,
    /// The project's store.
    pub store: Store,
    /// The server's store (the tools made at runtime).
    pub global: Rc<Store>,
    pub policy: Policy,
    pub opts: Options,
    cache: Option<inkling::Cache>,
    pub messages: Vec<Message>,
    /// Messages already in the cache.
    fed: usize,
    /// The start of the conversation (tools, system prompt) is in the cache.
    started: bool,
    /// What closes the model's last turn, when it did not end it itself.
    close: Vec<Piece>,
    /// The tools declared to the model in this cache.
    pub tools: Vec<ToolSpec>,
    pub session: i64,
}

const SUMMARY_REQUEST: &str = "The conversation is getting long and will now be replaced by a summary. Write that summary: \
the user's goals and instructions, what has been done (files read or changed, commands run and what they showed), \
facts and decisions worth keeping, and what remains to be done next. Be specific: file paths, names, numbers. \
Do not call tools. Write only the summary.";

fn today() -> String {
    // SAFETY: time and localtime_r only write the values passed to them.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
    }
}

/// The project's own instructions, if it has a file of them.
fn project_instructions(workspace: &Path) -> Option<(String, String)> {
    ["CWBCODE.md", "AGENTS.md", "CLAUDE.md"].iter().find_map(|name| {
        let text = std::fs::read_to_string(workspace.join(name)).ok()?;
        (!text.trim().is_empty()).then(|| (name.to_string(), tools::clip(text.trim(), 6000)))
    })
}

pub fn system_prompt(policy: &Policy) -> String {
    let ws = &policy.workspace;
    let mut s = format!(
        "You are Causewaybay Code (CC for short; the program is cwbcode), an AI agent running entirely on the user's Mac: a coding agent and a general assistant. \
You work in a terminal, in the user's workspace folder, with the tools declared above.

# How to work
- Do the task with your tools instead of describing what could be done. Look before you answer: read the files, list the folder, search the code, run the command.
- For a coding task: find the relevant code first (glob, grep, read_file), make the change with edit_file or write_file, then check it (build, run the tests) with bash, and fix what fails. Follow the conventions of the code around you.
- Change files with edit_file and write_file, not with shell commands (no sed, no redirection, no scripts that rewrite files). Read a file before editing it: edit_file needs the exact text of the file.
- For questions that need no tool (explanations, writing, math, general knowledge), just answer.
- You can call several tools in one step when they do not depend on each other.
- For work of three steps or more, keep a task list with todo_write: write the steps first, mark each in_progress when you start it and completed when it is done.
- When a tool fails, read the error and try another way; do not repeat the same call. If you cannot do something, say so plainly.
- Be concise. When the work is done, say briefly what you did and what the user should know. Answer in the language the user writes in.

# Making tools
When a task needs something your tools cannot do well, or a step you will repeat (parsing a format, a calculation, a batch change over many files, a report), make a tool for it with make_tool: you write it in Rust, it is compiled and saved, and you call it with run_tool, now and in later sessions. Prefer a small tool with clear arguments. If it fails to compile, read the compiler's message and fix the source.

# Memory
You have a knowledge base for this project (SQLite, searched by keywords and meaning together). When the user asks about something that may have been saved earlier (their notes, preferences, past decisions, saved files or images), use memory_search first. When the user asks you to remember or save something, use memory_save or memory_save_file. Entries are fetched in full with memory_get.

# Safety
Everything runs locally: you have no web access. {} Your file tools change files only inside the workspace. {}. Never try to work around a refusal; tell the user instead. Do not run destructive commands (deleting files you did not create, rewriting git history, discarding changes) unless the user asked for exactly that.

# Environment
- Workspace: {}
- Platform: macOS (Apple Silicon), shell zsh
- Date: {}",
        tools::confinement(policy),
        match policy.mode {
            crate::sandbox::Mode::Ask => "The user approves each edit and command before it happens",
            crate::sandbox::Mode::Auto => "The user has allowed edits and commands in the workspace without asking",
            crate::sandbox::Mode::ReadOnly => "This session is read-only: you can read and search, but not write files or run commands",
            crate::sandbox::Mode::Plan => "This session is in plan mode: look into the request with the read and search tools, then answer with a short numbered plan (which files, what changes in each, how you will check the result) and stop. Do not write files or run commands: the user reads the plan and switches mode to have it carried out",
        },
        ws.display(),
        today(),
    );
    if ws.join(".git").exists() {
        s.push_str("\n- The workspace is a git repository");
    }
    if let Some((name, text)) = project_instructions(ws) {
        s.push_str(&format!("\n\n# Project instructions ({name})\n{text}"));
    }
    s
}

/// Ask the model one thing in a conversation of its own and return its answer.
fn oneshot(engine: &Engine, system: &str, image: Option<&Path>, user: &str, max_tokens: usize) -> Result<String> {
    let mut cache = engine.make_cache();
    let mut pieces = prompt::start(&[], system, "0");
    let message = Message::User { text: user.to_string(), images: image.iter().map(|_| ImageRef { blob: 0, name: String::new() }).collect() };
    pieces.extend(prompt::render(&message, &mut |_| engine.model.attach_image(&mut cache, image.expect("an image message has an image")))?);
    let ids = engine.encode(&pieces)?;
    let mut parser = Parser::new();
    engine.generate(&ids, &mut cache, max_tokens, |token| {
        parser.push(engine, token, &mut |_| {});
        !interrupted()
    })?;
    parser.close(engine, &mut |_| {});
    Ok(parser.text.trim().to_string())
}

/// A description of an image, by which the knowledge base finds it.
pub fn describe_image(engine: &Engine, path: &Path) -> Result<String> {
    oneshot(
        engine,
        "",
        Some(path),
        "Describe this image for a search index, in one paragraph: what it shows, any text in it (quoted), notable objects, people, colors, the setting, and what kind of image it is (photo, screenshot, chart, diagram, drawing).",
        400,
    )
}

impl Agent {
    /// An agent for one conversation of a project: a new one (`session` None) or a saved one.
    pub fn new(brain: Brain, store: Store, global: Rc<Store>, policy: Policy, opts: Options, session: Option<i64>) -> Result<Self> {
        let cache = brain.engine().map(|e| e.make_cache());
        let (session, messages) = match session {
            Some(id) => (id, store.session_messages(id)?),
            None => (store.new_session(&policy.workspace.to_string_lossy())?, Vec::new()),
        };
        let tools = tools::specs(&global, &policy);
        Ok(Self { brain, store, global, policy, opts, cache, messages, fed: 0, started: false, close: Vec::new(), tools, session })
    }

    /// Tokens in the model's cache.
    pub fn tokens(&self) -> usize {
        self.cache.as_ref().map_or(0, |c| c.len())
    }

    /// Forget the cache; the conversation so far is read again with the next step. Also
    /// what brings newly made tools, a new mode or a new thinking effort into the prompt.
    pub fn rebuild(&mut self) {
        self.cache = self.brain.engine().map(|e| e.make_cache());
        self.fed = 0;
        self.started = false;
        self.close.clear();
        self.tools = tools::specs(&self.global, &self.policy);
    }

    fn push(&mut self, message: Message) -> Result<()> {
        self.store.append_message(self.session, &message)?;
        self.messages.push(message);
        Ok(())
    }

    /// The prompt for the next step: everything that is not in the cache yet.
    fn feed(&mut self, engine: &Engine) -> Result<Vec<Piece>> {
        let mut pieces = if self.started { std::mem::take(&mut self.close) } else { prompt::start(&self.tools, &system_prompt(&self.policy), &self.opts.effort) };
        let Self { messages, store, cache, fed, .. } = self;
        let cache = cache.as_mut().expect("a model has a cache");
        for message in &messages[*fed..] {
            pieces.extend(prompt::render(message, &mut |image| engine.model.attach_image(cache, &store.blob_file(image.blob)?))?);
        }
        Ok(pieces)
    }

    /// One step: the model reads what is new and writes its turn.
    fn step(&mut self, ui: &mut Ui) -> Result<(Parser, Stats)> {
        let brain = self.brain.clone();
        let result = match &brain {
            Brain::Model(engine) => (|| -> Result<(Parser, Stats)> {
                let pieces = self.feed(engine)?;
                let ids = engine.encode(&pieces)?;
                let mut parser = Parser::new();
                let cache = self.cache.as_mut().expect("a model has a cache");
                let stats = engine.generate(&ids, cache, self.opts.max_tokens, |token| {
                    parser.push(&**engine, token, &mut |event| ui.event(event));
                    !interrupted()
                })?;
                parser.close(&**engine, &mut |event| ui.event(event));
                Ok((parser, stats))
            })(),
            Brain::Script(turns) => {
                let turn = turns.borrow_mut().pop_front().unwrap_or_else(|| Turn { text: "(the script has no more turns)".into(), ..Turn::default() });
                for (text, thinking) in [(&turn.thinking, true), (&turn.text, false)] {
                    // In pieces, as a model would write it.
                    for piece in text.split_inclusive(' ') {
                        ui.event(if thinking { prompt::Event::Thinking(piece) } else { prompt::Event::Text(piece) });
                    }
                    if !text.is_empty() {
                        ui.event(prompt::Event::End);
                    }
                }
                let generated = (turn.thinking.len() + turn.text.len()) / 4 + 1;
                Ok((Parser::scripted(turn.thinking, turn.text, turn.calls), Stats { gen_tokens: generated, gen_tps: 100.0, eos_in_cache: true, ..Stats::default() }))
            }
        };
        match result {
            Ok((parser, stats)) => {
                self.started = true;
                self.fed = self.messages.len();
                self.close = prompt::close_turn(stats.eos_in_cache, parser.at_end_message);
                Ok((parser, stats))
            }
            Err(e) => {
                // The cache may hold half a prompt: start it over.
                self.rebuild();
                Err(e)
            }
        }
    }

    /// Send the user's message and work until the model answers without calling a tool.
    pub fn send(&mut self, text: &str, images: Vec<ImageRef>, ui: &mut Ui) -> Result<Usage> {
        let start = Instant::now();
        let before = self.messages.len();
        self.push(Message::User { text: text.to_string(), images })?;
        let mut usage = Usage::default();
        let mut summarized = 0;
        for step in 0..self.opts.max_steps {
            if self.tokens() + self.unfed_estimate() > self.opts.context_limit && self.started {
                // Summarizing again and again within one message would go in circles.
                if summarized == 2 {
                    ui.warn("[the context is full again right after summarizing; stopping here. Start over with /clear, or allow more with --context]");
                    break;
                }
                summarized += 1;
                ui.note(&format!("[{} tokens in context: summarizing the conversation to make room]", self.tokens()));
                self.compact(ui)?;
            }
            let (parser, stats) = match self.step(ui) {
                Ok(done) => done,
                Err(e) => {
                    if step == 0 {
                        // A message that cannot be read (a broken image, say) is taken back.
                        self.messages.truncate(before);
                        self.store.replace_messages(self.session, &self.messages)?;
                    }
                    return Err(e);
                }
            };
            usage.steps += 1;
            usage.read += stats.prompt_tokens;
            usage.read_seconds += if stats.prompt_tps > 0.0 { stats.prompt_tokens as f64 / stats.prompt_tps } else { 0.0 };
            usage.generated += stats.gen_tokens;
            usage.gen_seconds += if stats.gen_tps > 0.0 { stats.gen_tokens.saturating_sub(1) as f64 / stats.gen_tps } else { 0.0 };
            let stopped = interrupted();
            // Calls of a turn that was interrupted are not run.
            let calls: Vec<Call> = if stopped { vec![] } else { parser.calls.clone() };
            self.push(Message::Assistant { thinking: parser.thinking, text: parser.text, calls: calls.clone() })?;
            self.fed = self.messages.len();
            if stopped {
                ui.warn("[interrupted]");
                break;
            }
            if calls.is_empty() {
                if !stats.eos_in_cache {
                    ui.warn(&format!("[stopped at {} tokens for one step; say \"continue\" to go on, or raise --max-tokens]", self.opts.max_tokens));
                }
                break;
            }
            for call in &calls {
                usage.tool_calls += 1;
                let output = if interrupted() {
                    tools::ToolOutput { text: "Not run: the user interrupted.".into(), images: vec![], ok: false }
                } else {
                    ui.tool_start(call);
                    let t = Instant::now();
                    let mut ctx = Ctx { policy: &mut self.policy, store: &self.store, global: &self.global, session: self.session, events: Vec::new(), engine: self.brain.engine(), stop: &interrupted };
                    let output = tools::execute(&mut ctx, call);
                    let events = std::mem::take(&mut ctx.events);
                    ui.tool_end(&output, t.elapsed().as_secs_f64());
                    for event in events {
                        ui.emit(event);
                    }
                    output
                };
                self.push(Message::Tool { name: call.name.clone(), text: output.text, images: output.images })?;
            }
            if interrupted() {
                ui.warn("[interrupted]");
                break;
            }
            if step + 1 == self.opts.max_steps {
                ui.warn(&format!("[stopped after {} steps; say \"continue\" to go on]", self.opts.max_steps));
            }
        }
        usage.seconds = start.elapsed().as_secs_f64();
        Ok(usage)
    }

    /// A rough count of the tokens of the messages not yet in the cache.
    fn unfed_estimate(&self) -> usize {
        self.messages[self.fed.min(self.messages.len())..]
            .iter()
            .map(|m| match m {
                Message::System(t) => t.len() / 3,
                Message::User { text, images } | Message::Tool { text, images, .. } => text.len() / 3 + images.len() * 700,
                Message::Assistant { text, .. } => text.len() / 3,
            })
            .sum()
    }

    /// Replace the conversation by the model's summary of it.
    pub fn compact(&mut self, ui: &mut Ui) -> Result<()> {
        // What the user has just said and the model has not read yet is not history: it stays
        // as it is, after the summary. (Unread tool results are read and summarized.)
        let mut keep_from = self.messages.len();
        while keep_from > self.fed && matches!(self.messages[keep_from - 1], Message::User { .. }) {
            keep_from -= 1;
        }
        let tail = self.messages.split_off(keep_from);
        let kept = self.messages.len();
        if kept == 0 {
            self.messages = tail;
            return Ok(());
        }
        // The request and its answer are not part of the saved conversation.
        self.messages.push(Message::User { text: SUMMARY_REQUEST.to_string(), images: vec![] });
        let result = self.step(&mut Ui::muted());
        self.messages.truncate(kept);
        let summary = match &result {
            Ok((parser, _)) if !parser.text.trim().is_empty() => parser.text.trim().to_string(),
            Ok((parser, _)) => parser.thinking.trim().to_string(),
            Err(_) => String::new(),
        };
        if summary.is_empty() || interrupted() {
            self.messages.extend(tail);
            self.rebuild();
            result?;
            anyhow::bail!("the conversation could not be summarized; it is unchanged");
        }
        self.messages = vec![Message::User { text: format!("[The earlier conversation was replaced by this summary to save memory.]\n\n{summary}\n\n[End of the summary.]"), images: vec![] }];
        self.messages.extend(tail);
        self.store.replace_messages(self.session, &self.messages)?;
        self.rebuild();
        ui.note(&format!("[conversation summarized: {} messages became {} characters]", kept, summary.chars().count()));
        Ok(())
    }
}
