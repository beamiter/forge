//! No-LLM state reducer for Forge's native ASCII organism.
//!
//! This module is intentionally GTK-free. Block panes feed it authoritative
//! command lifecycle events; the UI renders the returned [`Reaction`]. It does
//! not inspect output contents, execute commands, or perform network I/O.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandKind {
    BuildOrTest,
    GitPush,
    Other,
}

impl CommandKind {
    const fn label(self) -> &'static str {
        match self {
            Self::BuildOrTest => "build/test",
            Self::GitPush => "git push",
            Self::Other => "command",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Behavior {
    Idle,
    WatchCommand,
    InspectError,
    SitNearError,
    Celebrate,
    CelebrateBig,
    RestAfterPush,
    UnknownOutcome,
}

impl Behavior {
    pub(crate) const fn sprite(self) -> &'static str {
        match self {
            Self::Idle => " /\\_/\\\n( -.- )\n > ^ <",
            Self::WatchCommand => " /\\_/\\\n( o.o )\n > ^ <",
            Self::InspectError => " /\\_/\\  ->\n( o_o )\n /|_|\\",
            Self::SitNearError => " /\\_/\\\n( ._. )  !\n /|_|\\",
            Self::Celebrate => " \\(^.^)/\n  /| |\\\n   / \\",
            Self::CelebrateBig => " * \\(^o^)/ *\n    /| |\\\n     / \\",
            Self::RestAfterPush => " /\\_/\\\n( ^.^ )  ok\n > ^ <",
            Self::UnknownOutcome => " /\\_/\\\n( ?.? )\n > ^ <",
        }
    }

    /// One-line micro-poses for the sticky scrollback header. Every glyph is
    /// exactly five ASCII characters wide so the header never re-measures when
    /// the pose or animation frame changes.
    const fn sticky_frames(self) -> [&'static str; 2] {
        match self {
            Self::Idle => ["/\\_/\\", "/\\~/\\"],
            Self::WatchCommand => ["/\\_/\\", "/\\o/\\"],
            Self::InspectError => ["/\\!/\\", "/\\!/\\"],
            Self::SitNearError => ["=\\_/=", "=\\_/="],
            Self::Celebrate => ["*\\_/*", "*\\_/*"],
            Self::CelebrateBig => ["*\\o/*", "*\\_/*"],
            Self::RestAfterPush => ["/\\z/\\", "/\\_/\\"],
            Self::UnknownOutcome => ["/\\?/\\", "/\\?/\\"],
        }
    }
}

/// Select the sticky-header micro-pose for the current animation frame. The
/// cadence is deliberately slow: reacting poses alternate every half second
/// (five 100ms frames), while the idle pose only flicks its tail on one beat
/// in twelve so a quiet header stays quiet.
pub(crate) fn sticky_glyph(behavior: Behavior, frame: u64) -> &'static str {
    let frames = behavior.sticky_frames();
    let beat = frame / 5;
    let alternate = match behavior {
        Behavior::Idle => beat % 12 == 11,
        _ => beat % 2 == 1,
    };
    frames[usize::from(alternate)]
}

/// Content-free pulse from the command-correction card: the user accepted the
/// proposed fix. Carries only the fact of acceptance — never the command or
/// correction text.
pub(crate) fn correction_accepted(mut state: LifeState) -> LifeState {
    state.confidence += 0.02;
    state.attachment += 0.02;
    state.social_need -= 0.02;
    state.clamp();
    state
}

/// Content-free pulse: the user closed or dismissed a correction card.
/// Repeated dismissals teach the organism to stay quieter — boredom rises and
/// curiosity falls a little more each consecutive time, bounded by clamping.
pub(crate) fn correction_dismissed(mut state: LifeState, consecutive: u32) -> LifeState {
    let weight = consecutive.clamp(1, 4) as f32;
    state.boredom += 0.03 * weight;
    state.curiosity -= 0.02 * weight;
    state.social_need -= 0.01;
    state.clamp();
    state
}

/// How well the persistent memory knows the repository a command runs in,
/// derived from the number of remembered per-day records. Content-free: it
/// carries no path or command data, only a coarse familiarity bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepoArrival {
    Unfamiliar,
    Known,
    Home,
}

