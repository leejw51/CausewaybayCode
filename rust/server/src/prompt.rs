//! Inkling 2's chat format, as Ollama's `inkling2` renderer and parser define it:
//!
//!   <|message_system|>tool_declare<|content_xml|>[{"description":..,"name":..,"parameters":..,"type":"function"}]<|end_message|>
//!   <|message_system|><|content_text|>SYSTEM<|end_message|>
//!   <|message_system|><|content_text|>Thinking effort level: 0.7<|end_message|>
//!   <|message_user|><|content_text|>USER<|end_message|>
//!   <|message_model|><|content_thinking|>...<|end_message|>
//!   <|message_model|><|content_text|>...<|end_message|>
//!   <|message_model|>NAME<|content_invoke_tool_json|>{"name":"NAME","args":{..}}<|end_message|>
//!   <|content_model_end_sampling|>
//!   <|message_tool|>NAME<|content_text|>RESULT<|end_message|>
//!
//! A prompt is a list of pieces: control tokens, which only this file writes, and text, in
//! which nothing can become a control token (see `Engine::encode`).

use serde_json::{Map, Value, json};

use crate::inkling::vision::IMAGE_TOKEN;

#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Control(String),
    Text(String),
}

fn c(s: &str) -> Piece {
    Piece::Control(s.to_string())
}

fn t(s: &str) -> Piece {
    Piece::Text(s.to_string())
}

/// The pieces as one string, for tests and debugging.
pub fn flatten(pieces: &[Piece]) -> String {
    pieces.iter().map(|p| match p { Piece::Control(s) | Piece::Text(s) => s.as_str() }).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments object.
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: String,
    pub args: Value,
    /// Why the call could not be read (bad JSON); it is answered with this error.
    pub error: Option<String>,
}

/// An image shown to the model: a blob in the store (see store.rs).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRef {
    pub blob: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    System(String),
    User { text: String, images: Vec<ImageRef> },
    Assistant { thinking: String, text: String, calls: Vec<Call> },
    Tool { name: String, text: String, images: Vec<ImageRef> },
}

impl Message {
    pub fn to_json(&self) -> Value {
        let imgs = |v: &[ImageRef]| Value::Array(v.iter().map(|i| json!({"blob": i.blob, "name": i.name})).collect());
        match self {
            Self::System(s) => json!({"role": "system", "text": s}),
            Self::User { text, images } => json!({"role": "user", "text": text, "images": imgs(images)}),
            Self::Assistant { thinking, text, calls } => json!({
                "role": "assistant", "thinking": thinking, "text": text,
                "calls": calls.iter().map(|c| json!({"name": c.name, "args": c.args, "error": c.error})).collect::<Vec<_>>(),
            }),
            Self::Tool { name, text, images } => json!({"role": "tool", "name": name, "text": text, "images": imgs(images)}),
        }
    }

    pub fn from_json(v: &Value) -> Option<Self> {
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        let imgs = || -> Vec<ImageRef> {
            v["images"].as_array().map(|a| a.iter().filter_map(|i| Some(ImageRef { blob: i["blob"].as_i64()?, name: i["name"].as_str().unwrap_or("").into() })).collect()).unwrap_or_default()
        };
        Some(match v["role"].as_str()? {
            "system" => Self::System(s("text")),
            "user" => Self::User { text: s("text"), images: imgs() },
            "assistant" => Self::Assistant {
                thinking: s("thinking"),
                text: s("text"),
                calls: v["calls"].as_array().map(|a| a.iter().map(|c| Call { name: c["name"].as_str().unwrap_or("").into(), args: c["args"].clone(), error: c["error"].as_str().map(String::from) }).collect()).unwrap_or_default(),
            },
            "tool" => Self::Tool { name: s("name"), text: s("text"), images: imgs() },
            _ => return None,
        })
    }
}

/// Thinking effort names and the level the model is told.
pub fn effort_level(name: &str) -> Option<&'static str> {
    Some(match name {
        "none" | "off" | "false" => "0",
        "minimal" => "0.1",
        "low" => "0.2",
        "medium" => "0.7",
        "high" | "on" | "true" => "0.9",
        "max" | "xhigh" => "0.99",
        _ => return None,
    })
}

fn sort_keys(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|k| (k.clone(), sort_keys(&m[k]))).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(sort_keys).collect()),
        other => other.clone(),
    }
}

