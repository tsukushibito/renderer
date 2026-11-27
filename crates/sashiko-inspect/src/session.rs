use crate::protocol::{Filter, Operation, Projection, Record, Request, Result, answer};
use crate::{Event, ObjectId, Value as FieldValue, World};
use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const HISTORY: usize = 64;
const SUBSCRIPTIONS: usize = 16;
const SNAPSHOTS: usize = 16;
const MAX_FIELDS: usize = 64;
static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);

/// Fixed-capacity history. Events copy the game's facts before the game changes again.
pub struct Journal<'a, F> {
    history: &'a mut History<F>,
    subscriptions: &'a mut [Subscription<F>],
}

struct History<F> {
    entries: VecDeque<Record<F>>,
    sequence: u64,
    tick: u64,
}

impl<F: Copy> Journal<'_, F> {
    pub fn emit(&mut self, event: Event<F>) {
        let history = &mut self.history;
        history.sequence = history
            .sequence
            .checked_add(1)
            .expect("event sequence overflow");
        if history.entries.len() == HISTORY {
            history.entries.pop_front();
        }
        let record = Record {
            sequence: history.sequence,
            tick: history.tick,
            event,
        };
        history.entries.push_back(record);
        // Pin the first match at emission: one busy tick can exceed the history ring.
        for sub in &mut *self.subscriptions {
            if let Some(wait) = &sub.wait
                && sub.matched.is_none()
                && sub.ready.is_none()
                && sub.matches(&record)
                && Instant::now() < wait.deadline
            {
                sub.matched = Some(record);
            }
        }
    }
}

struct Snapshot {
    id: u64,
    tick: u64,
    expires: Instant,
    objects: Map<String, Value>,
    missing: Map<String, Value>,
}

struct Subscription<F> {
    role: crate::protocol::Role,
    id: Arc<str>,
    kind: String,
    object: Option<ObjectId>,
    after: u64,
    wait: Option<Wait>,
    ready: Option<Value>,
    matched: Option<Record<F>>,
}

impl<F> Subscription<F> {
    fn matches(&self, record: &Record<F>) -> bool {
        record.event.kind == self.kind
            && self.object.is_none_or(|id| match self.role {
                crate::protocol::Role::Subject => id == record.event.subject,
                crate::protocol::Role::Actor => Some(id) == record.event.actor,
            })
    }
}

struct Wait {
    deadline: Instant,
    pause: bool,
    capture: Projection,
}

/// One local process session for one independently stepped world.
pub struct Session<W: World> {
    jobs: Option<crate::jobs::Jobs>,
    world: W,
    name: Arc<str>,
    journal: History<W::Facts>,
    pub(crate) paused: bool,
    snapshots: Vec<Snapshot>,
    next_snapshot: u64,
    subscriptions: Vec<Subscription<W::Facts>>,
}

impl<W: World> Session<W> {
    pub fn new(world: W) -> Self {
        let start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!(
            "{:x}-{start:x}-{:x}",
            std::process::id(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        Self {
            world,
            jobs: None,
            name: name.into(),
            paused: true,
            journal: History {
                entries: VecDeque::with_capacity(HISTORY),
                sequence: 0,
                tick: 0,
            },
            snapshots: Vec::with_capacity(SNAPSHOTS),
            next_snapshot: 0,
            subscriptions: Vec::with_capacity(SUBSCRIPTIONS),
        }
    }

    pub fn world(&self) -> &W {
        &self.world
    }

    pub fn world_mut(&mut self) -> &mut W {
        &mut self.world
    }
    pub fn into_world(self) -> W {
        self.world
    }
    pub fn session_id(&self) -> &str {
        &self.name
    }
    pub fn is_paused(&self) -> bool {
        self.paused
    }
    pub fn set_paused(&mut self, pause: bool) {
        self.paused = pause;
    }
    pub fn with_jobs(mut self, jobs: crate::jobs::Jobs) -> Self {
        self.jobs = Some(jobs);
        self
    }
    pub fn jobs(&mut self) -> Option<&mut crate::jobs::Jobs> {
        self.jobs.as_mut()
    }

    pub fn tick(&self) -> u64 {
        self.journal.tick
    }

    /// Install an in-process event stream using the same cursor contract as NDJSON.
    pub fn watch(&mut self, id: &str, event: &str) -> std::result::Result<(), &'static str> {
        if id.is_empty() || id.len() > 128 {
            return Err("BAD_ID");
        }
        self.subscribe(
            id,
            Filter {
                role: crate::protocol::Role::Subject,
                event: event.into(),
                object: None,
            },
            self.journal.sequence,
            None,
        )
        .map(drop)
    }

    /// End a subscription. Poll until its terminal notification to release the
    /// slot and reuse the ID. Pending events remain in the session journal.
    pub fn cancel(&mut self, id: &str) -> std::result::Result<(), &'static str> {
        let sub = self
            .subscriptions
            .iter_mut()
            .find(|s| s.id.as_ref() == id)
            .ok_or("UNKNOWN_SUBSCRIPTION")?;
        sub.ready = Some(answer(
            json!(id),
            Ok(json!({"state":"ended","reason":"cancelled"})),
        ));
        Ok(())
    }

