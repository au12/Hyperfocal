//! Decodes that several requests share, and that stop when none of them wants the
//! result any more.
//!
//! The viewer asks for a photo's RAW render, its RAW info and its clipping mask, and
//! asks again when the exposure or the tone changes: all of them wait for one
//! development. When the browser gives up on them all (the view moved on to another
//! photo), the development is cancelled and FlashRaw lets go of the cores within a few
//! milliseconds, where it used to finish a decode nobody would see while the next photo
//! waited. While any request still waits, the work carries on: one that is given up (the
//! render, while the RAW info still waits) no longer makes the others start it over.

use super::Gate;
use super::cancel::{self, CancelToken};
use anyhow::{Result, anyhow};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

type Outcome<T> = std::result::Result<Arc<T>, String>;

/// The work running per key.
pub struct Flights<K, T> {
    map: Mutex<HashMap<K, Arc<Flight<T>>>>,
    /// What the work is ("develop"), for debug mode's timings.
    name: &'static str,
}

struct Flight<T> {
    token: CancelToken,
    /// Requests waiting for the result; changed under the map's lock.
    waiters: AtomicUsize,
    /// One of them is the photo on screen: the gate lets it ahead of prefetching.
    high: AtomicBool,
    /// Wakes the flight's task when it is cancelled while it waits for the gate.
    stop: tokio::sync::Notify,
    result: tokio::sync::watch::Receiver<Option<Outcome<T>>>,
}

impl<K, T> Flights<K, T>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    T: Send + Sync + 'static,
{
    pub fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Flights { map: Mutex::new(HashMap::new()), name })
    }

    /// The result of the work for `key`. Joins the work already running for it; else,
    /// unless `cached` has the result, starts `work` on its own task: on the blocking pool
    /// once `gate` lets it, under the flight's cancel token, holding the gate's permit
    /// until it has stopped. `work` stores its result where `cached` looks, so a request
    /// that comes after the flight has ended finds it there.
    pub async fn get(
        self: &Arc<Self>,
        key: K,
        high: bool,
        gate: &Arc<Gate>,
        cached: impl FnOnce() -> Option<Arc<T>>,
        work: impl FnOnce() -> Result<Arc<T>> + Send + 'static,
    ) -> Result<Arc<T>> {
        let (flight, start) = {
            let mut map = self.map.lock();
            match map.get(&key) {
                Some(f) => {
                    f.waiters.fetch_add(1, Ordering::SeqCst);
                    if high {
                        f.high.store(true, Ordering::SeqCst);
                    }
                    (f.clone(), None)
                }
                None => {
                    // Under the lock: a flight that just ended stored its result first.
                    if let Some(v) = cached() {
                        return Ok(v);
                    }
                    let (tx, rx) = tokio::sync::watch::channel(None);
                    let f = Arc::new(Flight {
                        token: CancelToken::new(),
                        waiters: AtomicUsize::new(1),
                        high: AtomicBool::new(high),
                        stop: tokio::sync::Notify::new(),
                        result: rx,
                    });
                    map.insert(key.clone(), f.clone());
                    (f, Some(tx))
                }
            }
        };
        let _waiting = Waiting { flights: self, key: &key, flight: &flight };
        if let Some(tx) = start {
            let ended = Ended { flights: self.clone(), key: key.clone(), flight: flight.clone() };
            let gate = gate.clone();
            let name = self.name;
            tokio::spawn(async move {
                let f = &ended.flight;
                let queued = std::time::Instant::now();
                let res = tokio::select! {
                    biased;
                    // Nobody waits any more: it never starts.
                    _ = f.stop.notified() => {
                        if crate::telemetry::on() {
                            crate::telemetry::count(&format!("flight.{name}.dropped_waiting"), 1);
                        }
                        Err(cancel::Cancelled.to_string())
                    }
                    permit = gate.acquire_as(&f.high) => {
                        let token = f.token.clone();
                        // Debug mode: time waiting for a decoder slot, then the work itself.
                        if crate::telemetry::on() {
                            crate::telemetry::record(&format!("flight.{name}.wait"), queued.elapsed(), false);
                        }
                        tokio::task::spawn_blocking(move || {
                            let _permit = permit;
                            if token.is_cancelled() {
                                return Err(cancel::Cancelled.to_string());
                            }
                            let started = std::time::Instant::now();
                            let r = cancel::scope(&token, work).map_err(|e| format!("{e:#}"));
                            if crate::telemetry::on() {
                                if token.is_cancelled() {
                                    // Stopped part-way: the time it ran was spent for nothing.
                                    crate::telemetry::record(&format!("flight.{name}.cancelled"), started.elapsed(), false);
                                } else {
                                    crate::telemetry::record(&format!("flight.{name}"), started.elapsed(), r.is_err());
                                }
                            }
                            r
                        })
                        .await
                        .unwrap_or_else(|e| Err(format!("the decoder stopped: {e}")))
                    }
                };
                drop(ended);
                let _ = tx.send(Some(res));
            });
        }
        let mut rx = flight.result.clone();
        let res = rx.wait_for(Option::is_some).await.map_err(|_| anyhow!("the decoder stopped"))?.clone();
        res.expect("waited for a result").map_err(|e| anyhow!(e))
    }

    /// Flights running now.
    #[cfg(test)]
    pub fn running(&self) -> usize {
        self.map.lock().len()
    }

    /// Flights running now (debug mode's samples).
    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    /// Requests waiting for the flight of `key`.
    #[cfg(test)]
    fn waiting(&self, key: &K) -> usize {
        self.map.lock().get(key).map_or(0, |f| f.waiters.load(Ordering::SeqCst))
    }
}

