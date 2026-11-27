//! Optional, storage-independent inspection at completed simulation ticks.
pub mod jobs;
mod protocol;
pub mod report;
mod session;
mod stdio;

use serde::Serialize;

pub use session::{Journal, Notification, Session};
pub use stdio::{StdioPort, serve_stdio};

/// The game assigns IDs that are never reused within this world instance.
pub type ObjectId = u64;

#[derive(Clone, Copy, Serialize)]
pub struct Field {
    pub id: &'static str,
    pub kind: Kind,
    pub description: &'static str,
    pub unit: Option<&'static str>,
}

#[derive(Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Bool,
    Text,
    Number,
    Reference,
}

#[derive(Clone, Copy, Serialize)]
pub struct EventSchema {
    pub id: &'static str,
    pub description: &'static str,
    pub fields: &'static [Field],
}

#[derive(Clone, Copy, Serialize)]
pub struct Schema {
    pub id: &'static str,
    pub version: u32,
    pub description: &'static str,
    pub fields: &'static [Field],
    pub events: &'static [EventSchema],
}

pub struct Object<'a> {
    pub id: ObjectId,
    pub kind: &'static str,
    pub name: &'a str,
}

pub enum Value<'a> {
    Bool(bool),
    Text(&'a str),
    Number(f64),
    Reference(ObjectId),
}

impl Value<'_> {
    fn kind(&self) -> Kind {
        match self {
            Self::Bool(_) => Kind::Bool,
            Self::Number(_) => Kind::Number,
            Self::Text(_) => Kind::Text,
            Self::Reference(_) => Kind::Reference,
        }
    }
}

/// All methods run on the host's simulation thread, at a committed boundary.
pub trait World {
    /// Payload copied at the decision point. Its serialized fields follow EventSchema.
    type Facts: Copy + Serialize;
    fn schemas(&self) -> &'static [Schema];
    /// Visit at most limit matching objects starting at offset, in repeatable order.
    fn find(
        &self,
        kind: Option<&str>,
        offset: usize,
        limit: usize,
        visit: &mut dyn FnMut(Object<'_>),
    );
    fn object_type(&self, id: ObjectId) -> Option<&'static str>;
    fn read(&self, id: ObjectId, field: &str) -> Option<Value<'_>>;
    /// Apply exactly one complete update. The host drives presentation separately.
    fn step(&mut self, events: &mut Journal<Self::Facts>);
}

#[derive(Clone, Copy, Serialize)]
pub struct Event<F> {
    pub kind: &'static str,
    pub subject: ObjectId,
    pub actor: Option<ObjectId>,
    pub facts: F,
}
