//! What every device's task shares, so each behaves the same whatever its
//! protocol:
//!
//! - its state is one watched value, published as a snapshot whenever it
//!   changes ([`published`]);
//! - requests from every surface are answered in turn ([`answer`]);
//! - its connection is kept up, and between sessions the device shows as
//!   offline, with anything asked of it then refused, not sent late when it
//!   comes back ([`keep_connected`]). A scene that ran an hour ago must not
//!   act now.

use crate::{
    capabilities::{Set, Snapshot},
    driver::Context,
};
use anyhow::Result;
use futures_util::future::BoxFuture;
use std::{future::Future, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};

/// The first wait before connecting again, doubled after each failure.
const FIRST: Duration = Duration::from_secs(2);
/// The longest wait between tries.
const MOST: Duration = Duration::from_secs(30);
/// A session that lasted this long was up, so the next try starts afresh.
const UP: Duration = Duration::from_secs(60);

/// A device's state, starting at `initial`, published as `show` makes it
/// whenever it changes. Publishing ends with the last sender.
pub fn published<S: Send + Sync + 'static>(
    context: &Context,
    initial: S,
    mut show: impl FnMut(&S) -> Snapshot + Send + 'static,
) -> watch::Sender<S> {
    let (states, mut state) = watch::channel(initial);
    context.publish(show(&state.borrow_and_update()));
    let context = context.clone();
    tokio::spawn(async move {
        while state.changed().await.is_ok() {
            let snapshot = show(&state.borrow_and_update());
            context.publish(snapshot);
        }
    });
    states
}

/// Answers each of `requests` in turn with what `answer` makes of it.
pub fn answer<F, Fut>(mut requests: mpsc::Receiver<Set>, mut answer: F)
where
    F: FnMut(&Set) -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send,
{
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            let result = answer(&request).await;
            request.finish(result);
        }
    });
}

/// Runs `session` again and again, until nothing sends `commands` any more.
/// After a session that failed it waits longer each time, up to half a
/// minute; after one that was up a while it tries again soon. Between
/// sessions `offline` marks the state, and commands sent meanwhile are
/// dropped: a session takes only what is asked while it is connected.
pub async fn keep_connected<S, C: std::fmt::Debug>(
    name: &str,
    states: &watch::Sender<S>,
    offline: impl Fn(&mut S),
    commands: &mut mpsc::Receiver<C>,
    mut session: impl for<'a> FnMut(&'a mut mpsc::Receiver<C>) -> BoxFuture<'a, Result<()>>,
) {
    let mut delay = FIRST;
    loop {
        while let Ok(command) = commands.try_recv() {
            log::info!("{name}: not sent, it was not connected: {command:?}");
        }
        let began = Instant::now();
        let ended = session(commands).await;
        match &ended {
            Ok(()) => log::info!("{name}: connection closed"),
            Err(error) => log::warn!("{name}: {error:#}"),
        }
        states.send_modify(&offline);
        if commands.is_closed() {
            break;
        }
        if ended.is_ok() || began.elapsed() >= UP {
            delay = FIRST;
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MOST);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::sync::{Arc, Mutex};

    #[tokio::test(start_paused = true)]
    async fn failures_wait_longer_and_a_session_that_was_up_starts_afresh() {
        let (states, _state) = watch::channel(true);
        let (sender, mut commands) = mpsc::channel::<u8>(4);
        let tries = Arc::new(Mutex::new(Vec::new()));
        let start = Instant::now();
        let mut sender = Some(sender);
        keep_connected(
            "test",
            &states,
            |online| *online = false,
            &mut commands,
            |_| {
                let tries = {
                    let mut tries = tries.lock().unwrap();
                    tries.push(start.elapsed().as_secs());
                    tries.len()
                };
                if tries == 6 {
                    sender.take();
                }
                Box::pin(async move {
                    // Up a while, then closed: the next try comes soon.
                    if tries == 4 {
                        tokio::time::sleep(UP).await;
                        return Ok(());
                    }
                    Err(anyhow!("refused"))
                })
            },
        )
        .await;
        // 0, then after 2, 4 and 8 seconds; up for 60; then after 2 and 4.
        assert_eq!(*tries.lock().unwrap(), [0, 2, 6, 14, 76, 80]);
        assert!(!*states.borrow());
    }

    #[tokio::test(start_paused = true)]
    async fn what_was_asked_while_it_was_down_is_not_sent() {
        let (states, _state) = watch::channel(());
        let (sender, mut commands) = mpsc::channel::<u8>(4);
        sender.send(1).await.unwrap();
        let got = Arc::new(Mutex::new(Vec::new()));
        let mut sender = Some(sender);
        keep_connected(
            "test",
            &states,
            |_| {},
            &mut commands,
            |commands| {
                // Asked while connected: this one goes.
                if let Some(sender) = sender.take() {
                    sender.try_send(2).unwrap();
                }
                while let Ok(command) = commands.try_recv() {
                    got.lock().unwrap().push(command);
                }
                Box::pin(async { Ok(()) })
            },
        )
        .await;
        assert_eq!(*got.lock().unwrap(), [2]);
    }
}
