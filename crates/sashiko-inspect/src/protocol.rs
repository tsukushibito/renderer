use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) type Projection = BTreeMap<String, Vec<String>>;
pub(crate) type Result<T> = std::result::Result<T, &'static str>;

#[derive(Deserialize)]
pub(crate) struct Request {
    pub id: String,
    #[serde(flatten)]
    pub operation: Operation,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Operation {
    Hello,
    Capture {
        request: crate::jobs::CaptureRequest,
    },
    Job {
        job: String,
    },
    Schema {
        #[serde(default)]
        kind: Option<String>,
    },
    Find {
        kind: Option<String>,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page")]
        limit: usize,
        revision: Option<String>,
    },
    Read {
        object: String,
        fields: Vec<String>,
        snapshot: Option<String>,
    },
    Snapshot {
        objects: Projection,
        #[serde(default = "minute")]
        ttl_ms: u64,
    },
    Wait {
        #[serde(flatten)]
        filter: Filter,
        #[serde(default)]
        pause: bool,
        #[serde(default = "minute")]
        timeout_ms: u64,
        #[serde(default)]
        capture: Projection,
    },
    Watch {
        #[serde(flatten)]
        filter: Filter,
        since: Option<String>,
    },
    Cancel {
        subscription: String,
    },
    Pause,
    Step,
    Resume,
}

fn page() -> usize {
    32
}
fn minute() -> u64 {
    60_000
}

#[derive(Deserialize)]
pub(crate) struct Filter {
    pub event: String,
    pub object: Option<String>,
    #[serde(default)]
    pub role: Role,
}

#[derive(Default, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Role {
    #[default]
    Subject,
    Actor,
}

pub(crate) fn answer(id: Value, result: Result<Value>) -> Value {
    match result {
        Ok(data) => json!({"ok":true,"id":id,"result":data}),
        Err(code) => json!({"ok":false,"id":id,"error":{"code":code}}),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Record<F> {
    pub sequence: u64,
    pub tick: u64,
    pub event: crate::Event<F>,
}
