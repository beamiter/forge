use crate::memory::Memory;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LifeState {
    pub energy: f32,
    pub mood: f32,
    pub curiosity: f32,
    pub boredom: f32,
    pub stress: f32,
    pub social_need: f32,
    pub attachment: f32,
    pub confidence: f32,
}

impl Default for LifeState {
    fn default() -> Self {
        Self {
            energy: 0.68,
            mood: 0.58,
            curiosity: 0.72,
            boredom: 0.28,
            stress: 0.12,
            social_need: 0.24,
            attachment: 0.42,
            confidence: 0.61,
        }
    }
}

impl LifeState {
    pub fn tick(&mut self, dt: f32, user_active: bool, sleeping: bool) {
        let dt = if dt.is_finite() {
            dt.clamp(0.0, 1.0)
        } else {
            0.0
        };

        if sleeping {
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

    pub fn clamp(&mut self) {
        for value in [
            &mut self.energy,
            &mut self.mood,
            &mut self.curiosity,
            &mut self.boredom,
            &mut self.stress,
            &mut self.social_need,
            &mut self.attachment,
            &mut self.confidence,
        ] {
            *value = if value.is_finite() {
                value.clamp(0.0, 1.0)
            } else {
                0.5
            };
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifeEvent {
    LongIdle,
    UserTyping,
    BuildStarted,
    CommandFailed,
    CommandSucceeded,
    GitPush,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behavior {
    Idle,
    Sleep,
    Wake,
    WatchTyping,
    WatchBuild,
    InspectError,
    SitNearError,
    WearyError,
    Explore,
    Approach,
    CelebrateSmall,
    CelebrateBig,
    RestAfterPush,
}

impl Behavior {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Sleep => "sleep",
            Self::Wake => "wake",
            Self::WatchTyping => "watch typing",
            Self::WatchBuild => "watch build",
            Self::InspectError => "inspect error",
            Self::SitNearError => "sit near error",
            Self::WearyError => "weary of errors",
            Self::Explore => "explore",
            Self::Approach => "approach",
            Self::CelebrateSmall => "small celebration",
            Self::CelebrateBig => "big celebration",
            Self::RestAfterPush => "rest after push",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    pub behavior: Behavior,
    pub speech: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Organism {
    pub state: LifeState,
    pub behavior: Behavior,
    pub consecutive_failures: u32,
    pub last_event: &'static str,
    pub speech: Option<String>,
    user_active_for: f32,
    behavior_hold_for: f32,
    speech_hold_for: f32,
    build_running: bool,
    idle_for: f32,
    pending_faster_memory: bool,
    rng: u64,
}

impl Default for Organism {
    fn default() -> Self {
        Self::new(0x4f52_4741_4e49_534d)
    }
}

impl Organism {
    pub fn new(seed: u64) -> Self {
        Self {
            state: LifeState::default(),
            behavior: Behavior::Idle,
            consecutive_failures: 0,
            last_event: "born",
            speech: None,
            user_active_for: 0.0,
            behavior_hold_for: 0.0,
            speech_hold_for: 0.0,
            build_running: false,
            idle_for: 0.0,
            pending_faster_memory: false,
            rng: seed.max(1),
        }
    }

    pub fn demo(seed: u64) -> Self {
        let mut organism = Self::new(seed);
        organism.state.energy = 0.12;
        organism.state.boredom = 0.18;
        organism.behavior = Behavior::Sleep;
        organism.behavior_hold_for = 30.0;
        organism.last_event = "long idle";
        organism
    }

    pub fn tick(&mut self, dt: f32) {
        let dt = if dt.is_finite() {
            dt.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.user_active_for = (self.user_active_for - dt).max(0.0);
        self.behavior_hold_for = (self.behavior_hold_for - dt).max(0.0);
        self.speech_hold_for = (self.speech_hold_for - dt).max(0.0);
        if self.speech_hold_for == 0.0 {
            self.speech = None;
        }

        if self.user_active_for > 0.0 || self.build_running {
            self.idle_for = 0.0;
        } else {
            self.idle_for += dt;
        }
        let sleeping = self.behavior == Behavior::Sleep;
        self.state.tick(dt, self.user_active_for > 0.0, sleeping);

        if self.behavior_hold_for == 0.0 {
            self.behavior = self.choose_utility_behavior();
            self.behavior_hold_for = match self.behavior {
                Behavior::Sleep => 2.5,
                Behavior::Explore => 1.4,
                Behavior::Approach => 1.8,
                _ => 1.0,
            };
        }
    }

    pub fn react(
        &mut self,
        event: LifeEvent,
        memory: &mut Memory,
        day: u64,
        repo: &str,
        at_ms: u64,
    ) -> Reaction {
        let (behavior, hold_for, speech, speech_for) = match event {
            LifeEvent::LongIdle => {
                self.last_event = "long idle";
                self.user_active_for = 0.0;
                self.build_running = false;
                (Behavior::Sleep, 8.0, None, 0.0)
            }
            LifeEvent::UserTyping => {
                self.last_event = "typing";
                self.user_active_for = 2.5;
                self.state.energy += 0.08;
                self.state.curiosity += 0.04;
                self.state.boredom -= 0.10;
                (Behavior::Wake, 0.7, None, 0.0)
            }
            LifeEvent::BuildStarted => {
                self.last_event = "cargo build started";
                self.user_active_for = 1.0;
                self.build_running = true;
                self.state.curiosity += 0.10;
                (Behavior::WatchBuild, 5.0, None, 0.0)
            }
            LifeEvent::CommandFailed => {
                self.last_event = "command failed";
                self.build_running = false;
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                memory.record_failure(day, repo, at_ms);
                self.state.stress += 0.05;
                self.state.confidence -= 0.025;
                let behavior = match self.consecutive_failures {
                    0 | 1 => Behavior::InspectError,
                    2..=3 => Behavior::SitNearError,
                    _ => Behavior::WearyError,
                };
                let speech = (self.consecutive_failures == 1).then(|| "这里。".to_owned());
                (behavior, 4.0, speech, 2.0)
            }
            LifeEvent::CommandSucceeded => {
                self.last_event = "command succeeded";
                self.build_running = false;
                let failures = self.consecutive_failures;
                memory.record_success(day, repo, at_ms);
                self.pending_faster_memory = memory.faster_than_previous(day, repo);
                self.state.stress -= 0.14;
                self.state.confidence += 0.12;
                self.state.mood += 0.14;
                self.consecutive_failures = 0;
                if failures >= 3 {
                    (Behavior::CelebrateBig, 3.0, Some("终于。".to_owned()), 2.4)
                } else if failures > 0 {
                    (
                        Behavior::CelebrateSmall,
                        2.0,
                        Some("好了。".to_owned()),
                        2.0,
                    )
                } else {
                    (
                        Behavior::CelebrateSmall,
                        1.6,
                        Some("过了。".to_owned()),
                        1.8,
                    )
                }
            }
            LifeEvent::GitPush => {
                self.last_event = "git push";
                memory.record_push(day, repo);
                self.state.attachment += 0.025;
                let speech = if self.pending_faster_memory {
                    self.pending_faster_memory = false;
                    "这次比昨天快。"
                } else {
                    "咖啡时间？"
                };
                (Behavior::RestAfterPush, 5.0, Some(speech.to_owned()), 4.0)
            }
        };

        self.state.clamp();
        self.behavior = behavior;
        self.behavior_hold_for = hold_for;
        self.speech = speech.clone();
        self.speech_hold_for = speech_for;
        Reaction { behavior, speech }
    }

    fn choose_utility_behavior(&mut self) -> Behavior {
        if self.state.energy < 0.15 {
            return Behavior::Sleep;
        }
        if self.build_running {
            return Behavior::WatchBuild;
        }
        if self.user_active_for > 0.0 {
            return Behavior::WatchTyping;
        }

        let candidates = [
            (
                Behavior::Sleep,
                (1.0 - self.state.energy) * 1.15 + self.idle_for.min(60.0) / 180.0,
            ),
            (
                Behavior::Explore,
                self.state.boredom * 0.72 + self.state.curiosity * 0.30,
            ),
            (
                Behavior::Approach,
                self.state.social_need * 0.72 + self.state.attachment * 0.12,
            ),
            (Behavior::Idle, 0.30 + self.state.mood * 0.10),
        ];

        let mut best = Behavior::Idle;
        let mut best_score = f32::NEG_INFINITY;
        for (behavior, score) in candidates {
            let inertia = if behavior == self.behavior { 0.08 } else { 0.0 };
            let score = score + inertia + self.next_unit() * 0.08;
            if score > best_score {
                best = behavior;
                best_score = score;
            }
        }
        best
    }

    fn next_unit(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng as u32) as f32 / u32::MAX as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_tick_matches_the_conversation_equations() {
        let mut state = LifeState {
            energy: 0.5,
            mood: 0.5,
            curiosity: 0.5,
            boredom: 0.5,
            stress: 0.5,
            social_need: 0.5,
            attachment: 0.5,
            confidence: 0.5,
        };
        state.tick(1.0, true, false);

        assert!((state.energy - 0.498).abs() < 1e-6);
        assert!((state.boredom - 0.490).abs() < 1e-6);
        assert!((state.curiosity - 0.503).abs() < 1e-6);
    }

    #[test]
    fn all_state_values_are_bounded_and_finite() {
        let mut state = LifeState {
            energy: -4.0,
            mood: 3.0,
            curiosity: f32::NAN,
            boredom: f32::INFINITY,
            stress: -1.0,
            social_need: 2.0,
            attachment: 3.0,
            confidence: -2.0,
        };
        state.clamp();

        for value in [
            state.energy,
            state.mood,
            state.curiosity,
            state.boredom,
            state.stress,
            state.social_need,
            state.attachment,
            state.confidence,
        ] {
            assert!(value.is_finite());
            assert!((0.0..=1.0).contains(&value));
        }
    }

    #[test]
    fn repeated_failures_escalate_and_success_celebrates() {
        let day = 20;
        let mut memory = Memory::demo(day, "forge");
        let mut organism = Organism::new(1);
        for expected in [
            Behavior::InspectError,
            Behavior::SitNearError,
            Behavior::SitNearError,
        ] {
            let reaction =
                organism.react(LifeEvent::CommandFailed, &mut memory, day, "forge", 1_000);
            assert_eq!(reaction.behavior, expected);
        }

        let reaction = organism.react(
            LifeEvent::CommandSucceeded,
            &mut memory,
            day,
            "forge",
            20_000,
        );
        assert_eq!(reaction.behavior, Behavior::CelebrateBig);
        assert_eq!(reaction.speech.as_deref(), Some("终于。"));

        let reaction = organism.react(LifeEvent::GitPush, &mut memory, day, "forge", 21_000);
        assert_eq!(reaction.speech.as_deref(), Some("这次比昨天快。"));
    }

    #[test]
    fn low_energy_has_priority_when_no_event_is_held() {
        let mut organism = Organism::new(2);
        organism.state.energy = 0.10;
        organism.tick(0.1);
        assert_eq!(organism.behavior, Behavior::Sleep);
    }

    #[test]
    fn utility_behavior_is_held_instead_of_rerolled_every_frame() {
        let mut organism = Organism::new(3);
        organism.behavior_hold_for = 0.0;
        organism.tick(0.05);
        let chosen = organism.behavior;

        for _ in 0..10 {
            organism.tick(0.05);
            assert_eq!(organism.behavior, chosen);
        }
        assert!(organism.behavior_hold_for > 0.0);
    }
}
