//! When a machine that runs programmes has finished one.
//!
//! Machines say so unreliably: some flag the end, some go straight to off.
//! A finish here is a run that was seen and ended without being stopped,
//! or that the machine flagged. It holds until a new run starts or someone
//! opens the door, so a sensor Home notifies on goes off once per cycle.

#[derive(Debug, Default)]
pub struct Cycle {
    running: bool,
    stopped: bool,
    finished: bool,
}

/// One look at the machine.
#[derive(Debug, Default, Clone, Copy)]
pub struct Seen {
    pub running: bool,
    /// Being stopped, so the end that follows is not a finish.
    pub stopping: bool,
    /// The machine's own word that the programme finished.
    pub flagged: bool,
    /// The door was opened, which is someone at the machine.
    pub opened: bool,
}

impl Cycle {
    /// Whether a programme has finished, as of this look.
    pub fn see(&mut self, seen: Seen) -> bool {
        if seen.running {
            self.running = true;
            self.stopped = false;
            self.finished = false;
        }
        if seen.stopping {
            self.stopped = true;
        }
        let ended = self.running && (!seen.running || seen.flagged);
        if ended {
            self.running = false;
            self.finished = !self.stopped;
        }
        if seen.opened {
            self.finished = false;
        }
        self.finished
    }

    pub fn finished(&self) -> bool {
        self.finished
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNING: Seen = Seen {
        running: true,
        stopping: false,
        flagged: false,
        opened: false,
    };
    const IDLE: Seen = Seen {
        running: false,
        stopping: false,
        flagged: false,
        opened: false,
    };

    #[test]
    fn a_run_that_ends_has_finished_until_the_door_opens() {
        let mut cycle = Cycle::default();
        assert!(!cycle.see(RUNNING));
        assert!(cycle.see(IDLE));
        assert!(cycle.see(IDLE));
        assert!(!cycle.see(Seen {
            opened: true,
            ..IDLE
        }));
    }

    #[test]
    fn the_machines_flag_ends_a_run_at_once() {
        let mut cycle = Cycle::default();
        cycle.see(RUNNING);
        assert!(cycle.see(Seen {
            flagged: true,
            ..RUNNING
        }));
    }

    #[test]
    fn a_stopped_run_and_a_machine_found_finished_are_not_finishes() {
        let mut cycle = Cycle::default();
        assert!(!cycle.see(Seen {
            flagged: true,
            ..IDLE
        }));
        cycle.see(RUNNING);
        cycle.see(Seen {
            stopping: true,
            ..RUNNING
        });
        assert!(!cycle.see(IDLE));
        cycle.see(RUNNING);
        assert!(cycle.see(IDLE), "a new run that ends is a finish again");
    }
}
