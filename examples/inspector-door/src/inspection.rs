use super::Game;
use sashiko::inspect::{
    Event, EventSchema, Field, Journal, Kind, Object, ObjectId, Schema, Value, World,
};
use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
#[serde(untagged)]
pub enum Facts {
    Keys {
        required_key: &'static str,
        presented_key: &'static str,
    },
    Lifecycle {},
}

const fn field(id: &'static str, kind: Kind, description: &'static str) -> Field {
    Field {
        id,
        kind,
        description,
        unit: None,
    }
}
const KEYS: &[Field] = &[
    field("required_key", Kind::Text, "Key required at the decision"),
    field("presented_key", Kind::Text, "Key presented at the decision"),
];
const SCHEMAS: &[Schema] = &[
    Schema {
        id: "Door",
        version: 1,
        description: "Basement entrance",
        fields: &[
            field("state", Kind::Text, "Open or locked"),
            field("required_key", Kind::Text, "Key that unlocks this door"),
        ],
        events: &[
            EventSchema {
                id: "unlock_denied",
                description: "Wrong key",
                fields: KEYS,
            },
            EventSchema {
                id: "unlocked",
                description: "Matching key",
                fields: KEYS,
            },
            EventSchema {
                id: "despawn",
                description: "Door removed",
                fields: &[],
            },
            EventSchema {
                id: "spawn",
                description: "Door created",
                fields: &[],
            },
        ],
    },
    Schema {
        id: "Player",
        version: 1,
        description: "Player holding one key",
        fields: &[field(
            "held_key",
            Kind::Text,
            "Key held at the completed tick",
        )],
        events: &[],
    },
];

impl Game {
    fn key(&self) -> &'static str {
        if self.held_basement_key {
            "basement"
        } else {
            "garden"
        }
    }
    pub(super) fn record_decision(&self, journal: &mut Journal<Facts>) {
        journal.emit(Event {
            kind: if self.open {
                "unlocked"
            } else {
                "unlock_denied"
            },
            subject: self.door,
            actor: Some(1),
            facts: Facts::Keys {
                required_key: "basement",
                presented_key: self.key(),
            },
        });
    }
    pub(super) fn record_lifetime(&self, journal: &mut Journal<Facts>, kind: &'static str) {
        journal.emit(Event {
            kind,
            subject: self.door,
            actor: None,
            facts: Facts::Lifecycle {},
        });
    }
}

impl World for Game {
    type Facts = Facts;
    fn schemas(&self) -> &'static [Schema] {
        SCHEMAS
    }
    fn find(
        &self,
        kind: Option<&str>,
        offset: usize,
        limit: usize,
        visit: &mut dyn FnMut(Object<'_>),
    ) {
        for object in [
            Object {
                id: 1,
                kind: "Player",
                name: "Key holder",
            },
            Object {
                id: self.door,
                kind: "Door",
                name: "Basement door",
            },
        ]
        .into_iter()
        .filter(|o| o.id != 0 && kind.is_none_or(|k| o.kind == k))
        .skip(offset)
        .take(limit)
        {
            visit(object)
        }
    }
    fn object_type(&self, id: ObjectId) -> Option<&'static str> {
        match id {
            1 => Some("Player"),
            n if n != 0 && n == self.door => Some("Door"),
            _ => None,
        }
    }
    fn read(&self, id: ObjectId, field: &str) -> Option<Value<'_>> {
        match (self.object_type(id)?, field) {
            ("Player", "held_key") => Some(Value::Text(self.key())),
            ("Door", "state") => Some(Value::Text(if self.open { "open" } else { "locked" })),
            ("Door", "required_key") => Some(Value::Text("basement")),
            _ => None,
        }
    }
    fn step(&mut self, journal: &mut Journal<Facts>) {
        self.update(Some(journal))
    }
}