    /// Fetch an owned notification. Call the returned encoder on the consumer thread.
    pub fn poll(&mut self) -> Option<Notification<W::Facts>> {
        self.notification(Instant::now()).map(Notification)
    }

    /// Bytes reserved for the journal's event slots, excluding transport and projections.
    pub fn journal_bytes(&self) -> usize {
        self.journal.entries.capacity() * std::mem::size_of::<Record<W::Facts>>()
    }

    /// Commit one tick, then resolve waiting conditions before any subsequent tick.
    pub fn step(&mut self) {
        self.journal.tick = self.journal.tick.checked_add(1).expect("tick overflow");
        self.world.step(&mut Journal {
            history: &mut self.journal,
            subscriptions: &mut self.subscriptions,
        });
        self.resolve_waits(Instant::now());
    }

    fn key(&self, id: ObjectId) -> String {
        format!("{}/o{id}", self.name)
    }

    fn decode(&self, token: &str, marker: &str) -> Option<u64> {
        let (session, local) = token.split_once('/')?;
        if session != self.name.as_ref() {
            return None;
        }
        local.strip_prefix(marker)?.parse().ok()
    }

    fn object_id(&self, token: &str) -> Result<ObjectId> {
        self.decode(token, "o")
            .filter(|id| self.world.object_type(*id).is_some())
            .ok_or("OBJECT_GONE")
    }

    fn status(&self) -> Value {
        json!({"tick":self.tick().to_string(),"revision":self.tick().to_string(),"paused":self.paused})
    }

    pub(crate) fn next_timeout(&self, now: Instant) -> Duration {
        self.subscriptions
            .iter()
            .filter_map(|s| s.wait.as_ref())
            .map(|w| w.deadline.saturating_duration_since(now))
            .min()
            .unwrap_or(Duration::from_secs(86_400))
    }

    fn fields(&self, object: &str, fields: &[String]) -> Result<Value> {
        if fields.is_empty() || fields.len() > MAX_FIELDS {
            return Err("BAD_PROJECTION");
        }
        let id = self.decode(object, "o").ok_or("OBJECT_GONE")?;
        let kind = self.world.object_type(id).ok_or("OBJECT_GONE")?;
        let schema = self
            .world
            .schemas()
            .iter()
            .find(|s| s.id == kind)
            .ok_or("SCHEMA_MISMATCH")?;
        let mut values = Map::new();
        for field in fields {
            let declared = schema
                .fields
                .iter()
                .find(|f| f.id == field)
                .ok_or("UNKNOWN_FIELD")?;
            let actual = self.world.read(id, field).ok_or("FIELD_UNAVAILABLE")?;
            if actual.kind() != declared.kind {
                return Err("SCHEMA_MISMATCH");
            }
            let value = match actual {
                FieldValue::Bool(value) => json!(value),
                FieldValue::Text(value) if value.len() <= 1024 => json!(value),
                FieldValue::Text(_) => return Err("CAPACITY"),
                FieldValue::Number(value) if value.is_finite() => json!(value),
                FieldValue::Number(_) => return Err("INVALID_NUMBER"),
                FieldValue::Reference(value) if self.world.object_type(value).is_some() => {
                    json!(self.key(value))
                }
                FieldValue::Reference(_) => return Err("REFERENCE_GONE"),
            };
            values.insert(field.clone(), value);
        }
        Ok(Value::Object(values))
    }

