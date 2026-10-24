use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration as StdDuration, Instant};

use chrono::{Local, NaiveDateTime};
use rand::Rng;

use crate::engine::{Engine, Event};

/// 在 [min_seconds, max_seconds] 内均匀取一个随机间隔(秒),用于"约 N 分钟
/// 内随机探测一次"的常驻轮询,避免固定节律。max_seconds 需不小于 min_seconds。
pub fn random_interval_secs(min_seconds: u64, max_seconds: u64) -> StdDuration {
    let min_seconds = min_seconds.min(max_seconds);
    let span = max_seconds.saturating_sub(min_seconds);
    let offset = rand::rng().random_range(0..=span);
    StdDuration::from_secs(min_seconds + offset)
}

pub struct Runner {
    engine: Engine,
    interval: StdDuration,
}

impl Runner {
    pub fn new(engine: Engine, interval: StdDuration) -> Self {
        Runner { engine, interval }
    }

    pub fn run(&mut self, emit: &mut dyn FnMut(Event)) {
        self.run_until(emit, None);
    }

    pub fn run_until(&mut self, emit: &mut dyn FnMut(Event), deadline: Option<Instant>) {
        loop {
            if !self.engine.running() {
                return;
            }
            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                return;
            }
            let now = Local::now().naive_local();
            for event in self.engine.tick(now) {
                emit(event);
            }
            thread::sleep(self.next_sleep(Local::now().naive_local()));
        }
    }

    pub fn run_with_stop(&mut self, emit: &mut dyn FnMut(Event), stop: &AtomicBool) {
        loop {
            if stop.load(Ordering::Relaxed) || !self.engine.running() {
                return;
            }
            let now = Local::now().naive_local();
            for event in self.engine.tick(now) {
                emit(event);
            }
            thread::sleep(self.next_sleep(Local::now().naive_local()));
        }
    }

    /// 常驻轮询:每轮由外部回调决定本轮随机/固定间隔,适合"上课时间高强度、
    /// 其余时间保活"的调度。`after_tick` 在每轮引擎 tick 后调用(可做持久化),
    /// 引擎因登录失效等原因自行停止后本方法返回。
    pub fn run_dynamic(
        &mut self,
        emit: &mut dyn FnMut(Event),
        next_sleep: &mut dyn FnMut() -> StdDuration,
        mut after_tick: impl FnMut(&mut Engine),
    ) {
        loop {
            if !self.engine.running() {
                return;
            }
            let now = Local::now().naive_local();
            for event in self.engine.tick(now) {
                emit(event);
            }
            if !self.engine.running() {
                return;
            }
            after_tick(&mut self.engine);
            // 引擎内部有到期待办(延迟签到/重试)时,优先按时唤醒,其次按调度间隔
            let mut sleep = next_sleep();
            if let Some(remaining) = self.engine.earliest_due(Local::now().naive_local()) {
                sleep = sleep.min(remaining);
            }
            if sleep.is_zero() {
                sleep = StdDuration::from_millis(500);
            }
            thread::sleep(sleep);
        }
    }

    /// 常驻随机轮询:每轮引擎 tick 后随机睡 [min_seconds, max_seconds] 秒再进入下一轮。
    /// 引擎因登录失效等原因自行停止后本方法返回。
    pub fn run_random(
        &mut self,
        emit: &mut dyn FnMut(Event),
        min_seconds: u64,
        max_seconds: u64,
    ) {
        self.run_dynamic(emit, &mut || {
            random_interval_secs(min_seconds, max_seconds)
        }, |_| {});
    }

    fn next_sleep(&self, now: NaiveDateTime) -> StdDuration {
        match self.engine.earliest_due(now) {
            Some(remaining) => remaining.min(self.interval),
            None => self.interval,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_interval_stays_within_bounds() {
        for _ in 0..500 {
            let interval = random_interval_secs(100, 140);
            assert!(interval >= StdDuration::from_secs(100));
            assert!(interval <= StdDuration::from_secs(140));
        }
    }

    #[test]
    fn random_interval_covers_many_values() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2000 {
            seen.insert(random_interval_secs(0, 40).as_secs());
        }
        assert!(seen.len() > 30, "随机分布过窄: {}", seen.len());
    }

    #[test]
    fn random_interval_handles_inverted_range() {
        let interval = random_interval_secs(140, 100);
        assert!(interval >= StdDuration::from_secs(100));
        assert!(interval <= StdDuration::from_secs(140));
        assert_eq!(random_interval_secs(30, 30), StdDuration::from_secs(30));
    }
}
