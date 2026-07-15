//! The clock transport: run state, auto-advance cadence, and the tick/stop
//! behavior that drives a `Circuit`'s sequential state.
//!
//! `Clock` owns *only* the transport state; the circuit it advances is passed
//! in. The GUI's app-level wrappers (`OsmilogApp::tick_once`/`stop_clock`/
//! `advance_clock`) bridge the returned settle result into the app-global error
//! label - see there.

use crate::gui::app::InteractionMode;
use crate::gui::document::Document;
use crate::sim::circuit::{Circuit, SettleError};
use crate::sim::command::Command;

// The clock transport's run state. Editing is locked whenever this is not
// `Stopped` (see OsmilogApp::editing_locked) - the whole run session (Play →
// Pause → …) is read-only, and only Stop returns to an editable circuit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClockRun {
    // Idle and editable; the clock is not advancing (initial state).
    Stopped,
    // Auto-advancing at `ticks_per_second`; editing locked.
    Playing,
    // Frozen mid-run with sequential state preserved; editing locked.
    Paused,
}

// Clock transport state: the run mode plus the auto-advance speed and the
// egui frame-clock timestamp of the last auto-tick. See OsmilogApp::logic for
// the auto-advance loop and show_clock_controls for the Play/Pause/Step/Stop UI.
pub struct Clock {
    pub(crate) run: ClockRun,
    // Auto-advance rate in ticks per real second (Playing only).
    pub(crate) ticks_per_second: f32,
    // ctx.input(|i| i.time) value when the last auto-tick fired. Chosen over
    // std::time::Instant, which panics on wasm32.
    pub(crate) last_tick_time: f64,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            run: ClockRun::Stopped,
            ticks_per_second: 1.0,
            last_tick_time: 0.0,
        }
    }
}

impl Clock {
    fn interval(&self) -> f64 {
        1.0 / self.ticks_per_second.max(0.001) as f64
    }

    // Advances the clock exactly one tick, untracked (bypassing the Command/undo
    // layer) so it never lands on the undo stack - clock stepping is a simulation
    // step, not a structural edit. Used by both the Step button and the
    // auto-advance loop, via OsmilogApp::tick_once.
    pub(crate) fn step(&mut self, circuit: &mut Circuit) -> Result<(), SettleError> {
        circuit.apply(Command::TickClock).0.unwrap_settle()
    }

    // Stops the clock: resets all sequential state to its power-on value
    // (untracked, like a tick) and returns to the editable Stopped state.
    pub(crate) fn stop(&mut self, circuit: &mut Circuit) -> Result<(), SettleError> {
        let result = circuit.apply(Command::ResetSequential).0.unwrap_settle();
        self.run = ClockRun::Stopped;
        result
    }

    // Begins (from Stopped) or resumes (from Paused) auto-advancing, anchoring
    // the cadence at `now` (the egui frame-clock time).
    pub(crate) fn play(&mut self, now: f64) {
        self.run = ClockRun::Playing;
        self.last_tick_time = now;
    }

    // Freezes mid-run, preserving sequential state (stays editing-locked).
    pub(crate) fn pause(&mut self) {
        self.run = ClockRun::Paused;
    }

    // Auto-advances the clock while Playing, given the current frame time `now`.
    // Uses a fixed-timestep accumulator (`ticks_due`) that fires every interval
    // elapsed this frame - not just one - so a late or coalesced repaint doesn't
    // skip ticks. Returns `Some(result)` of the *last* tick fired this frame (for
    // the caller to record), or `None` if no tick was due. A tick that fails to
    // settle auto-pauses so we don't hammer a broken circuit every frame; on that
    // path we return the failing result without requesting a further repaint.
    // `request_repaint` is called with the delay until the next tick boundary so
    // the frame loop stays alive between ticks (the app is otherwise reactive).
    pub(crate) fn advance(
        &mut self,
        circuit: &mut Circuit,
        now: f64,
        request_repaint: impl FnOnce(f64),
    ) -> Option<Result<(), SettleError>> {
        if self.run != ClockRun::Playing {
            return None;
        }
        let interval = self.interval();
        let (n_ticks, next) = ticks_due(now, self.last_tick_time, interval);
        self.last_tick_time = next;

        let mut last = None;
        for _ in 0..n_ticks {
            let result = self.step(circuit);
            let failed = result.is_err();
            last = Some(result);
            if failed {
                self.run = ClockRun::Paused;
                return last;
            }
        }

        // Wake right at the next boundary (in (0, interval]), not a full interval
        // from now, so repaint timing tracks the tick schedule.
        let wait = (self.last_tick_time + interval - now).max(0.0);
        request_repaint(wait);
        last
    }
}