/// The tool declarations: compact JSON with sorted keys, as the model's template writes them.
pub fn declaration(tools: &[ToolSpec]) -> String {
    let specs: Vec<Value> = tools
        .iter()
        .map(|tool| sort_keys(&json!({"description": tool.description, "name": tool.name, "parameters": tool.parameters, "type": "function"})))
        .collect();
    Value::Array(specs).to_string()
}

/// The body of a tool call: argument names sorted, values as they are.
pub fn call_body(call: &Call) -> String {
    let mut args = Map::new();
    if let Value::Object(m) = &call.args {
        let mut keys: Vec<&String> = m.keys().collect();
        keys.sort();
        for k in keys {
            args.insert(k.clone(), m[k].clone());
        }
    }
    format!("{{\"name\":{},\"args\":{}}}", Value::String(call.name.clone()), Value::Object(args))
}

/// What opens a conversation: the tools, the system prompt and the thinking effort.
pub fn start(tools: &[ToolSpec], system: &str, effort: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    if !tools.is_empty() {
        out.extend(declare(tools));
    }
    if !system.is_empty() {
        out.extend([c("<|message_system|><|content_text|>"), t(system), c("<|end_message|>")]);
    }
    out.extend([c("<|message_system|><|content_text|>"), t(&format!("Thinking effort level: {effort}")), c("<|end_message|>")]);
    out
}

pub fn declare(tools: &[ToolSpec]) -> Vec<Piece> {
    vec![c("<|message_system|>"), t("tool_declare"), c("<|content_xml|>"), t(&declaration(tools)), c("<|end_message|>")]
}

/// One message. `image_tokens` gives the number of placeholder tokens of an image (encoding
/// and queueing it for the next forward); images are fed in the order they are asked for.
/// An assistant message is rendered without its thinking.
pub fn render(message: &Message, image_tokens: &mut dyn FnMut(&ImageRef) -> anyhow::Result<usize>) -> anyhow::Result<Vec<Piece>> {
    let mut out = Vec::new();
    let mut content = |out: &mut Vec<Piece>, role: &str, name: &str, text: &str, images: &[ImageRef]| -> anyhow::Result<()> {
        // An image is a message of its own, before the text.
        for image in images {
            let n = image_tokens(image)?;
            out.extend([c(role), t(name), c(&format!("<|content_image|>{}<|end_message|>", IMAGE_TOKEN.repeat(n)))]);
        }
        if !text.is_empty() || images.is_empty() {
            out.extend([c(role), t(name), c("<|content_text|>"), t(text), c("<|end_message|>")]);
        }
        Ok(())
    };
    match message {
        Message::System(text) => content(&mut out, "<|message_system|>", "", text, &[])?,
        Message::User { text, images } => content(&mut out, "<|message_user|>", "", text, images)?,
        Message::Tool { name, text, images } => content(&mut out, "<|message_tool|>", name, text, images)?,
        Message::Assistant { text, calls, .. } => {
            if !text.is_empty() || calls.is_empty() {
                out.extend([c("<|message_model|><|content_text|>"), t(text), c("<|end_message|>")]);
            }
            for call in calls {
                out.extend([c("<|message_model|>"), t(&call.name), c("<|content_invoke_tool_json|>"), t(&call_body(call)), c("<|end_message|>")]);
            }
            out.push(c("<|content_model_end_sampling|>"));
        }
    }
    Ok(out)
}

/// What closes a model turn that did not end by itself (interrupted, or out of tokens).
pub fn close_turn(ended: bool, last_was_end_message: bool) -> Vec<Piece> {
    match (ended, last_was_end_message) {
        (true, _) => vec![],
        (false, true) => vec![c("<|content_model_end_sampling|>")],
        (false, false) => vec![c("<|end_message|><|content_model_end_sampling|>")],
    }
}

// ---------- reading the model's turn ----------

/// What the parser needs from the tokenizer.
pub trait Vocab {
    fn decode(&self, ids: &[u32]) -> String;
    /// "<|message_model|>" and the like for a control token's id.
    fn control(&self, id: u32) -> Option<&'static str>;
}

impl Vocab for crate::llm::Engine {
    fn decode(&self, ids: &[u32]) -> String {
        crate::llm::Engine::decode(self, ids)
    }