    fn snapshot(
        &mut self,
        objects: &Projection,
        ttl_ms: u64,
        now: Instant,
        allow_gone: bool,
    ) -> Result<Value> {
        if objects.is_empty()
            || objects.values().map(Vec::len).sum::<usize>() > MAX_FIELDS
            || !(1..=60_000).contains(&ttl_ms)
        {
            return Err("BAD_PROJECTION");
        }
        self.snapshots.retain(|s| now < s.expires);
        if self.snapshots.len() >= SNAPSHOTS {
            return Err("CAPACITY");
        }
        let mut data = Map::new();
        let mut missing = Map::new();
        for (object, fields) in objects {
            match self.fields(object, fields) {
                Ok(values) => {
                    data.insert(object.clone(), values);
                }
                Err("OBJECT_GONE") if allow_gone => {
                    missing.insert(object.clone(), json!("OBJECT_GONE"));
                }
                Err(reason) => return Err(reason),
            }
        }
        let id = self.next_snapshot;
        self.next_snapshot = id.checked_add(1).ok_or("CAPACITY")?;
        self.snapshots.push(Snapshot {
            id,
            tick: self.tick(),
            expires: now + Duration::from_millis(ttl_ms),
            objects: data,
            missing: missing.clone(),
        });
        Ok(
            json!({"id":format!("{}/s{id}",self.name),"tick":self.tick().to_string(),"revision":self.tick().to_string(),
            "phase":"end_of_tick","fields":objects,"missing":missing,"expires_in_ms":ttl_ms}),
        )
    }

    fn read(
        &self,
        object: &str,
        fields: &[String],
        token: Option<&str>,
        now: Instant,
    ) -> Result<Value> {
        if fields.is_empty() || fields.len() > MAX_FIELDS {
            return Err("BAD_PROJECTION");
        }
        let (values, tick) = if let Some(token) = token {
            let id = self.decode(token, "s").ok_or("SNAPSHOT_EXPIRED")?;
            let snap = self
                .snapshots
                .iter()
                .find(|s| s.id == id && now < s.expires)
                .ok_or("SNAPSHOT_EXPIRED")?;
            if snap.missing.contains_key(object) {
                return Err("OBJECT_GONE");
            }
            let data = snap.objects.get(object).ok_or("NOT_CAPTURED")?;
            let mut values = Map::new();
            for field in fields {
                values.insert(
                    field.clone(),
                    data.get(field).ok_or("NOT_CAPTURED")?.clone(),
                );
            }
            (Value::Object(values), snap.tick)
        } else {
            (self.fields(object, fields)?, self.tick())
        };
        Ok(
            json!({"tick":tick.to_string(),"revision":tick.to_string(),"object":object,"values":values}),
        )
    }

    fn subscribe(
        &mut self,
        id: &str,
        filter: Filter,
        after: u64,
        wait: Option<Wait>,
    ) -> Result<Value> {
        if self.subscriptions.len() == SUBSCRIPTIONS {
            return Err("CAPACITY");
        }
        if self.subscriptions.iter().any(|s| s.id.as_ref() == id) {
            return Err("DUPLICATE_SUBSCRIPTION");
        }
        if !self
            .world
            .schemas()
            .iter()
            .any(|s| s.events.iter().any(|e| e.id == filter.event))
        {
            return Err("UNKNOWN_EVENT");
        }
        let object = filter
            .object
            .as_deref()
            .map(|s| {
                if wait.is_some() {
                    self.object_id(s)
                } else {
                    self.decode(s, "o").ok_or("OBJECT_GONE")
                }
            })
            .transpose()?;
        if after > self.journal.sequence {
            return Err("BAD_CURSOR");
        }
        self.subscriptions.push(Subscription {
            role: filter.role,
            id: id.into(),
            kind: filter.event,
            object,
            after,
            wait,
            ready: None,
            matched: None,
        });
        Ok(json!({"subscription":id,"state":"registered","after":after.to_string()}))
    }