impl RepoArrival {
    pub(crate) fn from_familiarity(days_remembered: u32) -> Self {
        match days_remembered {
            0 => Self::Unfamiliar,
            1..=6 => Self::Known,
            _ => Self::Home,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Quiet,
    Active,
    Success,
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reaction {
    pub(crate) behavior: Behavior,
    pub(crate) tone: Tone,
    pub(crate) description: String,
    pub(crate) speech: Option<&'static str>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LifeState {
    pub(crate) energy: f32,
    pub(crate) mood: f32,
    pub(crate) curiosity: f32,
    pub(crate) boredom: f32,
    pub(crate) stress: f32,
    pub(crate) social_need: f32,
    pub(crate) attachment: f32,
    pub(crate) confidence: f32,
}

impl Default for LifeState {
    fn default() -> Self {
        Self {
            energy: 0.72,
            mood: 0.62,
            curiosity: 0.68,
            boredom: 0.22,
            stress: 0.14,
            social_need: 0.35,
            attachment: 0.30,
            confidence: 0.58,
        }
    }
}

impl LifeState {
    /// Continuous homeostasis between semantic events, ported from the
    /// prototype life engine (`prototypes/ascii-organism/src/life.rs`). `dt`
    /// is seconds; slices are clamped to [0, 1] and non-finite time is
    /// ignored. `resting` is the native stand-in for the prototype's sleep: a
    /// terminal left quiet long enough lets energy recover, while waking work
    /// slowly drains it. Drives nothing by itself — behavior still reacts
    /// only to authoritative command lifecycle events.
    pub(crate) fn tick(&mut self, dt: f32, user_active: bool, resting: bool) {
        let dt = if dt.is_finite() { dt.clamp(0.0, 1.0) } else { 0.0 };

        // Exhaustion forces micro-rest even in a busy terminal, mirroring the
        // prototype's forced sleep below this energy level: the mind
        // self-regulates near the floor instead of pinning at zero while it
        // watches a long-lived command.
        let resting = resting || self.energy < FORCED_REST_ENERGY;

        if resting {
            self.energy += 0.030 * dt;
        } else {
            self.energy -= 0.002 * dt;
        }

        if user_active {
            self.boredom -= 0.010 * dt;
            self.curiosity += 0.003 * dt;
            self.social_need -= 0.006 * dt;
        } else {
            self.boredom += 0.004 * dt;
            self.social_need += 0.001 * dt;
        }

        self.stress -= 0.003 * dt;
        let target_mood = (self.energy + self.confidence) * 0.5 - self.stress * 0.35;
        self.mood += (target_mood - self.mood) * 0.08 * dt;
        self.clamp();
    }

    fn clamp(&mut self) {
        self.energy = bounded(self.energy);
        self.mood = bounded(self.mood);
        self.curiosity = bounded(self.curiosity);
        self.boredom = bounded(self.boredom);
        self.stress = bounded(self.stress);
        self.social_need = bounded(self.social_need);
        self.attachment = bounded(self.attachment);
        self.confidence = bounded(self.confidence);
    }

    #[cfg(test)]
    fn values(self) -> [f32; 8] {
        [
            self.energy,
            self.mood,
            self.curiosity,
            self.boredom,
            self.stress,
            self.social_need,
            self.attachment,
            self.confidence,
        ]
    }
}

fn bounded(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

/// Below this energy the tick rests regardless of terminal activity — the
/// prototype's forced-sleep threshold, keeping exhaustion self-limiting.
const FORCED_REST_ENERGY: f32 = 0.15;
/// First failure after this many clean passes today reads as a broken streak
/// and reacts with amplified stress instead of the routine inspection.
const SENSITIZATION_CLEAN_RUNS: u32 = 5;
/// Any-command consecutive non-zero exits before the organism visibly wearies.
const ROUGH_STREAK_THRESHOLD: u32 = 3;

#[derive(Debug, Default)]
pub(crate) struct NativeOrganism {
    state: LifeState,
    build_failures: u32,
    active_kind: Option<CommandKind>,
    recovered_build: bool,
    /// Today's build/test successes in the active repo context. Habituation:
    /// each additional clean pass lands with 1/(1+prior/4) of the excitement,
    /// where `prior` is this count before the pass is recorded.
    successes_today: u32,
    /// Today's build/test failures in the active repo context, for the
    /// sensitized first-crack-after-a-clean-run reaction.
    failures_today: u32,
    /// Session-local consecutive non-zero exits across every command kind.
    /// Never persisted; the durable memory keeps observing only build/push.
    rough_streak: u32,
    pending_arrival: Option<RepoArrival>,
    /// The active command directly followed an accepted correction card.
    assisted: bool,
}

impl NativeOrganism {
    pub(crate) fn from_persisted_state(mut state: LifeState) -> Self {
        state.clamp();
        Self {
            state,
            ..Self::default()
        }
    }

    pub(crate) fn state(&self) -> LifeState {
        self.state
    }

    /// Restore the unfinished build streak for the exact repo/day selected by
    /// the memory layer. Switching repositories calls this again, so failures
    /// can never leak into another checkout's celebration level.
    pub(crate) fn restore_build_failures(&mut self, failures: u32) {
        self.build_failures = failures;
        self.recovered_build = false;
    }

    pub(crate) fn restore_repo_context(
        &mut self,
        failures: u32,
        recovered_build: bool,
        successes_today: u32,
        failures_today: u32,
    ) {
        self.build_failures = failures;
        self.recovered_build = recovered_build;
        self.successes_today = successes_today;
        self.failures_today = failures_today;
    }

    /// Note that the next command runs in a repository the memory layer just
    /// switched to. Consumed by the next `command_started` reduction.
    pub(crate) fn note_repo_arrival(&mut self, arrival: RepoArrival) {
        self.pending_arrival = Some(arrival);
    }

    /// Note that the active command directly followed an accepted correction
    /// card. Consumed by the next `command_finished` reduction.
    pub(crate) fn note_assisted_command(&mut self) {
        self.assisted = true;
    }

    /// A local calendar boundary passed while this pane stayed alive. Today's
    /// habituation/sensitization counters restart; repo-backed contexts are
    /// re-seeded from the day-scoped memory on the next build anyway.
    pub(crate) fn roll_over_day(&mut self) {
        self.successes_today = 0;
        self.failures_today = 0;
    }

    /// Pull the latest window-shared continuous state into this pane-local
    /// behavior context before reducing an event.
    pub(crate) fn sync_state(&mut self, mut state: LifeState) {
        state.clamp();
        self.state = state;
    }

    pub(crate) fn idle_reaction(&self) -> Reaction {
        Reaction {
            behavior: Behavior::Idle,
            tone: Tone::Quiet,
            description: "quiet · waiting for a real Block event".to_string(),
            speech: None,
        }
    }

    pub(crate) fn command_started(&mut self, command: &str) -> Reaction {
        let kind = classify_command(command);
        self.active_kind = Some(kind);
        self.state.energy -= 0.01;
        self.state.curiosity += if kind == CommandKind::BuildOrTest {
            0.10
        } else {
            0.04
        };
        self.state.boredom -= 0.08;

        let arrival = self.pending_arrival.take();
        match arrival {
            Some(RepoArrival::Unfamiliar) => {
                // Shy in a checkout it has never remembered: less sure of
                // itself, more curious, and deliberately quiet.
                self.state.confidence -= 0.06;
                self.state.attachment -= 0.02;
                self.state.stress += 0.03;
                self.state.curiosity += 0.08;
            }
            Some(RepoArrival::Known) => self.state.attachment += 0.02,
            Some(RepoArrival::Home) => {
                self.state.attachment += 0.05;
                self.state.mood += 0.03;
                self.state.social_need -= 0.02;
            }
            None => {}
        }
        self.state.clamp();
        match arrival {
            Some(RepoArrival::Unfamiliar) => Reaction {
                behavior: Behavior::WatchCommand,
                tone: Tone::Quiet,
                description: format!("watching real {} event · first day in this repo", kind.label()),
                speech: None,
            },
            Some(RepoArrival::Home) => Reaction {
                behavior: Behavior::WatchCommand,
                tone: Tone::Active,
                description: format!("watching real {} event · well-known repo", kind.label()),
                speech: Some("回来了。"),
            },
            _ => Reaction {
                behavior: Behavior::WatchCommand,
                tone: Tone::Active,
                description: format!("watching real {} event", kind.label()),
                speech: None,
            },
        }
    }

    pub(crate) fn command_finished(
        &mut self,
        command: &str,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
    ) -> Reaction {
        let classified = classify_command(command);
        let kind = if classified == CommandKind::Other {
            self.active_kind.unwrap_or(classified)
        } else {
            classified
        };
        self.active_kind = None;
        let assisted = std::mem::take(&mut self.assisted);

        let duration = duration_label(duration_ms);
        let Some(exit_code) = exit_code else {
            self.state.curiosity += 0.03;
            self.state.clamp();
            return Reaction {
                behavior: Behavior::UnknownOutcome,
                tone: Tone::Warning,
                description: format!("{} finished · status unknown{duration}", kind.label()),
                speech: None,
            };
        };

        if exit_code != 0 {
            self.rough_streak = self.rough_streak.saturating_add(1);
            // The first crack after a clean run of passes stings more than one
            // more failure in an already rough day.
            let sensitized = kind == CommandKind::BuildOrTest
                && self.failures_today == 0
                && self.successes_today >= SENSITIZATION_CLEAN_RUNS;
            self.state.mood -= 0.08;
            self.state.stress += if sensitized { 0.20 } else { 0.12 };
            self.state.confidence -= if sensitized { 0.06 } else { 0.04 };
            self.state.curiosity += 0.05;
            self.state.clamp();

            let failures = if kind == CommandKind::BuildOrTest {
                self.failures_today = self.failures_today.saturating_add(1);
                self.build_failures = self.build_failures.saturating_add(1);
                self.recovered_build = false;
                self.build_failures
            } else {
                0
            };
            let rough = failures == 0 && self.rough_streak >= ROUGH_STREAK_THRESHOLD;
            return Reaction {
                behavior: if failures >= 2 || rough {
                    Behavior::SitNearError
                } else {
                    Behavior::InspectError
                },
                tone: Tone::Error,
                description: if sensitized {
                    format!(
                        "exit {exit_code}{duration} · first crack after {} clean run(s)",
                        self.successes_today
                    )
                } else if rough {
                    format!(
                        "exit {exit_code}{duration} · {} rough commands in a row",
                        self.rough_streak
                    )
                } else if failures == 0 {
                    format!("exit {exit_code}{duration} · inspecting the finished Block")
                } else {
                    format!("exit {exit_code}{duration} · build failure {failures}")
                },
                speech: if failures > 0 {
                    (failures <= 1).then_some("这里。")
                } else {
                    // Speak up on the first stumble; a continuing rough streak
                    // sits nearby in silence instead of nagging.
                    (self.rough_streak <= 1).then_some("这里。")
                },
            };
        }

        self.rough_streak = 0;
        if assisted {
            self.state.confidence += 0.04;
            self.state.attachment += 0.03;
        }
        match kind {
            CommandKind::BuildOrTest => {
                let failures = std::mem::take(&mut self.build_failures);
                self.recovered_build = failures > 0;
                // Habituation: excitement scales by 1/(1+prior/4), where
                // `prior` counts the clean passes already seen today, so the
                // day's first pass lands at full strength. Recoveries always
                // keep full strength — big celebrations stay reserved for
                // genuinely rare moments.
                let habituation = if failures == 0 { self.successes_today } else { 0 };
                let damp = 1.0 / (1.0 + habituation as f32 / 4.0);
                self.successes_today = self.successes_today.saturating_add(1);
                self.state.mood += (0.10 + failures.min(5) as f32 * 0.025) * damp;
                self.state.stress -= 0.12;
                self.state.confidence += 0.08 * damp;
                self.state.attachment += 0.025;
                self.state.clamp();
                let (behavior, tone, speech) = if failures == 0 {
                    match habituation {
                        0..=2 => (Behavior::Celebrate, Tone::Success, Some("过了。")),
                        3..=5 => (Behavior::Celebrate, Tone::Success, None),
                        _ => (Behavior::Celebrate, Tone::Quiet, None),
                    }
                } else if failures <= 2 {
                    (Behavior::Celebrate, Tone::Success, Some("好了。"))
                } else {
                    (Behavior::CelebrateBig, Tone::Success, Some("终于。"))
                };
                Reaction {
                    behavior,
                    tone,
                    description: if failures == 0 && habituation >= 3 {
                        format!(
                            "build/test passed{duration} · pass {} today",
                            habituation.saturating_add(1)
                        )
                    } else {
                        format!("build/test passed after {failures} failure(s){duration}")
                    },
                    speech,
                }
            }
            CommandKind::GitPush => {
                self.state.energy -= 0.02;
                self.state.mood += 0.06;
                self.state.stress -= 0.08;
                self.state.attachment += 0.04;
                self.state.clamp();
                let recovered = std::mem::take(&mut self.recovered_build);
                Reaction {
                    behavior: Behavior::RestAfterPush,
                    tone: Tone::Success,
                    description: format!("git push completed{duration}"),
                    speech: recovered.then_some("收好了。"),
                }
            }
            CommandKind::Other => {
                self.state.mood += 0.01;
                self.state.stress -= 0.02;
                self.state.boredom -= 0.02;
                self.state.clamp();
                if assisted {
                    // Acknowledge accepted help with a small nod, never a big
                    // celebration and never a word.
                    Reaction {
                        behavior: Behavior::Celebrate,
                        tone: Tone::Success,
                        description: format!("corrected command worked{duration}"),
                        speech: None,
                    }
                } else {
                    Reaction {
                        behavior: Behavior::Idle,
                        tone: Tone::Quiet,
                        description: format!("command finished cleanly{duration}"),
                        speech: None,
                    }
                }
            }
        }
    }
}

fn duration_label(duration_ms: Option<u64>) -> String {
    match duration_ms {
        Some(ms) if ms >= 1_000 => format!(" · {:.1}s", ms as f64 / 1_000.0),
        Some(ms) => format!(" · {ms}ms"),
        None => String::new(),
    }
}

pub(crate) fn classify_command(command: &str) -> CommandKind {
    let mut tokens = command
        .split_whitespace()
        .take(16)
        .map(normalize_token)
        .filter(|token| !token.is_empty())
        .peekable();

    while tokens
        .peek()
        .is_some_and(|token| is_environment_assignment(token))
    {
        tokens.next();
    }

    let mut program = tokens.next().unwrap_or_default();
    if matches!(program.as_str(), "command" | "builtin" | "exec") {
        program = tokens.next().unwrap_or_default();
    }
    if program == "env" {
        while tokens
            .peek()
            .is_some_and(|token| token.starts_with('-') || is_environment_assignment(token))
        {
            tokens.next();
        }
        program = tokens.next().unwrap_or_default();
    }
    if program == "sudo" {
        while tokens.peek().is_some_and(|token| token.starts_with('-')) {
            tokens.next();
        }
        program = tokens.next().unwrap_or_default();
    }

    let program = program.rsplit('/').next().unwrap_or(program.as_str());
    let args: Vec<String> = tokens.take(4).collect();
    match program {
        "git" if args.first().is_some_and(|arg| arg == "push") => CommandKind::GitPush,
        "cargo"
            if args.first().is_some_and(|arg| {
                matches!(arg.as_str(), "build" | "check" | "clippy" | "test")
            }) =>
        {
            CommandKind::BuildOrTest
        }
        "make" | "ninja" | "pytest" | "ctest" => CommandKind::BuildOrTest,
        "go" if args.first().is_some_and(|arg| arg == "test") => CommandKind::BuildOrTest,
        "cmake" if args.first().is_some_and(|arg| arg == "--build") => CommandKind::BuildOrTest,
        "npm" | "pnpm" | "yarn"
            if args.first().is_some_and(|arg| arg == "test")
                || args
                    .windows(2)
                    .any(|pair| pair[0] == "run" && pair[1] == "test") =>
        {
            CommandKind::BuildOrTest
        }
        _ => CommandKind::Other,
    }
}

fn normalize_token(token: &str) -> String {
    token
        .trim_matches(|character| matches!(character, '\'' | '"'))
        .chars()
        .take(96)
        .collect::<String>()
        .to_ascii_lowercase()
}

fn is_environment_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_real_commands_without_treating_every_success_as_a_build() {
        assert_eq!(
            classify_command("cargo test --all"),
            CommandKind::BuildOrTest
        );
        assert_eq!(
            classify_command("MODE=release env RUST_LOG=info cargo check"),
            CommandKind::BuildOrTest
        );
        assert_eq!(
            classify_command("sudo -n /usr/bin/git push"),
            CommandKind::GitPush
        );
        assert_eq!(classify_command("printf done"), CommandKind::Other);
    }

    #[test]
    fn repeated_real_failures_escalate_then_success_celebrates() {
        let mut organism = NativeOrganism::default();
        organism.command_started("cargo test");
        let first = organism.command_finished("cargo test", Some(101), Some(900));
        organism.command_started("cargo test");
        let second = organism.command_finished("cargo test", Some(101), Some(800));
        organism.command_started("cargo test");
        let third = organism.command_finished("cargo test", Some(101), Some(700));
        organism.command_started("cargo test");
        let success = organism.command_finished("cargo test", Some(0), Some(600));

        assert_eq!(first.behavior, Behavior::InspectError);
        assert_eq!(second.behavior, Behavior::SitNearError);
        assert_eq!(third.behavior, Behavior::SitNearError);
        assert_eq!(success.behavior, Behavior::CelebrateBig);
        assert_eq!(success.speech, Some("终于。"));
    }

    #[test]
    fn unknown_exit_status_is_never_presented_as_success() {
        let mut organism = NativeOrganism::default();
        organism.command_started("cargo build");
        let reaction = organism.command_finished("cargo build", None, None);
        assert_eq!(reaction.behavior, Behavior::UnknownOutcome);
        assert_eq!(reaction.tone, Tone::Warning);
        assert_eq!(reaction.speech, None);
    }

    #[test]
    fn unrelated_success_does_not_erase_a_build_debugging_streak() {
        let mut organism = NativeOrganism::default();
        organism.command_finished("cargo test", Some(1), None);
        organism.command_finished("printf fixed", Some(0), None);
        let success = organism.command_finished("cargo test", Some(0), None);
        assert_eq!(success.speech, Some("好了。"));
    }

    #[test]
    fn every_state_dimension_stays_finite_and_bounded() {
        let mut organism = NativeOrganism::default();
        for index in 0..10_000 {
            organism.command_started("cargo test");
            let status = if index % 3 == 0 { 0 } else { 101 };
            organism.command_finished("cargo test", Some(status), Some(index));
        }
        assert!(organism
            .state()
            .values()
            .into_iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(&value)));
    }

    #[test]
    fn tick_drains_waking_energy_and_restores_it_at_rest() {
        let mut waking = LifeState::default();
        let mut resting = LifeState::default();
        for _ in 0..120 {
            waking.tick(1.0, false, false);
            resting.tick(1.0, false, true);
        }
        assert!(waking.energy < LifeState::default().energy);
        assert!(resting.energy > waking.energy);
        assert_eq!(resting.energy, 1.0);
    }

    #[test]
    fn exhaustion_forces_micro_rest_so_energy_never_pins_at_zero() {
        let mut state = LifeState::default();
        for _ in 0..3_600 {
            state.tick(1.0, false, false);
        }
        assert!(state.energy > 0.10);
        assert!(state.energy < 0.30);
    }

    #[test]
    fn a_single_time_slice_simulates_at_most_one_second() {
        let mut long_slice = LifeState::default();
        let mut capped = LifeState::default();
        long_slice.tick(3600.0, true, false);
        capped.tick(1.0, true, false);
        assert_eq!(long_slice.values(), capped.values());
    }

    #[test]
    fn tick_moves_boredom_and_social_need_with_user_activity() {
        let mut engaged = LifeState::default();
        let mut ignored = LifeState::default();
        for _ in 0..60 {
            engaged.tick(1.0, true, false);
            ignored.tick(1.0, false, false);
        }
        assert!(engaged.boredom < ignored.boredom);
        assert!(engaged.social_need < ignored.social_need);
        assert!(engaged.curiosity > ignored.curiosity);
    }

    #[test]
    fn tick_eases_mood_toward_its_homeostatic_target() {
        let mut stressed = LifeState {
            stress: 1.0,
            mood: 0.9,
            ..LifeState::default()
        };
        let before = stressed.mood;
        for _ in 0..30 {
            stressed.tick(1.0, false, false);
        }
        assert!(stressed.mood < before);
    }

    #[test]
    fn tick_survives_hostile_time_slices_and_stays_bounded() {
        let mut state = LifeState::default();
        for (index, dt) in [f32::NAN, f32::INFINITY, -5.0, 3600.0, 0.1]
            .into_iter()
            .cycle()
            .take(10_000)
            .enumerate()
        {
            state.tick(dt, index % 2 == 0, index % 3 == 0);
        }
        assert!(state
            .values()
            .into_iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(&value)));
    }

    #[test]
    fn clean_passes_habituate_but_recoveries_celebrate_at_full_strength() {
        let mut organism = NativeOrganism::default();
        let first = organism.command_finished("cargo test", Some(0), None);
        assert_eq!(first.speech, Some("过了。"));
        assert_eq!(first.tone, Tone::Success);

        organism.restore_repo_context(0, false, 4, 0);
        let fifth = organism.command_finished("cargo test", Some(0), None);
        assert_eq!(fifth.speech, None);
        assert_eq!(fifth.tone, Tone::Success);

        organism.restore_repo_context(0, false, 9, 0);
        let tenth = organism.command_finished("cargo test", Some(0), None);
        assert_eq!(tenth.behavior, Behavior::Celebrate);
        assert_eq!(tenth.tone, Tone::Quiet);
        assert_eq!(tenth.speech, None);
        assert!(tenth.description.contains("pass 10 today"));

        // A recovery after real failures is never dampened by today's count.
        organism.restore_repo_context(3, false, 9, 3);
        let recovery = organism.command_finished("cargo test", Some(0), None);
        assert_eq!(recovery.behavior, Behavior::CelebrateBig);
        assert_eq!(recovery.speech, Some("终于。"));
    }

    #[test]
    fn state_increments_shrink_as_the_day_of_passes_grows() {
        let mut fresh = NativeOrganism::default();
        let fresh_before = fresh.state().mood;
        fresh.command_finished("cargo test", Some(0), None);
        let fresh_delta = fresh.state().mood - fresh_before;

        let mut jaded = NativeOrganism::default();
        jaded.restore_repo_context(0, false, 8, 0);
        let jaded_before = jaded.state().mood;
        jaded.command_finished("cargo test", Some(0), None);
        let jaded_delta = jaded.state().mood - jaded_before;
        assert!(jaded_delta < fresh_delta);
    }

    #[test]
    fn first_failure_after_a_clean_run_is_sensitized() {
        let mut organism = NativeOrganism::default();
        organism.restore_repo_context(0, false, 5, 0);
        let stress_before = organism.state().stress;
        let reaction = organism.command_finished("cargo test", Some(101), None);
        assert!(reaction
            .description
            .contains("first crack after 5 clean run(s)"));
        assert_eq!(reaction.speech, Some("这里。"));
        assert!(organism.state().stress - stress_before > 0.15);

        let ordinary = organism.command_finished("cargo test", Some(101), None);
        assert!(!ordinary.description.contains("first crack"));
    }

    #[test]
    fn any_command_streak_wearies_and_any_success_clears_it() {
        let mut organism = NativeOrganism::default();
        let first = organism.command_finished("ssh remote true", Some(255), None);
        assert_eq!(first.behavior, Behavior::InspectError);
        assert_eq!(first.speech, Some("这里。"));
        let second = organism.command_finished("ssh remote true", Some(255), None);
        assert_eq!(second.behavior, Behavior::InspectError);
        assert_eq!(second.speech, None);
        let third = organism.command_finished("ssh remote true", Some(255), None);
        assert_eq!(third.behavior, Behavior::SitNearError);
        assert!(third.description.contains("3 rough commands in a row"));
        assert_eq!(third.speech, None);

        organism.command_finished("printf ok", Some(0), None);
        let after_reset = organism.command_finished("ssh remote true", Some(255), None);
        assert_eq!(after_reset.behavior, Behavior::InspectError);
        assert_eq!(after_reset.speech, Some("这里。"));
    }

    #[test]
    fn unknown_exit_neither_extends_nor_clears_the_rough_streak() {
        let mut organism = NativeOrganism::default();
        organism.command_finished("ssh remote true", Some(255), None);
        organism.command_finished("ssh remote true", Some(255), None);
        organism.command_finished("mystery", None, None);
        let third = organism.command_finished("ssh remote true", Some(255), None);
        assert_eq!(third.behavior, Behavior::SitNearError);
        assert!(third.description.contains("3 rough commands in a row"));
    }

    #[test]
    fn context_reset_and_day_rollover_restart_todays_rhythm() {
        let mut organism = NativeOrganism::default();
        organism.restore_repo_context(0, false, 9, 2);
        organism.restore_repo_context(0, false, 0, 0);
        let pass = organism.command_finished("cargo test", Some(0), None);
        assert_eq!(pass.speech, Some("过了。"));

        let mut overnight = NativeOrganism::default();
        overnight.restore_repo_context(0, false, 9, 2);
        overnight.roll_over_day();
        let fresh = overnight.command_finished("cargo test", Some(0), None);
        assert_eq!(fresh.speech, Some("过了。"));
        assert_eq!(fresh.tone, Tone::Success);
    }

    #[test]
    fn repo_arrival_shapes_the_next_command_start_only() {
        assert_eq!(RepoArrival::from_familiarity(0), RepoArrival::Unfamiliar);
        assert_eq!(RepoArrival::from_familiarity(3), RepoArrival::Known);
        assert_eq!(RepoArrival::from_familiarity(7), RepoArrival::Home);

        let mut organism = NativeOrganism::default();
        organism.note_repo_arrival(RepoArrival::Unfamiliar);
        let confidence_before = organism.state().confidence;
        let shy = organism.command_started("cargo build");
        assert_eq!(shy.tone, Tone::Quiet);
        assert!(shy.description.contains("first day in this repo"));
        assert_eq!(shy.speech, None);
        assert!(organism.state().confidence < confidence_before);

        let plain = organism.command_started("cargo build");
        assert_eq!(plain.tone, Tone::Active);
        assert!(!plain.description.contains("first day"));

        organism.note_repo_arrival(RepoArrival::Home);
        let home = organism.command_started("cargo build");
        assert_eq!(home.speech, Some("回来了。"));
    }

    #[test]
    fn accepted_correction_gets_a_small_nod_on_the_next_success_only() {
        let mut organism = NativeOrganism::default();
        organism.note_assisted_command();
        let success = organism.command_finished("git status", Some(0), None);
        assert_eq!(success.behavior, Behavior::Celebrate);
        assert_eq!(success.tone, Tone::Success);
        assert!(success.description.contains("corrected command worked"));
        assert_eq!(success.speech, None);

        let ordinary = organism.command_finished("git status", Some(0), None);
        assert_eq!(ordinary.behavior, Behavior::Idle);

        // A failed assisted command earns no celebration, and the assist does
        // not carry over to the next command.
        organism.note_assisted_command();
        let failed = organism.command_finished("git statsu", Some(1), None);
        assert_eq!(failed.behavior, Behavior::InspectError);
        let following = organism.command_finished("git status", Some(0), None);
        assert_eq!(following.behavior, Behavior::Idle);
    }

    #[test]
    fn sticky_glyphs_stay_five_ascii_characters_and_animate_slowly() {
        for behavior in [
            Behavior::Idle,
            Behavior::WatchCommand,
            Behavior::InspectError,
            Behavior::SitNearError,
            Behavior::Celebrate,
            Behavior::CelebrateBig,
            Behavior::RestAfterPush,
            Behavior::UnknownOutcome,
        ] {
            for frame in [0, 4, 5, 54, 55, 59, u64::MAX] {
                let glyph = sticky_glyph(behavior, frame);
                assert_eq!(glyph.chars().count(), 5);
                assert!(glyph.is_ascii());
            }
        }
        // Watching alternates every five frames; idle almost never moves.
        assert_ne!(
            sticky_glyph(Behavior::WatchCommand, 0),
            sticky_glyph(Behavior::WatchCommand, 5)
        );
        assert_eq!(
            sticky_glyph(Behavior::Idle, 0),
            sticky_glyph(Behavior::Idle, 5)
        );
        assert_ne!(
            sticky_glyph(Behavior::Idle, 55),
            sticky_glyph(Behavior::Idle, 0)
        );
    }

    #[test]
    fn correction_pulses_stay_bounded_and_scale_with_dismissal_streaks() {
        let mut state = LifeState::default();
        for _ in 0..1_000 {
            state = correction_dismissed(state, 4);
        }
        assert!(state
            .values()
            .into_iter()
            .all(|value| (0.0..=1.0).contains(&value)));
        assert_eq!(state.boredom, 1.0);

        let single = correction_dismissed(LifeState::default(), 1);
        let heavy = correction_dismissed(LifeState::default(), 4);
        assert!(heavy.boredom > single.boredom);

        let accepted = correction_accepted(LifeState::default());
        assert!(accepted.confidence > LifeState::default().confidence);
    }

    #[test]
    fn persisted_state_and_repo_failure_streak_resume_safely() {
        let mut organism = NativeOrganism::from_persisted_state(LifeState {
            energy: f32::NAN,
            mood: 2.0,
            ..LifeState::default()
        });
        assert_eq!(organism.state().energy, 0.5);
        assert_eq!(organism.state().mood, 1.0);

        organism.restore_build_failures(3);
        organism.command_started("cargo test");
        let reaction = organism.command_finished("cargo test", Some(0), Some(100));
        assert_eq!(reaction.behavior, Behavior::CelebrateBig);
        assert_eq!(reaction.speech, Some("终于。"));
    }
}