    fn control(&self, id: u32) -> Option<&'static str> {
        let s = &self.special;
        Some(match id {
            x if x == s.message_model => "<|message_model|>",
            x if x == s.thinking => "<|content_thinking|>",
            x if x == s.text => "<|content_text|>",
            x if x == s.tool_json => "<|content_invoke_tool_json|>",
            x if x == s.tool_text => "<|content_invoke_tool_text|>",
            x if x == s.tool_error => "<|content_tool_error|>",
            x if x == s.end_message => "<|end_message|>",
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    /// Between messages; text here is taken as an answer.
    Boundary,
    /// After <|message_model|>: the tool's name, if this message calls one.
    Header,
    Thinking,
    Text,
    ToolJson,
    ToolText,
    /// A frame that is not shown (the model's own tool-error frame).
    Hidden,
}

pub enum Event<'a> {
    Thinking(&'a str),
    Text(&'a str),
    /// A thinking or text message ended.
    End,
}

/// Reads a model turn token by token: thinking, answer text and tool calls.
pub struct Parser {
    mode: Mode,
    frame: Vec<u32>,
    shown: usize,
    name: String,
    pub thinking: String,
    pub text: String,
    pub calls: Vec<Call>,
    /// The last token was <|end_message|>.
    pub at_end_message: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self { mode: Mode::Boundary, frame: Vec::new(), shown: 0, name: String::new(), thinking: String::new(), text: String::new(), calls: Vec::new(), at_end_message: false }
    }

    /// A turn that was not read from tokens (a scripted model's).
    pub fn scripted(thinking: String, text: String, calls: Vec<Call>) -> Self {
        Self { thinking, text, calls, at_end_message: true, ..Self::new() }
    }

    pub fn push(&mut self, vocab: &dyn Vocab, token: u32, on: &mut dyn FnMut(Event)) {
        let control = vocab.control(token);
        self.at_end_message = control == Some("<|end_message|>");
        match control {
            Some("<|message_model|>") => {
                self.close(vocab, on);
                self.mode = Mode::Header;
            }
            Some("<|end_message|>") => {
                self.close(vocab, on);
                self.mode = Mode::Boundary;
            }
            Some(marker) => {
                if self.mode == Mode::Header {
                    self.name = vocab.decode(&self.frame).trim().to_string();
                    self.frame.clear();
                } else {
                    self.close(vocab, on);
                    self.name.clear();
                }
                self.shown = 0;
                self.mode = match marker {
                    "<|content_thinking|>" => Mode::Thinking,
                    "<|content_text|>" => Mode::Text,
                    "<|content_invoke_tool_json|>" => Mode::ToolJson,
                    "<|content_invoke_tool_text|>" => Mode::ToolText,
                    _ => Mode::Hidden,
                };
            }
            None => {
                self.frame.push(token);
                if matches!(self.mode, Mode::Thinking | Mode::Text | Mode::Boundary) {
                    let text = vocab.decode(&self.frame);
                    // A character whose bytes are not all there yet is held back.
                    if !text.ends_with('\u{FFFD}') && text.len() > self.shown && text.is_char_boundary(self.shown) {
                        let piece = &text[self.shown..];
                        on(if self.mode == Mode::Thinking { Event::Thinking(piece) } else { Event::Text(piece) });
                        self.shown = text.len();
                    }
                }
            }
        }
    }

    /// End the open message, if any (also at the end of the turn, for one that was cut off).
    pub fn close(&mut self, vocab: &dyn Vocab, on: &mut dyn FnMut(Event)) {
        let body = vocab.decode(&self.frame);
        self.frame.clear();
        self.shown = 0;
        let mode = std::mem::replace(&mut self.mode, Mode::Boundary);
        match mode {
            Mode::Thinking => {
                self.thinking.push_str(&body);
                on(Event::End);
            }
            Mode::Text | Mode::Boundary => {
                if !body.trim().is_empty() {
                    if !self.text.is_empty() {
                        self.text.push_str("\n\n");
                    }
                    self.text.push_str(body.trim_end());
                    on(Event::End);
                }
            }
            Mode::ToolJson => self.calls.push(parse_call(&self.name, &body)),
            Mode::ToolText => self.calls.push(Call { name: self.name.clone(), args: json!({"input": body}), error: None }),
            Mode::Header | Mode::Hidden => {}
        }
        if mode != Mode::Header {
            self.name.clear();
        }
    }
}

fn parse_call(header: &str, body: &str) -> Call {
    let parsed: Result<Value, _> = serde_json::from_str(body.trim());
    match parsed {
        Ok(v) => {
            let name = v["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(header).to_string();
            // {"name":..,"args":{..}} is the format; a bare arguments object is accepted too.
            let args = match v.get("args").or_else(|| v.get("arguments")) {
                Some(a) => a.clone(),
                None if v.get("name").is_none() => v.clone(),
                None => json!({}),
            };
            let error = (!args.is_object()).then(|| "the tool call's \"args\" must be a JSON object".to_string());
            Call { name, args, error }
        }
        Err(e) => Call { name: header.to_string(), args: Value::Null, error: Some(format!("the tool call was not valid JSON ({e}); write {{\"name\":\"...\",\"args\":{{...}}}}")) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_images() -> impl FnMut(&ImageRef) -> anyhow::Result<usize> {
        |_| Ok(2)
    }

    #[test]
    fn a_conversation_renders_as_the_ollama_renderer_does() {
        // The reference cases of ollama/model/renderers/inkling2_reference_test.go.
        let tools = [ToolSpec {
            name: "lookup".into(),
            description: String::new(),
            parameters: json!({"type": "object", "properties": {"z": {"type": "string"}, "A": {"type": "number"}}}),
        }];
        assert_eq!(
            flatten(&declare(&tools)),
            r#"<|message_system|>tool_declare<|content_xml|>[{"description":"","name":"lookup","parameters":{"properties":{"A":{"type":"number"},"z":{"type":"string"}},"type":"object"},"type":"function"}]<|end_message|>"#
        );
        let call = Call { name: "lookup".into(), args: json!({"z": "café", "A": {"z": 1.0, "a": null}}), error: None };
        assert_eq!(call_body(&call), r#"{"name":"lookup","args":{"A":{"z":1.0,"a":null},"z":"café"}}"#);
        let assistant = Message::Assistant { thinking: "hidden".into(), text: "Let me check.".into(), calls: vec![call, Call { name: "save".into(), args: json!({}), error: None }] };
        assert_eq!(
            flatten(&render(&assistant, &mut no_images()).unwrap()),
            "<|message_model|><|content_text|>Let me check.<|end_message|>\
             <|message_model|>lookup<|content_invoke_tool_json|>{\"name\":\"lookup\",\"args\":{\"A\":{\"z\":1.0,\"a\":null},\"z\":\"café\"}}<|end_message|>\
             <|message_model|>save<|content_invoke_tool_json|>{\"name\":\"save\",\"args\":{}}<|end_message|>\
             <|content_model_end_sampling|>"
        );
        let result = Message::Tool { name: "lookup".into(), text: "First".into(), images: vec![] };
        assert_eq!(flatten(&render(&result, &mut no_images()).unwrap()), "<|message_tool|>lookup<|content_text|>First<|end_message|>");
    }

    #[test]
    fn the_start_puts_tools_then_system_then_effort() {
        assert_eq!(
            flatten(&start(&[], "Be brief.", "0")) + &flatten(&render(&Message::User { text: "hi".into(), images: vec![] }, &mut no_images()).unwrap()),
            "<|message_system|><|content_text|>Be brief.<|end_message|>\
             <|message_system|><|content_text|>Thinking effort level: 0<|end_message|>\
             <|message_user|><|content_text|>hi<|end_message|>"
        );
        assert_eq!(effort_level("medium"), Some("0.7"));
        assert_eq!(effort_level("loud"), None);
    }

    #[test]
    fn an_image_is_its_own_message_before_the_text() {
        let user = Message::User { text: "what is this?".into(), images: vec![ImageRef { blob: 1, name: "a.png".into() }] };
        assert_eq!(
            flatten(&render(&user, &mut no_images()).unwrap()),
            "<|message_user|><|content_image|><|unused_200054|><|unused_200054|><|end_message|><|message_user|><|content_text|>what is this?<|end_message|>"
        );
        let tool = Message::Tool { name: "view_image".into(), text: String::new(), images: vec![ImageRef { blob: 1, name: "a.png".into() }] };
        assert_eq!(flatten(&render(&tool, &mut no_images()).unwrap()), "<|message_tool|>view_image<|content_image|><|unused_200054|><|unused_200054|><|end_message|>");
    }

    #[test]
    fn messages_survive_json() {
        let all = [
            Message::System("s".into()),
            Message::User { text: "u".into(), images: vec![ImageRef { blob: 7, name: "x.png".into() }] },
            Message::Assistant { thinking: "t".into(), text: "a".into(), calls: vec![Call { name: "n".into(), args: json!({"k": [1, "two"]}), error: None }] },
            Message::Tool { name: "n".into(), text: "r".into(), images: vec![] },
        ];
        for m in all {
            assert_eq!(Message::from_json(&m.to_json()), Some(m));
        }
    }

    /// A vocabulary of one token per character, with the control tokens from 1000.
    struct Chars;
    const CONTROLS: [&str; 7] =
        ["<|message_model|>", "<|content_thinking|>", "<|content_text|>", "<|content_invoke_tool_json|>", "<|content_invoke_tool_text|>", "<|content_tool_error|>", "<|end_message|>"];

    impl Vocab for Chars {
        fn decode(&self, ids: &[u32]) -> String {
            ids.iter().map(|&i| char::from_u32(i).unwrap()).collect()
        }
        fn control(&self, id: u32) -> Option<&'static str> {
            CONTROLS.get((id as usize).wrapping_sub(0x10_0000)).copied()
        }
    }

    fn tokens(mut s: &str) -> Vec<u32> {
        let mut out = Vec::new();
        'outer: while !s.is_empty() {
            for (i, control) in CONTROLS.iter().enumerate() {
                if let Some(rest) = s.strip_prefix(control) {
                    out.push(0x10_0000 + i as u32);
                    s = rest;
                    continue 'outer;
                }
            }
            let ch = s.chars().next().unwrap();
            out.push(ch as u32);
            s = &s[ch.len_utf8()..];
        }
        out
    }

    fn parse(s: &str) -> (Parser, String) {
        let mut p = Parser::new();
        let mut shown = String::new();
        let mut on = |e: Event| match e {
            Event::Thinking(t) => shown.push_str(&format!("({t})")),
            Event::Text(t) => shown.push_str(t),
            Event::End => shown.push('|'),
        };
        for t in tokens(s) {
            p.push(&Chars, t, &mut on);
        }
        p.close(&Chars, &mut on);
        (p, shown)
    }

    #[test]
    fn a_turn_is_read_as_thinking_text_and_calls() {
        // What the model wrote for a real request (Ollama's parser trace).
        let (p, shown) = parse(
            "<|message_model|><|content_thinking|>Hm<|end_message|>\
             <|message_model|><|content_text|>OK한<|end_message|>\
             <|message_model|>read_file<|content_invoke_tool_json|>{\"name\":\"read_file\",\"args\":{\"path\":\"/tmp/a.txt\"}}<|end_message|>\
             <|message_model|>bash<|content_invoke_tool_json|>{\"name\":\"bash\",\"args\":{\"command\":\"ls <|x|>\"}}<|end_message|>",
        );
        assert_eq!(p.thinking, "Hm");
        assert_eq!(p.text, "OK한");
        assert_eq!(shown, "(H)(m)|OK한|");
        assert_eq!(p.calls.len(), 2);
        assert_eq!(p.calls[0], Call { name: "read_file".into(), args: json!({"path": "/tmp/a.txt"}), error: None });
        assert_eq!(p.calls[1].args["command"], "ls <|x|>");
        assert!(p.at_end_message);
    }

    #[test]
    fn broken_and_cut_off_calls_are_reported_not_dropped() {
        let (p, _) = parse("<|message_model|>grep<|content_invoke_tool_json|>{\"name\":\"grep\",\"args\":{\"pattern\":");
        assert_eq!(p.calls[0].name, "grep");
        assert!(p.calls[0].error.is_some());
        assert!(!p.at_end_message);
        // A bare arguments object under a named header is taken as the arguments.
        let (p, _) = parse("<|message_model|>grep<|content_invoke_tool_json|>{\"pattern\":\"x\"}<|end_message|>");
        assert_eq!(p.calls[0], Call { name: "grep".into(), args: json!({"pattern": "x"}), error: None });
        // Text outside any message is an answer.
        let (p, _) = parse("plain answer");
        assert_eq!(p.text, "plain answer");
    }
}
