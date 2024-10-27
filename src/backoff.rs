#[derive(Default)]
pub struct Backoff {
    #[cfg(not(loom))]
    backoff: crossbeam_utils::Backoff,
}

impl Backoff {
    pub fn spin(&self) {
        #[cfg(loom)]
        loom::thread::yield_now();

        #[cfg(not(loom))]
        self.backoff.spin();
    }

    pub fn snooze(&self) {
        #[cfg(loom)]
        self.spin();

        #[cfg(not(loom))]
        self.backoff.snooze();
    }
}