impl Document {
    // The clock transport: a speed setting plus Play / Pause / Step / Stop.
    // Buttons are enable-gated on the current run state (see the state table in
    // ClockRun); entering Play locks editing for the whole session and Stop
    // resets sequential state. All ticks are issued untracked (see tick_once).
    pub(crate) fn show_clock_controls(&mut self, ui: &mut egui::Ui) {
        const MAX_CLOCK_TPS: f32 = 100.0;
        let run = self.clock.run;

        // Speed is only adjustable while stopped - locked during a run session.
        ui.add_enabled(
            run == ClockRun::Stopped,
            egui::DragValue::new(&mut self.clock.ticks_per_second)
                .speed(0.1)
                .range(1.0..=MAX_CLOCK_TPS)
                .suffix(" tick/s"),
        );

        // Play: start (from Stopped) or resume (from Paused) auto-advancing.
        // Resets the auto-advance clock and abandons any in-progress placement
        // so nothing can edit mid-run.
        if ui
            .add_enabled(run != ClockRun::Playing, egui::Button::new("Play"))
            .clicked()
        {
            let now = ui.ctx().input(|i| i.time);
            self.clock.play(now);
            self.mode = InteractionMode::Idle;
        }

        // Pause: freeze mid-run, preserving sequential state (stays locked).
        if ui
            .add_enabled(run == ClockRun::Playing, egui::Button::new("Pause"))
            .clicked()
        {
            self.clock.pause();
        }

        // Step: advance exactly one tick. Available when not playing - from
        // Stopped it's a single manual tick (stays editable); from Paused it
        // nudges the frozen run forward one step.
        if ui
            .add_enabled(run != ClockRun::Playing, egui::Button::new("Step"))
            .clicked()
        {
            self.tick_once();
        }

        // Stop: halt, reset all sequential state to power-on, return to editable.
        if ui
            .add_enabled(run != ClockRun::Stopped, egui::Button::new("Stop"))
            .clicked()
        {
            self.stop_clock();
        }
    }
}

// Upper bound on ticks fired in a single frame. egui is reactive and only wakes
// via request_repaint_after, whose delivered frame gap is >= the requested one
// and jitters longer (OS timer granularity, vsync, WASM setTimeout clamping and
// background-tab throttling). A late/coalesced wake therefore covers several
// intervals at once, and all of them must fire or the sequential state skips
// values. This cap stops a genuine multi-second stall (a breakpoint, a
// backgrounded tab) from replaying a huge backlog - a "spiral of death"; past it
// we resync to `now` and drop the backlog instead.
const MAX_CATCHUP_TICKS: u32 = 8;

// Fixed-timestep accumulator for one frame of the auto-advance loop: given the
// current frame time, the reference timestamp of the last fired tick, and the
// interval, returns how many ticks are due this frame and the new reference.
// Kept pure (no egui/self) so the cadence is unit-testable.
//
// It fires *every* whole interval elapsed since `last`, not just one - a single
// late frame that spans two intervals owes two ticks, and dropping the extra is
// exactly the "frame skip" a counter shows as missing numbers. The reference
// advances by whole intervals (`last + n*interval`), preserving sub-interval
// phase so the average rate stays `1/interval` regardless of frame jitter;
// snapping it to `now` would fold each frame's overshoot into the cadence (which
// is why moving the mouse once sped ticking up). Only a backlog beyond
// MAX_CATCHUP_TICKS - a real stall, not ordinary jitter - resyncs to `now`.
fn ticks_due(now: f64, last: f64, interval: f64) -> (u32, f64) {
    if now - last < interval {
        return (0, last);
    }
    let elapsed = ((now - last) / interval).floor() as u32;
    if elapsed > MAX_CATCHUP_TICKS {
        (MAX_CATCHUP_TICKS, now)
    } else {
        (elapsed, last + elapsed as f64 * interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ticks_due_is_frame_rate_independent() {
        let interval = 0.2;

        // (n_ticks exact; reference compared with a float tolerance.)
        let check = |(n, next): (u32, f64), en: u32, enext: f64| {
            assert_eq!(n, en);
            assert!((next - enext).abs() < 1e-9, "ref {next} != {enext}");
        };

        // Interval not elapsed yet: no tick, reference unchanged.
        check(ticks_due(0.1, 0.0, interval), 0, 0.0);

        // Dense frames (mouse moving): a frame lands just past the boundary.
        // One tick; the reference advances by exactly one interval, NOT to `now`,
        // so the small overshoot doesn't accumulate into the cadence.
        check(ticks_due(0.21, 0.0, interval), 1, 0.2);
        check(ticks_due(0.216, 0.0, interval), 1, 0.2);

        // A late/coalesced frame spanning two intervals owes TWO ticks (the core
        // fix: no dropped ticks). Reference advances by two whole intervals,
        // keeping phase - the leftover 0.01 carries into the next frame.
        check(ticks_due(0.41, 0.0, interval), 2, 0.4);

        // Three intervals in one frame -> three ticks.
        check(ticks_due(0.61, 0.0, interval), 3, 0.6);

        // A genuine stall beyond the catch-up cap: fire the cap, then resync to
        // `now` and drop the backlog rather than replaying a burst.
        let (n, next) = ticks_due(100.0, 0.0, interval);
        assert_eq!(n, MAX_CATCHUP_TICKS);
        assert_eq!(next, 100.0);
    }
}