    pub(crate) fn request(&mut self, request: Request, now: Instant) -> Result<Value> {
        let id = &request.id;
        match request.operation {
            Operation::Hello => {
                let mut operations = vec![
                    "hello", "schema", "find", "read", "snapshot", "wait", "watch", "cancel",
                    "pause", "step", "resume",
                ];
                if self.jobs.is_some() {
                    operations.extend(["capture", "job"]);
                }
                Ok(
                    json!({"protocol":1,"session":&*self.name,"world":"game","state":self.status(),
                    "operations":operations,"diagnostics":self.jobs.as_ref().map(|j| j.capabilities())}),
                )
            }
            Operation::Capture { request } => {
                let tick = self.tick();
                let id = self
                    .jobs
                    .as_mut()
                    .ok_or("UNSUPPORTED")?
                    .submit(tick, request)?;
                Ok(
                    json!({"job":format!("{}/j{id}", self.name),"state":"queued","requested_tick":tick.to_string()}),
                )
            }
            Operation::Job { job } => {
                let id = self.decode(&job, "j").ok_or("UNKNOWN_JOB")?;
                Ok(json!(self.jobs.as_ref().ok_or("UNSUPPORTED")?.status(id)?))
            }
            Operation::Schema { kind: Some(kind) } => {
                let schema = self
                    .world
                    .schemas()
                    .iter()
                    .find(|s| s.id == kind)
                    .ok_or("UNKNOWN_TYPE")?;
                Ok(json!(schema))
            }
            Operation::Schema { kind: None } => Ok(json!(self.world.schemas())),
            Operation::Find {
                kind,
                offset,
                limit,
                revision,
            } => {
                if !(1..=128).contains(&limit) {
                    return Err("CAPACITY");
                }
                if (offset != 0 || revision.is_some())
                    && revision.as_deref().and_then(|s| s.parse::<u64>().ok()) != Some(self.tick())
                {
                    return Err("STALE_PAGE");
                }
                let mut objects = Vec::new();
                self.world
                    .find(kind.as_deref(), offset, limit + 1, &mut |o| {
                        objects.push(json!({"id":self.key(o.id),"kind":o.kind,"name":o.name}))
                    });
                let next = if objects.len() > limit {
                    objects.truncate(limit);
                    Some(offset.checked_add(limit).ok_or("BAD_CURSOR")?)
                } else {
                    None
                };
                Ok(json!({"objects":objects,"next_offset":next,"revision":self.tick().to_string()}))
            }
            Operation::Snapshot { objects, ttl_ms } => self.snapshot(&objects, ttl_ms, now, false),
            Operation::Read {
                object,
                fields,
                snapshot,
            } => self.read(&object, &fields, snapshot.as_deref(), now),
            Operation::Watch { filter, since } => {
                let cursor = since
                    .map(|s| s.parse().map_err(|_| "BAD_CURSOR"))
                    .transpose()?
                    .unwrap_or(self.journal.sequence);
                self.subscribe(id, filter, cursor, None)
            }
            Operation::Wait {
                filter,
                pause,
                timeout_ms,
                capture,
            } => {
                if !(1..=86_400_000).contains(&timeout_ms) {
                    return Err("BAD_TIMEOUT");
                }
                if capture.keys().any(|k| k != "self" && k != "actor")
                    || capture.values().any(Vec::is_empty)
                    || capture.values().map(Vec::len).sum::<usize>() > MAX_FIELDS
                {
                    return Err("BAD_PROJECTION");
                }
                let wait = Wait {
                    deadline: now + Duration::from_millis(timeout_ms),
                    pause,
                    capture,
                };
                self.subscribe(id, filter, self.journal.sequence, Some(wait))
            }
            Operation::Cancel { subscription } => {
                self.cancel(&subscription)?;
                Ok(json!({"subscription":subscription,"state":"cancelled"}))
            }
            Operation::Pause => {
                self.paused = true;
                Ok(self.status())
            }
            Operation::Step if !self.paused => Err("RUNNING"),
            Operation::Step => {
                self.step();
                Ok(self.status())
            }
            Operation::Resume => {
                self.paused = false;
                Ok(self.status())
            }
        }
    }

