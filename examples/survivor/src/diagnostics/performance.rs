use super::Result;
use sashiko::inspect::report::Manifest;
use sashiko_renderer::WindowRenderer;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    time::{Duration, Instant},
};

const CAPACITY: usize = 1024;
#[derive(Clone, Copy, Serialize)]
pub(super) struct Row {
    pub frame: u64,
    pub tick: u64,
    pub updates: u64,
    pub presentation: u64,
    pub elapsed_ns: u64,
    pub interval_ns: u64,
    pub update_ns: u64,
    pub draw_ns: u64,
    pub extent: [u32; 2],
    pub visible_enemies: usize,
    pub drawn_enemies: usize,
    pub recording: bool,
    pub history: bool,
    pub readback: bool,
}
#[derive(Clone)]
pub(super) struct Log {
    pub rows: VecDeque<Row>,
    memories: VecDeque<Value>,
    clock: Value,
}
pub(super) fn ns(time: Duration) -> u64 {
    time.as_nanos().min(u128::from(u64::MAX)) as u64
}
impl Log {
    pub fn new(origin: Instant) -> Self {
        let before = ns(origin.elapsed());
        let counter = sashiko::profiling::performance_counter();
        let after = ns(origin.elapsed());
        let clock = match counter {
            Ok((qpc, hz)) => {
                json!({"domain":"Windows QPC","qpc":qpc.to_string(),"frequency_hz":hz,"elapsed_bracket_ns":[before,after],"pid":std::process::id()})
            }
            Err(e) => {
                json!({"domain":"process monotonic","qpc_unavailable":e.to_string(),"pid":std::process::id()})
            }
        };
        Self {
            rows: VecDeque::with_capacity(CAPACITY),
            memories: VecDeque::with_capacity(18),
            clock,
        }
    }
    pub fn push(&mut self, row: Row, renderer: &mut WindowRenderer) {
        if row.presentation.is_multiple_of(60) {
            let observe = |r: std::result::Result<Value, String>| {
                r.unwrap_or_else(|e| json!({"unavailable":e}))
            };
            let process = observe(sashiko::profiling::working_set().map_err(|e| e.to_string()));
            let graphics = observe(
                sashiko::profiling::renderer_memory(renderer.runtime_info())
                    .map_err(|e| e.to_string()),
            );
            if self.memories.len() == 18 {
                self.memories.pop_front();
            }
            self.memories.push_back(json!({"frame":row.frame,"elapsed_ns":row.elapsed_ns,"process":process,"renderer":graphics}));
        }
        if self.rows.len() == CAPACITY {
            self.rows.pop_front();
        }
        self.rows.push_back(row);
    }
    pub fn write(self, dir: &Path, m: &mut Manifest) -> Result<()> {
        let mut data = BufWriter::new(File::create(dir.join("performance.ndjson"))?);
        let pid = std::process::id();
        let mut events = Vec::with_capacity(self.rows.len() * 3);
        for row in &self.rows {
            serde_json::to_writer(&mut data, row)?;
            data.write_all(b"\n")?;
            let info = json!({"session":m.session,"frame":row.frame,"tick":row.tick,"updates":row.updates,"update_ns":row.update_ns,"draw_ns":row.draw_ns,"readback":row.readback});
            events.push(
                json!({"ph":"X","name":"present interval","cat":"survivor","pid":pid,"tid":1,
                "ts":row.elapsed_ns.saturating_sub(row.interval_ns) as f64/1000.,
                "dur":row.interval_ns as f64/1000.,"args":info}),
            );
            events.push(json!({"ph":"C","name":"CPU work (ns)","pid":pid,"tid":1,"ts":row.elapsed_ns as f64/1000.,
                "args":{"update":row.update_ns,"scene_and_draw":row.draw_ns}}));
        }
        data.flush()?;
        let mut trace = BufWriter::new(File::create(dir.join("cpu-trace.json"))?);
        serde_json::to_writer(
            &mut trace,
            &json!({"traceEvents":events,"displayTimeUnit":"ms","session":m.session}),
        )?;
        trace.flush()?;
        m.files
            .insert("performance".into(), "performance.ndjson".into());
        m.files.insert("cpu_trace".into(), "cpu-trace.json".into());
        m.settings["performance"] = json!({"clock":self.clock,"present_clock":"host notifications; ns units, resolution bounded by QPC frequency; GPU clock not calibrated",
            "capacity":CAPACITY,"first_presentation":self.rows.front().map(|r|r.presentation),"memory_samples":self.memories,
            "tracy_instrumented":cfg!(feature="tracy"),"native_metrics":"optional session CSV, joined by presentation index after warmup"});
        Ok(())
    }
}
