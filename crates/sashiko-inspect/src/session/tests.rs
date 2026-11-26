use super::*;
use crate::{EventSchema, Field, Kind, Object, Schema};
use std::collections::BTreeMap;

struct Row {
    id: u64,
    hp: f64,
}

trait Storage {
    fn new() -> Self;
    fn visit(&self, visit: &mut dyn FnMut(u64, f64));
    fn replace_first(&mut self);
}

struct Rows(Vec<Row>);
impl Storage for Rows {
    fn new() -> Self {
        Self(vec![Row { id: 1, hp: 10. }, Row { id: 2, hp: 20. }])
    }
    fn visit(&self, visit: &mut dyn FnMut(u64, f64)) {
        for r in &self.0 {
            visit(r.id, r.hp)
        }
    }
    fn replace_first(&mut self) {
        self.0.swap_remove(0);
        self.0.push(Row { id: 3, hp: 30. });
    }
}

// Separate component store; the entity's position in the query is not its identity.
struct Components {
    entities: Vec<u64>,
    health: BTreeMap<u64, f64>,
}
impl Storage for Components {
    fn new() -> Self {
        Self {
            entities: vec![1, 2],
            health: BTreeMap::from([(1, 10.), (2, 20.)]),
        }
    }
    fn visit(&self, visit: &mut dyn FnMut(u64, f64)) {
        for id in &self.entities {
            visit(*id, self.health[id])
        }
    }
    fn replace_first(&mut self) {
        self.entities = vec![3, 2];
        self.health.remove(&1);
        self.health.insert(3, 30.);
    }
}

struct Arena<S>(S);
impl<S: Storage> World for Arena<S> {
    type Facts = ();
    fn schemas(&self) -> &'static [Schema] {
        &[Schema {
            id: "Enemy",
            version: 1,
            description: "",
            fields: &[Field {
                id: "hp",
                kind: Kind::Number,
                unit: None,
                description: "Hit points",
            }],
            events: &[EventSchema {
                id: "removed",
                description: "",
                fields: &[],
            }],
        }]
    }
    fn find(
        &self,
        kind: Option<&str>,
        offset: usize,
        limit: usize,
        visit: &mut dyn FnMut(Object<'_>),
    ) {
        if kind.is_some_and(|k| k != "Enemy") {
            return;
        }
        let mut index = 0;
        self.0.visit(&mut |id, _| {
            if (offset..offset.saturating_add(limit)).contains(&index) {
                visit(Object {
                    id,
                    kind: "Enemy",
                    name: "Mob",
                })
            }
            index += 1;
        })
    }
    fn object_type(&self, id: u64) -> Option<&'static str> {
        self.read(id, "hp").map(|_| "Enemy")
    }
    fn read(&self, id: u64, field: &str) -> Option<FieldValue<'_>> {
        let mut hp = None;
        self.0.visit(&mut |entity, value| {
            if entity == id && field == "hp" {
                hp = Some(FieldValue::Number(value))
            }
        });
        hp
    }
    fn step(&mut self, events: &mut Journal<()>) {
        events.emit(Event {
            kind: "removed",
            subject: 1,
            actor: None,
            facts: (),
        });
        self.0.replace_first()
    }
}

fn check_storage<S: Storage>() {
    let mut session = Session::new(Arena(S::new()));
    let start = Instant::now();
    let first = session.key(1);
    let second = session.key(2);
    let hp = vec!["hp".to_owned()];
    let snap = session
        .snapshot(
            &BTreeMap::from([(first.clone(), hp.clone()), (second.clone(), hp.clone())]),
            10,
            start,
            false,
        )
        .unwrap();
    session.step();
    assert_eq!(session.read(&first, &hp, None, start), Err("OBJECT_GONE"));
    assert_eq!(
        session.read(&second, &hp, None, start).unwrap()["values"]["hp"],
        20.
    );
    let token = snap["id"].as_str();
    assert_eq!(
        session.read(&first, &hp, token, start).unwrap()["values"]["hp"],
        10.
    );
    assert_eq!(
        session.read(&first, &hp, token, start + Duration::from_millis(10)),
        Err("SNAPSHOT_EXPIRED")
    );
    assert_eq!(
        Session::new(Arena(S::new())).object_id(&first),
        Err("OBJECT_GONE")
    );
}

#[test]
fn same_handles_and_projections_for_rows_and_components() {
    check_storage::<Rows>();
    check_storage::<Components>();
}

#[test]
fn cancelling_and_reusing_a_watch_preserves_the_world_and_journal() {
    let mut session = Session::new(Arena(Rows::new()));
    for _ in 0..=SUBSCRIPTIONS {
        session.watch("observer", "removed").unwrap();
        session.step();
        session.cancel("observer").unwrap();
        let ended = session.poll().unwrap().into_json();
        assert_eq!(ended["id"], "observer");
        assert_eq!(
            ended["result"],
            json!({"state":"ended","reason":"cancelled"})
        );
        assert_eq!(session.cancel("observer"), Err("UNKNOWN_SUBSCRIPTION"));
    }
    assert_eq!(session.tick(), SUBSCRIPTIONS as u64 + 1);
    assert_eq!(session.world().object_type(3), Some("Enemy"));
    let replay =
        serde_json::from_value(json!({"op":"watch","id":"replay","event":"removed","since":"0"}))
            .unwrap();
    session.request(replay, Instant::now()).unwrap();
    assert_eq!(
        session.poll().unwrap().into_json()["result"]["event"]["tick"],
        "1"
    );
}

#[test]
fn live_snapshots_are_not_evicted_to_make_room() {
    let mut session = Session::new(Arena(Rows::new()));
    let now = Instant::now();
    let projection = BTreeMap::from([(session.key(1), vec!["hp".to_owned()])]);
    for _ in 0..SNAPSHOTS {
        session.snapshot(&projection, 10, now, false).unwrap();
    }
    assert_eq!(
        session.snapshot(&projection, 10, now, false),
        Err("CAPACITY")
    );
    assert!(
        session
            .snapshot(&projection, 10, now + Duration::from_millis(10), false)
            .is_ok()
    );
}

#[test]
fn armed_wait_pins_a_transient_event_before_the_ring_wraps() {
    let mut s = Session::new(Arena(Rows::new()));
    let request = serde_json::from_value(
        json!({"id":"condition","op":"wait","event":"removed","pause":true}),
    )
    .unwrap();
    s.request(request, Instant::now()).unwrap();
    let mut journal = Journal {
        history: &mut s.journal,
        subscriptions: &mut s.subscriptions,
    };
    for id in 0..=HISTORY as u64 {
        journal.emit(Event {
            kind: "removed",
            subject: id,
            actor: None,
            facts: (),
        });
    }
    let terminal = s.poll().unwrap().into_json();
    assert_eq!(terminal["result"]["event"]["sequence"], "1");
    assert_eq!(terminal["result"]["event"]["self"], s.key(0));
}
