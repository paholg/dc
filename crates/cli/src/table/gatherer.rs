//! Batched data source for table cells.
//!
//! A [`Gatherer`] polls `fetch` on an interval and publishes each snapshot over
//! a `watch`. Any number of cheap [`ValueSource`]s ([`Gatherer::cell`]) project
//! from it on read, so one `fetch` feeds many cells. It always polls; whether
//! the table keeps reading is the table's concern.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::watch;

use super::{Datum, ValueSource};

pub(crate) struct Gatherer<S> {
    rx: watch::Receiver<Arc<S>>,
}

// Clones regardless of `S: Clone` (only the receiver is cloned).
impl<S> Clone for Gatherer<S> {
    fn clone(&self) -> Self {
        Gatherer {
            rx: self.rx.clone(),
        }
    }
}

/// The write half of a [`Gatherer::progressive`] source: a task's handle for
/// publishing its snapshot as it fills in.
pub(crate) struct Publisher<S> {
    tx: watch::Sender<Arc<S>>,
    current: S,
}

impl<S: Clone + Send + Sync + 'static> Publisher<S> {
    /// Mutate the snapshot and publish it. Every cell reading this gatherer
    /// picks up the change on its next read.
    pub(crate) fn update(&mut self, f: impl FnOnce(&mut S)) {
        f(&mut self.current);
        let _ = self.tx.send(Arc::new(self.current.clone()));
    }
}

impl<S: Default + Send + Sync + 'static> Gatherer<S> {
    /// Poll `fetch` every `period`, publishing each snapshot. First poll fires
    /// immediately; the task ends once every derived cell is dropped.
    pub(crate) fn spawn<F, Fut>(period: Duration, fetch: F) -> Self
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = S> + Send + 'static,
    {
        let (tx, rx) = watch::channel(Arc::new(S::default()));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(period);
            loop {
                ticker.tick().await;
                let snapshot = fetch().await;
                if tx.send(Arc::new(snapshot)).is_err() {
                    break;
                }
            }
        });
        Gatherer { rx }
    }

    /// A gatherer fed by a task that publishes as it goes, rather than by a
    /// poll loop. For a sequence of dependent steps (resolve, then connect,
    /// then handshake, …) where each one's result should reach the screen as
    /// soon as it's known instead of at the end.
    pub(crate) fn progressive<F, Fut>(run: F) -> Self
    where
        S: Clone,
        F: FnOnce(Publisher<S>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let (tx, rx) = watch::channel(Arc::new(S::default()));
        tokio::spawn(async move {
            run(Publisher {
                tx,
                current: S::default(),
            })
            .await;
        });
        Gatherer { rx }
    }

    /// The latest published snapshot. For callers that consume the values
    /// directly rather than rendering them.
    pub(crate) fn snapshot(&self) -> Arc<S> {
        self.rx.borrow().clone()
    }

    /// A gatherer that recomputes each time this one publishes, not on its own
    /// timer. For sources that depend on another's output (e.g. stats keyed by
    /// the ids this gatherer discovered) and should run the moment it finishes.
    pub(crate) fn derive<V, F, Fut>(&self, transform: F) -> Gatherer<V>
    where
        F: Fn(Arc<S>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = V> + Send + 'static,
        V: Default + Send + Sync + 'static,
    {
        let mut rx = self.rx.clone();
        let (tx, out) = watch::channel(Arc::new(V::default()));
        tokio::spawn(async move {
            loop {
                let snapshot = rx.borrow_and_update().clone();
                if tx.send(Arc::new(transform(snapshot).await)).is_err() {
                    break;
                }
                if rx.changed().await.is_err() {
                    break;
                }
            }
        });
        Gatherer { rx: out }
    }

    /// A cell that picks a value out of each snapshot on read. No task, no
    /// re-poll.
    pub(crate) fn cell<V>(
        &self,
        pick: impl Fn(&S) -> Datum<V> + Send + Sync + 'static,
    ) -> ValueSource<V>
    where
        V: Send + 'static,
    {
        let pick = Arc::new(pick);

        let get = {
            let rx = self.rx.clone();
            let pick = pick.clone();
            Arc::new(move || pick(&rx.borrow())) as Arc<dyn Fn() -> Datum<V> + Send + Sync>
        };

        let ready = {
            let rx = self.rx.clone();
            Arc::new(move || {
                let mut rx = rx.clone();
                let pick = pick.clone();
                Box::pin(async move {
                    let _ = rx.wait_for(|s| !matches!(pick(s), Datum::Pending)).await;
                }) as BoxFuture<'static, ()>
            }) as Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>
        };

        ValueSource::new(get, ready)
    }
}