/// One request waiting for a flight. The last one to go cancels it, unless it has ended.
struct Waiting<'a, K: Clone + Eq + Hash, T> {
    flights: &'a Flights<K, T>,
    key: &'a K,
    flight: &'a Arc<Flight<T>>,
}

impl<K: Clone + Eq + Hash, T> Drop for Waiting<'_, K, T> {
    fn drop(&mut self) {
        let mut map = self.flights.map.lock();
        if self.flight.waiters.fetch_sub(1, Ordering::SeqCst) == 1 && map.get(self.key).is_some_and(|f| Arc::ptr_eq(f, self.flight)) {
            map.remove(self.key);
            self.flight.token.cancel();
            self.flight.stop.notify_one();
        }
    }
}

/// Takes a flight off the map when its task ends, however it ends (later requests start
/// afresh, and find a result in the cache).
struct Ended<K: Clone + Eq + Hash, T> {
    flights: Arc<Flights<K, T>>,
    key: K,
    flight: Arc<Flight<T>>,
}

impl<K: Clone + Eq + Hash, T> Drop for Ended<K, T> {
    fn drop(&mut self) {
        let mut map = self.flights.map.lock();
        if map.get(&self.key).is_some_and(|f| Arc::ptr_eq(f, &self.flight)) {
            map.remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Work that runs until it is cancelled or `ms` have passed, counting its starts and
    /// noting whether it was cancelled.
    fn work(ms: u64, starts: &Arc<AtomicUsize>, cancelled: &Arc<AtomicBool>) -> impl FnOnce() -> Result<Arc<u32>> + Send + 'static {
        let (starts, cancelled) = (starts.clone(), cancelled.clone());
        move || {
            starts.fetch_add(1, Ordering::SeqCst);
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(ms) {
                if cancel::check().is_err() {
                    cancelled.store(true, Ordering::SeqCst);
                    return Err(cancel::Cancelled.into());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(Arc::new(7))
        }
    }

    fn counters() -> (Arc<AtomicUsize>, Arc<AtomicBool>) {
        (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn requests_share_one_run() {
        let (flights, gate) = (Flights::<u8, u32>::new("test"), Arc::new(Gate::new(1)));
        let (starts, cancelled) = counters();
        let a = flights.get(1, true, &gate, || None, work(150, &starts, &cancelled));
        let b = flights.get(1, true, &gate, || None, work(150, &starts, &cancelled));
        let (a, b) = tokio::join!(a, b);
        assert_eq!((*a.unwrap(), *b.unwrap()), (7, 7));
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(!cancelled.load(Ordering::SeqCst));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(flights.running(), 0);
    }

    /// Polls `f` until it holds (or a few seconds have passed).
    async fn until(f: impl Fn() -> bool) {
        for _ in 0..500 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_last_request_to_go_cancels_it() {
        let (flights, gate) = (Flights::<u8, u32>::new("test"), Arc::new(Gate::new(1)));
        let (starts, cancelled) = counters();
        let request = tokio::spawn({
            let (flights, gate, w) = (flights.clone(), gate.clone(), work(10_000, &starts, &cancelled));
            async move { flights.get(1, true, &gate, || None, w).await }
        });
        until(|| starts.load(Ordering::SeqCst) == 1).await;
        // The browser gives up: the request is dropped at its `.await`.
        let t = Instant::now();
        request.abort();
        // The work stops within a few milliseconds, and keeps the gate until it has.
        let p = gate.acquire(true).await;
        assert!(cancelled.load(Ordering::SeqCst));
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        drop(p);
        assert_eq!(flights.running(), 0);
        // A new request starts afresh.
        let (starts2, cancelled2) = counters();
        assert_eq!(*flights.get(1, true, &gate, || None, work(10, &starts2, &cancelled2)).await.unwrap(), 7);
        assert_eq!(starts2.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn carries_on_while_anyone_waits() {
        let (flights, gate) = (Flights::<u8, u32>::new("test"), Arc::new(Gate::new(1)));
        let (starts, cancelled) = counters();
        let request = |ms| {
            let (flights, gate, w) = (flights.clone(), gate.clone(), work(ms, &starts, &cancelled));
            tokio::spawn(async move { flights.get(1, true, &gate, || None, w).await })
        };
        let first = request(400);
        until(|| starts.load(Ordering::SeqCst) == 1).await;
        let second = request(400);
        until(|| flights.waiting(&1) == 2).await;
        first.abort();
        assert_eq!(*second.await.unwrap().unwrap(), 7);
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(!cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_while_waiting_for_the_gate() {
        let (flights, gate) = (Flights::<u8, u32>::new("test"), Arc::new(Gate::new(1)));
        let held = gate.acquire(true).await;
        let (starts, cancelled) = counters();
        let r = tokio::time::timeout(Duration::from_millis(50), flights.get(1, true, &gate, || None, work(10, &starts, &cancelled))).await;
        assert!(r.is_err());
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(held);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(starts.load(Ordering::SeqCst), 0, "never started");
        assert_eq!(flights.running(), 0);
        // The given-up wait left no high-priority count behind: low priority still gets in.
        assert!(tokio::time::timeout(Duration::from_secs(1), gate.acquire(false)).await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cached_result_needs_no_flight() {
        let (flights, gate) = (Flights::<u8, u32>::new("test"), Arc::new(Gate::new(1)));
        let (starts, cancelled) = counters();
        assert_eq!(*flights.get(1, true, &gate, || Some(Arc::new(9)), work(10, &starts, &cancelled)).await.unwrap(), 9);
        assert_eq!(starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn errors_reach_every_waiter() {
        let (flights, gate) = (Flights::<u8, u32>::new("test"), Arc::new(Gate::new(1)));
        let fail = || -> Result<Arc<u32>> {
            std::thread::sleep(Duration::from_millis(50));
            Err(anyhow!("not a RAW file"))
        };
        let (a, b) = tokio::join!(flights.get(1, true, &gate, || None, fail), flights.get(1, true, &gate, || None, fail));
        assert_eq!(a.unwrap_err().to_string(), "not a RAW file");
        assert_eq!(b.unwrap_err().to_string(), "not a RAW file");
    }
}