    fn resolve_waits(&mut self, now: Instant) {
        for i in 0..self.subscriptions.len() {
            let sub = &self.subscriptions[i];
            if sub.ready.is_some() {
                continue;
            }
            let Some(wait) = &sub.wait else { continue };
            let outcome = match sub.matched {
                None if now >= wait.deadline => Err("TIMEOUT"),
                None => continue,
                Some(record) => {
                    if wait.pause {
                        self.paused = true
                    }
                    let mut projection = Projection::new();
                    let mut missing_roles = Vec::new();
                    for (role, fields) in &wait.capture {
                        let target = if role == "self" {
                            Some(record.event.subject)
                        } else {
                            record.event.actor
                        };
                        if let Some(id) = target {
                            projection
                                .entry(self.key(id))
                                .or_default()
                                .extend(fields.iter().cloned());
                        } else {
                            missing_roles.push(role.clone())
                        }
                    }
                    let mut data = json!({"event":wire_event(&self.name,record),"state":"ended"});
                    if !missing_roles.is_empty() {
                        data["missing_roles"] = json!(missing_roles)
                    }
                    if !projection.is_empty() {
                        match self.snapshot(&projection, 60_000, now, true) {
                            Ok(snapshot) => data["snapshot"] = snapshot,
                            Err(code) => data["capture_error"] = json!({"code":code}),
                        }
                    }
                    Ok(data)
                }
            };
            let sub = &mut self.subscriptions[i];
            sub.ready = Some(answer(json!(&*sub.id), outcome));
        }
    }

    fn next_match(&self, sub: &Subscription<W::Facts>) -> Result<Option<Record<W::Facts>>> {
        if self
            .journal
            .entries
            .front()
            .is_some_and(|r| sub.after < r.sequence - 1)
        {
            return Err("HISTORY_GAP");
        }
        Ok(self
            .journal
            .entries
            .iter()
            .find(|r| r.sequence > sub.after && sub.matches(r))
            .copied())
    }

    pub(crate) fn notification(&mut self, now: Instant) -> Option<Delivery<W::Facts>> {
        self.resolve_waits(now);
        for i in 0..self.subscriptions.len() {
            let sub = &self.subscriptions[i];
            if sub.ready.is_some() {
                return self.subscriptions.remove(i).ready.map(Delivery::Reply);
            }
            if sub.wait.is_some() {
                continue;
            }
            let after;
            let result = match self.next_match(sub) {
                Ok(Some(record)) => {
                    after = record.sequence;
                    Delivery::Watch {
                        session: self.name.clone(),
                        id: sub.id.clone(),
                        record,
                    }
                }
                Ok(None) => {
                    self.subscriptions[i].after = self.journal.sequence;
                    continue;
                }
                Err(_) => {
                    after = self.journal.entries.front()?.sequence - 1;
                    Delivery::Reply(answer(
                        json!(&*sub.id),
                        Ok(
                            json!({"state":"gap","code":"HISTORY_GAP","from":(sub.after+1).to_string(),"through":after.to_string()}),
                        ),
                    ))
                }
            };
            self.subscriptions[i].after = after;
            // Rotate serviced streams so a busy first watch cannot starve other readers.
            self.subscriptions.rotate_left(i + 1);
            return Some(result);
        }
        None
    }
}

/// Owned notification, independent of the lifetime of the inspected world.
pub struct Notification<F>(Delivery<F>);
impl<F: serde::Serialize> Notification<F> {
    pub fn into_json(self) -> Value {
        self.0.json()
    }
}

pub(crate) enum Delivery<F> {
    Reply(Value),
    Watch {
        session: Arc<str>,
        id: Arc<str>,
        record: Record<F>,
    },
}

impl<F: serde::Serialize> Delivery<F> {
    pub(crate) fn json(self) -> Value {
        match self {
            Self::Reply(value) => value,
            Self::Watch {
                session,
                id,
                record,
            } => answer(
                json!(&*id),
                Ok(json!({"state":"data","event":wire_event(&session,record)})),
            ),
        }
    }
}

fn wire_event<F: serde::Serialize>(session: &str, r: Record<F>) -> Value {
    let key = |id| format!("{session}/o{id}");
    json!({"sequence":r.sequence.to_string(),"tick":r.tick.to_string(),"type":r.event.kind,
        "self":key(r.event.subject),"actor":r.event.actor.map(key),"facts":r.event.facts})
}

#[cfg(test)]
mod tests;
