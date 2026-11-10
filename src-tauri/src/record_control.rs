//! Last requested state wins; a single worker owns every recording pipeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Starting,
    Recording,
    Saving,
}
#[derive(Default)]
pub struct Control {
    pub phase: Phase,
    wanted: bool,
    closing: bool,
}
impl Control {
    pub fn wanted(&self) -> bool {
        self.wanted && !self.closing
    }
    pub fn toggle(&mut self) -> bool {
        if self.closing {
            return false;
        }
        self.wanted = !self.wanted;
        if self.phase == Phase::Idle && self.wanted {
            self.phase = Phase::Starting;
            true
        } else {
            false
        }
    }
    pub fn started(&mut self) -> bool {
        if self.wanted() && self.phase == Phase::Starting {
            self.phase = Phase::Recording;
            true
        } else {
            false
        }
    }
    pub fn stopping(&mut self, minimized: bool) {
        self.phase = Phase::Saving;
        if minimized {
            self.wanted = false;
        }
    }
    pub fn finish(&mut self, success: bool) -> bool {
        if !success || self.closing {
            self.wanted = false;
        }
        if self.wanted {
            self.phase = Phase::Starting;
            true
        } else {
            self.phase = Phase::Idle;
            false
        }
    }
    pub fn stop(&mut self) {
        self.wanted = false;
    }
    pub fn close(&mut self) {
        self.closing = true;
        self.wanted = false;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_during_start_does_not_acknowledge_start() {
        let mut c = Control::default();
        assert!(c.toggle());
        assert!(!c.toggle());
        assert!(!c.started());
        assert!(!c.finish(true));
        assert_eq!(c.phase, Phase::Idle);
    }
    #[test]
    fn start_during_save_runs_once_after_finish() {
        let mut c = Control::default();
        assert!(c.toggle());
        assert!(c.started());
        assert!(!c.toggle());
        c.stopping(false);
        assert!(!c.toggle());
        assert!(c.finish(true));
        assert!(c.started());
        assert_eq!(c.phase, Phase::Recording);
    }
    #[test]
    fn repeated_save_toggles_and_exit_keep_latest_intent() {
        let mut c = Control::default();
        c.toggle();
        c.started();
        c.toggle();
        c.stopping(false);
        c.toggle();
        c.toggle();
        assert!(!c.finish(true));
        assert!(c.toggle());
        c.close();
        assert!(!c.toggle());
        assert!(!c.finish(true));
    }
    #[test]
    fn failure_does_not_retry_forever() {
        let mut c = Control::default();
        c.toggle();
        c.started();
        assert!(!c.finish(false));
        assert!(!c.wanted());
        assert!(c.toggle());
    }
}
