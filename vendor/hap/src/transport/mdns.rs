use libmdns::{Responder, Service};
use log::debug;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

use crate::pointer;

type Task = Box<dyn Future<Output = ()> + Unpin + Send>;

/// An mDNS Responder. Used to announce the Accessory's name and HAP TXT records to potential controllers.
pub struct MdnsResponder {
    config: pointer::Config,
    responder: Option<Responder>,
    service: Option<Service>,
    task: Option<Task>,
    /// Whether libmdns's task still runs. Dropping its handles after it has
    /// gone panics ("responder died"), as when a runtime shuts down, so then
    /// they are forgotten instead: there is nobody left to tell.
    alive: Arc<AtomicBool>,
}

impl MdnsResponder {
    /// Creates a new mDNS Responder.
    pub async fn new(config: pointer::Config) -> Self {
        let (responder, task) = libmdns::Responder::with_default_handle().expect("creating mDNS responder");

        MdnsResponder {
            config,
            responder: Some(responder),
            service: None,
            task: Some(task),
            alive: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Derives new mDNS TXT records from the server's `Config`.
    pub async fn update_records(&mut self) {
        debug!("attempting to set mDNS records");

        self.release_service();

        let c = self.config.lock().await;

        let name = c.name.clone();
        let port = c.port;
        let tr = c.txt_records();

        drop(c);

        let responder = self.responder.as_ref().expect("a responder until dropped");
        self.service = Some(responder.register("_hap._tcp".into(), name, port, &[
            &tr[0], &tr[1], &tr[2], &tr[3], &tr[4], &tr[5], &tr[6], &tr[7],
        ]));

        debug!("setting mDNS records: {:?}", &tr);
    }

    /// Returns the mDNS task to throw on a scheduler.
    pub fn run_handle(&mut self) -> Task {
        let task = match self.task.take() {
            Some(task) => task,
            // if the task handle is gone, recreate the whole responder
            None => {
                let (responder, task) = libmdns::Responder::with_default_handle().expect("creating mDNS responder");
                self.release_service();
                let old = self.responder.replace(responder);
                self.release(old);
                self.alive = Arc::new(AtomicBool::new(true));

                task
            },
        };
        Box::new(Alive {
            task,
            alive: self.alive.clone(),
        })
    }

    fn release_service(&mut self) {
        let service = self.service.take();
        self.release(service);
    }

    fn release<T>(&self, handle: Option<T>) {
        if self.alive.load(Ordering::SeqCst) {
            drop(handle);
        } else {
            std::mem::forget(handle);
        }
    }
}

impl Drop for MdnsResponder {
    fn drop(&mut self) {
        self.release_service();
        let responder = self.responder.take();
        self.release(responder);
    }
}

/// libmdns's task, which says when it has gone.
struct Alive {
    task: Task,
    alive: Arc<AtomicBool>,
}

impl Future for Alive {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.task).poll(cx)
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
    }
}
