use std::time::{Duration, Instant};

use crate::tui::Tui;

/// Render a busy screen, run a blocking operation, return result + elapsed.
pub fn run_with_busy<T>(
    terminal: &mut Tui,
    label: &str,
    operation: impl FnOnce() -> T,
) -> (T, Duration) {
    crate::ui::screens::common::render_busy(terminal, label).ok();
    let started = Instant::now();
    let result = operation();
    (result, started.elapsed())
}

pub fn elapsed_message(label: &str, elapsed: Duration) -> String {
    format!("{label} · 耗时 {:.2}s", elapsed.as_secs_f64())
}
