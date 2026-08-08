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

#[derive(Debug, Default)]
pub(crate) struct NativeOrganism {
    state: LifeState,
    build_failures: u32,
    active_kind: Option<CommandKind>,
    recovered_build: bool,
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

    pub(crate) fn restore_repo_context(&mut self, failures: u32, recovered_build: bool) {
        self.build_failures = failures;
        self.recovered_build = recovered_build;
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
        self.state.clamp();
        Reaction {
            behavior: Behavior::WatchCommand,
            tone: Tone::Active,
            description: format!("watching real {} event", kind.label()),
            speech: None,
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
            self.state.mood -= 0.08;
            self.state.stress += 0.12;
            self.state.confidence -= 0.04;
            self.state.curiosity += 0.05;
            self.state.clamp();

            let failures = if kind == CommandKind::BuildOrTest {
                self.build_failures = self.build_failures.saturating_add(1);
                self.recovered_build = false;
                self.build_failures
            } else {
                0
            };
            return Reaction {
                behavior: if failures >= 2 {
                    Behavior::SitNearError
                } else {
                    Behavior::InspectError
                },
                tone: Tone::Error,
                description: if failures == 0 {
                    format!("exit {exit_code}{duration} · inspecting the finished Block")
                } else {
                    format!("exit {exit_code}{duration} · build failure {failures}")
                },
                speech: (failures <= 1).then_some("这里。"),
            };
        }

        match kind {
            CommandKind::BuildOrTest => {
                let failures = std::mem::take(&mut self.build_failures);
                self.recovered_build = failures > 0;
                self.state.mood += 0.10 + (failures.min(5) as f32 * 0.025);
                self.state.stress -= 0.12;
                self.state.confidence += 0.08;
                self.state.attachment += 0.025;
                self.state.clamp();
                let (behavior, speech) = match failures {
                    0 => (Behavior::Celebrate, Some("过了。")),
                    1..=2 => (Behavior::Celebrate, Some("好了。")),
                    _ => (Behavior::CelebrateBig, Some("终于。")),
                };
                Reaction {
                    behavior,
                    tone: Tone::Success,
                    description: format!("build/test passed after {failures} failure(s){duration}"),
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
