//! Where what happens in a turn goes: a sink of events (cwbcode_proto::What). The server
//! streams them to the client and logs them in the project's store; the clients draw them.

use cwbcode_proto::What;

use crate::prompt::{Call, Event};
use crate::tools::{ToolOutput, summary};

pub struct Ui {
    sink: Box<dyn FnMut(What)>,
    muted: bool,
}

impl Ui {
    pub fn new(sink: impl FnMut(What) + 'static) -> Self {
        Self { sink: Box::new(sink), muted: false }
    }

    /// A UI that shows nothing (for steps the user does not watch, like summarizing).
    pub fn muted() -> Self {
        Self { sink: Box::new(|_| {}), muted: true }
    }

    pub fn emit(&mut self, what: What) {
        if !self.muted {
            (self.sink)(what);
        }
    }

    /// A piece of the model's turn, as it is written.
    pub fn event(&mut self, event: Event) {
        match event {
            Event::Thinking(text) => self.emit(What::Thinking(text.to_string())),
            Event::Text(text) => self.emit(What::Text(text.to_string())),
            Event::End => self.emit(What::EndMessage),
        }
    }

    pub fn tool_start(&mut self, call: &Call) {
        self.emit(What::ToolStart { name: call.name.clone(), summary: summary(call), args: call.args.to_string() });
    }

    pub fn tool_end(&mut self, out: &ToolOutput, seconds: f64) {
        self.emit(What::ToolEnd { ok: out.ok, text: out.text.clone(), images: out.images.len() as u32, seconds });
    }

    /// A line from cwbcode itself.
    pub fn note(&mut self, text: &str) {
        self.emit(What::Note(text.to_string()));
    }

    pub fn warn(&mut self, text: &str) {
        self.emit(What::Warning(text.to_string()));
    }

    pub fn error(&mut self, text: &str) {
        self.emit(What::Error(text.to_string()));
    }
}
