//! The hold logic itself, free of any OpenXR plumbing so it can be unit tested.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// A pose only counts as good after this many consecutive valid frames.
    /// Right after tracking comes back the reported pose is still wrong for a
    /// few frames, which makes the hand snap; those frames are held as well.
    pub min_valid_frames: u32,
    /// Stop holding after this long without tracking. `None` holds forever.
    pub max_hold: Option<Duration>,
    /// Also keep the controller "connected" (interaction profile and pose
    /// action stay active) while the runtime reports it gone.
    pub hold_connection: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_valid_frames: 5,
            max_hold: None,
            hold_connection: true,
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Some(n) = env("POSE_HOLDER_MIN_VALID_FRAMES").and_then(|v| v.parse().ok()) {
            cfg.min_valid_frames = n;
        }
        if let Some(secs) = env("POSE_HOLDER_MAX_HOLD_SECS").and_then(|v| v.parse::<f64>().ok()) {
            cfg.max_hold = (secs > 0.0 && secs.is_finite()).then(|| Duration::from_secs_f64(secs));
        }
        if let Some(v) = env("POSE_HOLDER_HOLD_CONNECTION") {
            cfg.hold_connection = !matches!(v.as_str(), "0" | "false" | "no" | "off");
        }
        cfg
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_lowercase())
}

/// What to do with the value the runtime just returned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decision<T> {
    /// Hand the runtime's value to the application unchanged.
    PassThrough,
    /// Replace it with this earlier good value.
    Hold(T),
}

/// Tracks one value (a pose, an interaction profile, an "active" flag) and
/// decides when the last good one should be replayed.
#[derive(Debug)]
pub struct Holder<T> {
    last_good: Option<T>,
    valid_frames: u32,
    last_frame: Option<i64>,
    lost_since: Option<Instant>,
    holding: bool,
}

impl<T> Default for Holder<T> {
    fn default() -> Self {
        Self {
            last_good: None,
            valid_frames: 0,
            last_frame: None,
            lost_since: None,
            holding: false,
        }
    }
}

impl<T: Copy> Holder<T> {
    /// `frame` identifies the frame being queried (the display time), so that
    /// asking twice within one frame does not count as two valid frames.
    /// `value` is `Some` when the runtime reported something valid.
    pub fn update(
        &mut self,
        cfg: &Config,
        min_valid_frames: u32,
        frame: i64,
        value: Option<T>,
        now: Instant,
    ) -> Decision<T> {
        let new_frame = self.last_frame != Some(frame);
        self.last_frame = Some(frame);

        let trusted = match value {
            Some(_) => {
                if new_frame || self.valid_frames == 0 {
                    self.valid_frames = self.valid_frames.saturating_add(1);
                }
                self.valid_frames > min_valid_frames
            }
            None => {
                self.valid_frames = 0;
                false
            }
        };

        self.holding = false;
        if trusted {
            self.last_good = value;
            self.lost_since = None;
            return Decision::PassThrough;
        }

        let Some(last) = self.last_good else {
            return Decision::PassThrough;
        };
        let lost_since = *self.lost_since.get_or_insert(now);
        match cfg.max_hold {
            Some(max) if now.duration_since(lost_since) > max => Decision::PassThrough,
            _ => {
                self.holding = true;
                Decision::Hold(last)
            }
        }
    }

    /// Whether the most recent update replayed an earlier value.
    pub fn is_holding(&self) -> bool {
        self.holding
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(h: &mut Holder<u32>, cfg: &Config, frame: i64, v: Option<u32>) -> Decision<u32> {
        h.update(cfg, cfg.min_valid_frames, frame, v, Instant::now())
    }

    #[test]
    fn holds_last_good_value_and_ignores_first_frames_after_recovery() {
        let cfg = Config::default();
        let mut h = Holder::default();

        // Nothing to hold yet: whatever the runtime says goes through.
        assert_eq!(step(&mut h, &cfg, 0, None), Decision::PassThrough);
        for f in 1..=6 {
            assert_eq!(step(&mut h, &cfg, f, Some(f as u32)), Decision::PassThrough);
        }
        // Tracking lost: the last trusted value (frame 6) is replayed.
        assert_eq!(step(&mut h, &cfg, 7, None), Decision::Hold(6));
        assert_eq!(step(&mut h, &cfg, 8, None), Decision::Hold(6));
        // Valid again, but the first 5 frames are not trusted yet.
        for f in 9..=13 {
            assert_eq!(step(&mut h, &cfg, f, Some(100)), Decision::Hold(6));
        }
        assert_eq!(step(&mut h, &cfg, 14, Some(101)), Decision::PassThrough);
        assert_eq!(step(&mut h, &cfg, 15, None), Decision::Hold(101));
    }

    #[test]
    fn repeated_queries_in_one_frame_count_once() {
        let cfg = Config::default();
        let mut h = Holder::default();
        for f in 1..=6 {
            step(&mut h, &cfg, f, Some(1));
        }
        step(&mut h, &cfg, 7, None);
        for _ in 0..20 {
            assert_eq!(step(&mut h, &cfg, 8, Some(2)), Decision::Hold(1));
        }
    }

    #[test]
    fn gives_up_after_max_hold() {
        let cfg = Config {
            max_hold: Some(Duration::from_secs(2)),
            ..Config::default()
        };
        let mut h = Holder::default();
        let t0 = Instant::now();
        assert_eq!(h.update(&cfg, 0, 1, Some(7), t0), Decision::PassThrough);
        assert_eq!(h.update(&cfg, 0, 2, None, t0), Decision::Hold(7));
        let later = t0 + Duration::from_secs(3);
        assert_eq!(h.update(&cfg, 0, 3, None, later), Decision::PassThrough);
        // Comes back immediately once valid (min_valid_frames = 0 here).
        assert_eq!(h.update(&cfg, 0, 4, Some(8), later), Decision::PassThrough);
        assert_eq!(h.update(&cfg, 0, 5, None, later), Decision::Hold(8));
    }
}
