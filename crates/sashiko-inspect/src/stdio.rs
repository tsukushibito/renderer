use crate::protocol::{Request, answer};
use crate::session::Delivery;
use crate::{Session, World};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{
    Arc,
    mpsc::{self, RecvTimeoutError, TrySendError},
};
use std::thread;
use std::time::{Duration, Instant};

const MAX_LINE: usize = 16 * 1024;
const QUEUE: usize = 32;

enum Input {
    Valid(Request),
    Invalid(Value),
}

fn parse(line: &[u8]) -> Input {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return Input::Invalid(answer(Value::Null, Err("BAD_JSON")));
    };
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 128);
    let Some(id) = id else {
        return Input::Invalid(answer(Value::Null, Err("BAD_ID")));
    };
    let id = id.to_owned();
    match serde_json::from_value(value) {
        Ok(r) => Input::Valid(r),
        Err(_) => Input::Invalid(answer(json!(id), Err("BAD_REQUEST"))),
    }
}

// Consume oversized lines incrementally so the next newline restores framing.
fn read_input(mut reader: impl BufRead, tx: mpsc::SyncSender<Input>) -> io::Result<()> {
    let mut line = Vec::with_capacity(MAX_LINE);
    let mut overflow = false;
    loop {
        let chunk = reader.fill_buf()?;
        let eof = chunk.is_empty();
        let end = chunk.iter().position(|b| *b == b'\n');
        let count = end.unwrap_or(chunk.len());
        if line.len() + count > MAX_LINE {
            overflow = true
        }
        if !overflow {
            line.extend_from_slice(&chunk[..count])
        }
        reader.consume(count + usize::from(end.is_some()));
        if end.is_some() || (eof && (overflow || !line.is_empty())) {
            let input = if overflow {
                Input::Invalid(answer(Value::Null, Err("LINE_TOO_LONG")))
            } else {
                parse(&line)
            };
            if tx.send(input).is_err() {
                break;
            }
            line.clear();
            overflow = false;
        }
        if eof {
            break;
        }
    }
    Ok(())
}

/// Nonblocking connection for a host which already owns its tick/window loop.
pub struct StdioPort<F> {
    requests: mpsc::Receiver<Input>,
    out: mpsc::SyncSender<Delivery<F>>,
    finished: mpsc::Receiver<io::Result<()>>,
    eof: Arc<AtomicBool>,
    pending: Option<Delivery<F>>,
    notify_next: bool,
}

impl<F: serde::Serialize + Send + 'static> StdioPort<F> {
    pub fn open() -> Self {
        let (send, requests) = mpsc::sync_channel(QUEUE);
        let eof = Arc::new(AtomicBool::new(false));
        let closed = Arc::clone(&eof);
        thread::spawn(move || {
            if let Err(e) = read_input(io::stdin().lock(), send) {
                eprintln!("inspector input: {e}")
            }
            closed.store(true, Ordering::Release);
        });
        let (out, lines) = mpsc::sync_channel::<Delivery<F>>(QUEUE);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let result = (|| -> io::Result<()> {
                let mut stream = io::stdout().lock();
                for value in lines {
                    serde_json::to_writer(&mut stream, &value.json())?;
                    stream.write_all(b"\n")?;
                    stream.flush()?;
                }
                Ok(())
            })();
            let _ = done.send(result);
        });
        Self {
            requests,
            out,
            finished,
            eof,
            pending: None,
            notify_next: false,
        }
    }

    /// Process a bounded batch. A slow client retains its next reply and the
    /// game continues. False indicates the peer has disconnected.
    pub fn poll<W: World<Facts = F>>(&mut self, session: &mut Session<W>) -> bool {
        for _ in 0..QUEUE {
            if let Some(value) = self.pending.take() {
                match self.out.try_send(value) {
                    Ok(()) => (),
                    Err(TrySendError::Full(value)) => {
                        self.pending = Some(value);
                        return true;
                    }
                    Err(TrySendError::Disconnected(_)) => return false,
                }
            }
            if std::mem::take(&mut self.notify_next) {
                self.pending = session.notification(Instant::now());
                if self.pending.is_some() {
                    continue;
                }
            }
            match self.requests.try_recv() {
                Ok(input) => {
                    self.notify_next = true;
                    self.pending = Some(Delivery::Reply(match input {
                        Input::Valid(r) => answer(json!(r.id), session.request(r, Instant::now())),
                        Input::Invalid(v) => v,
                    }));
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.pending = session.notification(Instant::now());
                    if self.pending.is_none() {
                        return true;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => return false,
            }
        }
        true
    }
}

/// Serve one development session until stdin EOF. stdout is reserved for NDJSON.
/// The reader and writer own their I/O; the simulation thread never waits on stdout.
pub fn serve_stdio<W: World>(world: W, period: Duration) -> io::Result<()>
where
    W::Facts: Send + 'static,
{
    if period.is_zero() || period > Duration::from_secs(1) {
        return Err(io::Error::other("tick period must be in (0, 1s]"));
    }
    let StdioPort {
        requests,
        out,
        finished,
        eof,
        ..
    } = StdioPort::open();
    let mut session = Session::new(world);
    let mut pending = None;
    let mut blocked_since = None;
    let mut next = Instant::now() + period;
    let poll = Duration::from_millis(2);
    loop {
        let now = Instant::now();
        if !session.paused && now >= next {
            session.step();
            next = now + period;
        }
        if let Some(value) = pending.take() {
            match out.try_send(value) {
                Ok(()) => blocked_since = None,
                Err(TrySendError::Full(value)) => {
                    if eof.load(Ordering::Acquire)
                        && now.duration_since(*blocked_since.get_or_insert(now))
                            >= Duration::from_secs(1)
                    {
                        return Err(io::Error::other("stdout stalled at EOF"));
                    }
                    pending = Some(value);
                    thread::sleep(poll);
                    continue;
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
        pending = session.notification(now);
        let notified = pending.is_some();
        // Service at least one request between successive notification batches.
        if let Some(value) = pending.take() {
            match out.try_send(value) {
                Ok(()) => (),
                Err(TrySendError::Full(value)) => {
                    pending = Some(value);
                    continue;
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
        let now = Instant::now();
        let mut delay = session.next_timeout(now);
        if !session.paused {
            delay = delay.min(next.saturating_duration_since(now))
        }
        if notified {
            delay = Duration::ZERO
        }
        match requests.recv_timeout(delay) {
            Ok(Input::Valid(r)) => {
                let id = json!(r.id);
                let paused = session.paused;
                pending = Some(Delivery::Reply(answer(
                    id,
                    session.request(r, Instant::now()),
                )));
                if paused && !session.paused {
                    next = Instant::now() + period;
                }
            }
            Ok(Input::Invalid(v)) => pending = Some(Delivery::Reply(v)),
            Err(RecvTimeoutError::Timeout) => (),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(out);
    match finished.recv_timeout(Duration::from_secs(1)) {
        Ok(Err(error)) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Ok(result) => result,
        Err(_) => Err(io::Error::other("inspector output did not drain")),
    }
}
