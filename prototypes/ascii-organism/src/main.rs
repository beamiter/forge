use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{
    self, disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{execute, queue};
use forge_ascii_organism::{Behavior, LifeEvent, Memory, Organism};
use serde_json::json;
use std::env;
use std::error::Error;
use std::io::{self, stdout, IsTerminal, Stdout, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const FRAME_TIME: Duration = Duration::from_millis(50);
const DEMO_STEPS: &[(f32, LifeEvent)] = &[
    (0.0, LifeEvent::LongIdle),
    (3.0, LifeEvent::UserTyping),
    (6.0, LifeEvent::BuildStarted),
    (9.0, LifeEvent::CommandFailed),
    (11.0, LifeEvent::BuildStarted),
    (13.0, LifeEvent::CommandFailed),
    (15.0, LifeEvent::BuildStarted),
    (17.0, LifeEvent::CommandFailed),
    (19.0, LifeEvent::BuildStarted),
    (22.0, LifeEvent::CommandSucceeded),
    (26.0, LifeEvent::GitPush),
];

#[derive(Debug)]
struct Args {
    demo: bool,
    headless_demo: bool,
    speed: f32,
    memory_path: Option<PathBuf>,
    no_memory: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = env::args().skip(1);
        let mut parsed = Self {
            demo: false,
            headless_demo: false,
            speed: 1.0,
            memory_path: None,
            no_memory: false,
        };
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--demo" => parsed.demo = true,
                "--headless-demo" => parsed.headless_demo = true,
                "--no-memory" => parsed.no_memory = true,
                "--speed" => {
                    let value = args.next().ok_or("--speed requires a number")?;
                    parsed.speed = value
                        .parse::<f32>()
                        .map_err(|_| format!("invalid --speed value: {value}"))?;
                    if !parsed.speed.is_finite() || !(0.1..=20.0).contains(&parsed.speed) {
                        return Err("--speed must be between 0.1 and 20".to_owned());
                    }
                }
                "--memory" => {
                    let value = args.next().ok_or("--memory requires a path")?;
                    parsed.memory_path = Some(PathBuf::from(value));
                }
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                unknown => return Err(format!("unknown option: {unknown}")),
            }
        }
        if parsed.no_memory && parsed.memory_path.is_some() {
            return Err("--no-memory cannot be combined with --memory".to_owned());
        }
        Ok(parsed)
    }
}

fn print_help() {
    println!(
        "forge-ascii-organism 0.1.0\n\
         \n\
         Usage: forge-ascii-organism [OPTIONS]\n\
         \n\
           --demo            Run the scripted Forge demo, then stay interactive\n\
           --headless-demo   Replay the same semantic events and print JSON\n\
           --speed N         Demo playback multiplier (0.1..20, default 1)\n\
           --memory PATH     Override the persistent memory file\n\
           --no-memory       Disable disk persistence\n\
           -h, --help        Show this help\n\
         \n\
         Keys: t typing, b build, f fail, s success, p push, i idle, r replay, q quit"
    );
}

#[derive(Debug, Clone)]
struct WorldLine {
    text: String,
    color: Color,
}

struct DemoClock {
    elapsed: f32,
    next_step: usize,
    speed: f32,
    complete: bool,
}

impl DemoClock {
    fn new(speed: f32) -> Self {
        Self {
            elapsed: 0.0,
            next_step: 0,
            speed,
            complete: false,
        }
    }

    fn advance(&mut self, dt: f32) -> Vec<(f32, LifeEvent)> {
        self.elapsed += dt * self.speed;
        let mut events = Vec::new();
        while let Some((at, event)) = DEMO_STEPS.get(self.next_step) {
            if *at > self.elapsed {
                break;
            }
            events.push((*at, *event));
            self.next_step += 1;
        }
        if self.next_step == DEMO_STEPS.len() && self.elapsed >= 31.0 {
            self.complete = true;
        }
        events
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter(output: &mut Stdout) -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(
            output,
            EnterAlternateScreen,
            EnableBracketedPaste,
            Hide,
            Clear(ClearType::All)
        ) {
            let _ = disable_raw_mode();
            let _ = execute!(
                output,
                DisableBracketedPaste,
                ResetColor,
                Show,
                LeaveAlternateScreen
            );
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            stdout(),
            DisableBracketedPaste,
            ResetColor,
            Show,
            LeaveAlternateScreen
        );
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse().map_err(|error| {
        eprintln!("error: {error}\n");
        print_help();
        io::Error::new(io::ErrorKind::InvalidInput, error)
    })?;

    if args.headless_demo {
        return run_headless_demo().map_err(Into::into);
    }
    run_tui(args)
}

fn run_headless_demo() -> io::Result<()> {
    let day = Memory::local_day();
    let repo = "forge";
    let mut memory = Memory::demo(day, repo);
    let mut organism = Organism::demo(7);
    let mut transitions = Vec::new();
    let mut observed_behaviors = Vec::new();
    let base = Memory::unix_ms();
    let mut previous_at = 0.0;

    for (at, event) in DEMO_STEPS {
        advance_life(&mut organism, *at - previous_at);
        previous_at = *at;
        let reaction = organism.react(
            *event,
            &mut memory,
            day,
            repo,
            base.saturating_add((*at * 1_000.0) as u64),
        );
        observed_behaviors.push(reaction.behavior.label());
        transitions.push(json!({
            "at_seconds": at,
            "event": format!("{event:?}"),
            "behavior": reaction.behavior.label(),
            "speech": reaction.speech,
        }));
    }

    let values = [
        organism.state.energy,
        organism.state.mood,
        organism.state.curiosity,
        organism.state.boredom,
        organism.state.stress,
        organism.state.social_need,
        organism.state.attachment,
        organism.state.confidence,
    ];
    let bounded = values
        .iter()
        .all(|value| value.is_finite() && (0.0..=1.0).contains(value));
    let expected_behaviors = [
        "sleep",
        "wake",
        "watch build",
        "inspect error",
        "watch build",
        "sit near error",
        "watch build",
        "sit near error",
        "watch build",
        "big celebration",
        "rest after push",
    ];
    let transitions_ok = observed_behaviors == expected_behaviors
        && organism.speech.as_deref() == Some("这次比昨天快。");
    let report = json!({
        "ok": bounded && transitions_ok,
        "mode": "no-llm",
        "repo": repo,
        "final_behavior": organism.behavior.label(),
        "memory_phrase": organism.speech,
        "state_bounded": bounded,
        "transitions_ok": transitions_ok,
        "transitions": transitions,
    });
    serde_json::to_writer_pretty(io::stdout().lock(), &report)?;
    println!();
    Ok(())
}

fn run_tui(args: Args) -> Result<(), Box<dyn Error>> {
    if !terminal::is_raw_mode_enabled().unwrap_or(false)
        && (!io::stdin().is_terminal() || !io::stdout().is_terminal())
    {
        return Err("interactive mode requires a terminal; use --headless-demo".into());
    }

    let mut day = Memory::local_day();
    let (repo_key, repo_label) = current_repo_identity();
    let memory_path = if args.no_memory {
        None
    } else {
        args.memory_path.clone().or_else(Memory::default_path)
    };
    let demo_memory = args.demo;
    let persistent_path = if demo_memory {
        None
    } else {
        memory_path.as_deref()
    };
    let _memory_lock = persistent_path.map(Memory::acquire_lock).transpose()?;
    let mut memory = if demo_memory {
        Memory::demo(day, &repo_key)
    } else if let Some(path) = persistent_path {
        // Refuse to replace an unreadable store with fresh defaults. A corrupt
        // or newer-version file must remain intact for explicit recovery.
        Memory::load(path)?
    } else {
        Memory::default()
    };
    memory.begin_session();
    if let Some(path) = persistent_path {
        memory.save_atomic(path)?;
    }

    let mut organism = if args.demo {
        Organism::demo(7)
    } else {
        Organism::default()
    };
    let mut demo = args.demo.then(|| DemoClock::new(args.speed));
    let mut demo_base_ms = Memory::unix_ms();
    let mut world = initial_world();
    let mut output = stdout();
    let _guard = TerminalGuard::enter(&mut output)?;
    let mut last_tick = Instant::now();
    let started = last_tick;
    let mut quit = false;

    while !quit {
        let now = Instant::now();
        let dt = now.duration_since(last_tick).as_secs_f32().min(0.25);
        last_tick = now;
        if !demo_memory {
            day = Memory::local_day();
        }

        let mut simulation_dt = dt;
        if let Some(clock) = demo.as_mut() {
            simulation_dt *= clock.speed;
            for (at, life_event) in clock.advance(dt) {
                apply_event(
                    life_event,
                    &mut organism,
                    &mut memory,
                    day,
                    &repo_key,
                    demo_base_ms.saturating_add((at * 1_000.0) as u64),
                    &mut world,
                );
            }
        }
        organism.tick(simulation_dt);
        let visual_elapsed = demo
            .as_ref()
            .map(|clock| clock.elapsed)
            .unwrap_or_else(|| started.elapsed().as_secs_f32());
        let memory_mode = if demo_memory {
            "SYNTHETIC MEMORY"
        } else if persistent_path.is_some() {
            "PERSISTENT"
        } else {
            "VOLATILE"
        };
        draw(
            &mut output,
            &organism,
            &memory,
            day,
            &repo_key,
            &repo_label,
            memory_mode,
            &world,
            demo.as_ref(),
            visual_elapsed,
        )?;

        if event::poll(FRAME_TIME)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if key_requests_quit(&key) {
                        quit = true;
                    } else if key.code == KeyCode::Char('r')
                        && key.modifiers == KeyModifiers::NONE
                        && demo.is_some()
                    {
                        organism = Organism::demo(7);
                        memory = Memory::demo(day, &repo_key);
                        memory.begin_session();
                        world = initial_world();
                        demo = Some(DemoClock::new(args.speed));
                        demo_base_ms = Memory::unix_ms();
                    } else if let Some(life_event) = key_life_event(&key) {
                        apply_event(
                            life_event,
                            &mut organism,
                            &mut memory,
                            day,
                            &repo_key,
                            Memory::unix_ms(),
                            &mut world,
                        );
                        if event_changes_memory(life_event) {
                            if let Some(path) = persistent_path {
                                memory.save_atomic(path)?;
                            }
                        }
                    }
                }
                Event::Paste(_) => {}
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }

    if let Some(path) = persistent_path {
        memory.save_atomic(path)?;
    }
    Ok(())
}

fn advance_life(organism: &mut Organism, seconds: f32) {
    let mut remaining = seconds.max(0.0);
    while remaining > 0.0 {
        let step = remaining.min(0.05);
        organism.tick(step);
        remaining -= step;
    }
}

fn key_requests_quit(key: &KeyEvent) -> bool {
    (matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) && key.modifiers == KeyModifiers::NONE)
        || (key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL)
}

fn key_life_event(key: &KeyEvent) -> Option<LifeEvent> {
    if key.modifiers != KeyModifiers::NONE {
        return None;
    }
    match key.code {
        KeyCode::Char('t') => Some(LifeEvent::UserTyping),
        KeyCode::Char('b') => Some(LifeEvent::BuildStarted),
        KeyCode::Char('f') => Some(LifeEvent::CommandFailed),
        KeyCode::Char('s') => Some(LifeEvent::CommandSucceeded),
        KeyCode::Char('p') => Some(LifeEvent::GitPush),
        KeyCode::Char('i') => Some(LifeEvent::LongIdle),
        _ => None,
    }
}

fn event_changes_memory(event: LifeEvent) -> bool {
    matches!(
        event,
        LifeEvent::CommandFailed | LifeEvent::CommandSucceeded | LifeEvent::GitPush
    )
}

fn apply_event(
    event: LifeEvent,
    organism: &mut Organism,
    memory: &mut Memory,
    day: u64,
    repo: &str,
    at_ms: u64,
    world: &mut Vec<WorldLine>,
) {
    organism.react(event, memory, day, repo, at_ms);
    update_world(world, event);
}

fn update_world(world: &mut Vec<WorldLine>, event: LifeEvent) {
    let additions: &[(&str, Color)] = match event {
        LifeEvent::LongIdle => &[("$", Color::DarkGrey)],
        LifeEvent::UserTyping => &[("$ cargo build", Color::White)],
        LifeEvent::BuildStarted => &[("   Compiling forge v0.2.0", Color::Cyan)],
        LifeEvent::CommandFailed => &[
            ("error[E0382]: borrow of moved value: `state`", Color::Red),
            ("  --> src/life.rs:42:17", Color::DarkRed),
        ],
        LifeEvent::CommandSucceeded => &[
            ("   Finished dev profile", Color::Green),
            ("   test result: ok", Color::Green),
        ],
        LifeEvent::GitPush => &[
            ("$ git push", Color::White),
            ("   main -> main", Color::Green),
        ],
    };
    world.extend(additions.iter().map(|(text, color)| WorldLine {
        text: (*text).to_owned(),
        color: *color,
    }));
    if world.len() > 9 {
        world.drain(..world.len() - 9);
    }
}

fn initial_world() -> Vec<WorldLine> {
    vec![
        WorldLine {
            text: "Synthetic semantic events // not connected to Forge command hooks".to_owned(),
            color: Color::DarkGrey,
        },
        WorldLine {
            text: "$".to_owned(),
            color: Color::White,
        },
    ]
}

#[allow(clippy::too_many_arguments)]
fn draw(
    output: &mut Stdout,
    organism: &Organism,
    memory: &Memory,
    day: u64,
    repo_key: &str,
    repo_label: &str,
    memory_mode: &str,
    world: &[WorldLine],
    demo: Option<&DemoClock>,
    elapsed: f32,
) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    queue!(output, MoveTo(0, 0), Clear(ClearType::All))?;
    if width < 80 || height < 20 {
        draw_text(
            output,
            1,
            1,
            "ASCII organism needs at least 80x20 cells. Resize Forge.",
            width.saturating_sub(2),
            Color::Yellow,
        )?;
        output.flush()?;
        return Ok(());
    }

    draw_text(
        output,
        1,
        0,
        &format!("ASCII ORGANISM v0.1 // SYNTHETIC VTE HARNESS // NO LLM // {memory_mode}"),
        width - 2,
        Color::Cyan,
    )?;
    draw_text(
        output,
        1,
        1,
        &"─".repeat(width.saturating_sub(2) as usize),
        width - 2,
        Color::DarkGrey,
    )?;

    let stats = [
        ("energy", organism.state.energy),
        ("mood", organism.state.mood),
        ("curiosity", organism.state.curiosity),
        ("boredom", organism.state.boredom),
        ("stress", organism.state.stress),
        ("social", organism.state.social_need),
        ("attachment", organism.state.attachment),
        ("confidence", organism.state.confidence),
    ];
    let half = width / 2;
    for (index, (name, value)) in stats.iter().enumerate() {
        let column = if index < 4 { 1 } else { half };
        let row = 2 + (index % 4) as u16;
        let bar = state_bar(*value, 12);
        draw_text(
            output,
            column,
            row,
            &format!("{name:>10} {bar} {:>3}%", (*value * 100.0) as u8),
            half.saturating_sub(2),
            state_color(*value),
        )?;
    }

    let today = memory.today(day, repo_key);
    let memory_line = format!(
        "repo={repo_label}  behavior={}  failures={}  sessions={}  last={}",
        organism.behavior.label(),
        today.map_or(0, |stats| stats.build_failures),
        memory.sessions,
        organism.last_event,
    );
    draw_text(output, 1, 6, &memory_line, width - 2, Color::DarkGrey)?;

    let world_top = 8u16;
    let ground = height.saturating_sub(4);
    let available_log_rows = ground.saturating_sub(world_top + 6) as usize;
    let visible = world.len().min(available_log_rows.max(2));
    for (index, line) in world[world.len().saturating_sub(visible)..]
        .iter()
        .enumerate()
    {
        draw_text(
            output,
            3,
            world_top + index as u16,
            &line.text,
            width.saturating_sub(6),
            line.color,
        )?;
    }

    let sprite = sprite(organism.behavior, elapsed);
    let sprite_width = sprite
        .iter()
        .map(|line| UnicodeWidthStr::width(*line) as u16)
        .max()
        .unwrap_or(1);
    let max_x = width.saturating_sub(sprite_width + 3).max(2);
    let x = creature_x(organism.behavior, elapsed, max_x);
    let y = ground.saturating_sub(sprite.len() as u16);
    if let Some(speech) = organism.speech.as_deref() {
        let speech_width = UnicodeWidthStr::width(speech) as u16 + 4;
        let speech_x = x.min(width.saturating_sub(speech_width + 1));
        draw_text(
            output,
            speech_x,
            y.saturating_sub(2),
            &format!("“{speech}”"),
            width.saturating_sub(speech_x + 1),
            Color::Yellow,
        )?;
    }
    for (offset, line) in sprite.iter().enumerate() {
        draw_text(
            output,
            x,
            y + offset as u16,
            line,
            width.saturating_sub(x + 1),
            creature_color(organism.behavior),
        )?;
    }
    draw_text(
        output,
        1,
        ground,
        &"─".repeat(width.saturating_sub(2) as usize),
        width - 2,
        Color::DarkGrey,
    )?;

    let progress = demo.map(|clock| {
        if clock.complete {
            "demo complete · press r to replay".to_owned()
        } else {
            format!("demo {:>4.1}/31s", clock.elapsed.min(31.0))
        }
    });
    let footer = format!(
        "[t]ype [b]uild [f]ail [s]uccess [p]ush [i]dle [r]eplay [q]uit{}",
        progress
            .as_deref()
            .map(|value| format!("   │   {value}"))
            .unwrap_or_default()
    );
    draw_text(output, 1, height - 2, &footer, width - 2, Color::DarkGrey)?;
    queue!(output, ResetColor)?;
    output.flush()
}

fn draw_text(
    output: &mut Stdout,
    x: u16,
    y: u16,
    text: &str,
    max_width: u16,
    color: Color,
) -> io::Result<()> {
    let text = truncate_cells(text, max_width as usize);
    queue!(
        output,
        MoveTo(x, y),
        SetForegroundColor(color),
        Print(text),
        ResetColor
    )
}

fn truncate_cells(text: &str, max_width: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|character| {
            let width = UnicodeWidthChar::width(*character).unwrap_or(0);
            if used + width > max_width {
                false
            } else {
                used += width;
                true
            }
        })
        .collect()
}

fn state_bar(value: f32, cells: usize) -> String {
    let filled = (value.clamp(0.0, 1.0) * cells as f32).round() as usize;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(cells - filled))
}

fn state_color(value: f32) -> Color {
    if value < 0.25 {
        Color::DarkYellow
    } else if value > 0.8 {
        Color::Green
    } else {
        Color::White
    }
}

fn creature_color(behavior: Behavior) -> Color {
    match behavior {
        Behavior::InspectError | Behavior::SitNearError | Behavior::WearyError => Color::Red,
        Behavior::CelebrateSmall | Behavior::CelebrateBig => Color::Green,
        Behavior::Sleep => Color::DarkGrey,
        Behavior::WatchBuild | Behavior::WatchTyping | Behavior::Wake => Color::Cyan,
        Behavior::RestAfterPush => Color::Yellow,
        _ => Color::White,
    }
}

fn creature_x(behavior: Behavior, elapsed: f32, max_x: u16) -> u16 {
    match behavior {
        Behavior::Sleep | Behavior::RestAfterPush => 5.min(max_x),
        Behavior::InspectError | Behavior::SitNearError | Behavior::WearyError => max_x / 2,
        Behavior::WatchBuild | Behavior::WatchTyping | Behavior::Wake => max_x.saturating_sub(4),
        Behavior::CelebrateSmall | Behavior::CelebrateBig => max_x / 2,
        Behavior::Explore => {
            let span = max_x.saturating_sub(2).max(1);
            let phase = (elapsed * 5.0) as u16 % (span * 2);
            2 + if phase <= span {
                phase
            } else {
                span * 2 - phase
            }
        }
        _ => (max_x * 3 / 4).max(2),
    }
}

fn sprite(behavior: Behavior, elapsed: f32) -> &'static [&'static str] {
    let alternate = ((elapsed * 4.0) as u64).is_multiple_of(2);
    match behavior {
        Behavior::Sleep => &[" /\\_/\\", "( -.- )  zZ", " /   \\_"],
        Behavior::Wake => &[" /\\_/\\", "( O.O )", " / > <"],
        Behavior::WatchTyping => &[" /\\_/\\", "( o.o )", " / >_>"],
        Behavior::WatchBuild => {
            if alternate {
                &[" /\\_/\\", "( o.o )", " / >[]"]
            } else {
                &[" /\\_/\\", "( o.o )", " / >[=]"]
            }
        }
        Behavior::InspectError => &[" /\\_/\\", "( o.o )", "   > v"],
        Behavior::SitNearError => &[" /\\_/\\", "( •.• )", " /|  |"],
        Behavior::WearyError => &[" /\\_/\\", "( -_- )", " /|  |"],
        Behavior::CelebrateSmall => &["  /\\_/\\", " ( ^.^ )", "  /| |\\"],
        Behavior::CelebrateBig => {
            if alternate {
                &["    /\\_/\\", "\\o/( ^w^ )\\o/", "     / \\"]
            } else {
                &["  \\ /\\_/\\ /", "   ( ^w^ )", "    /   \\"]
            }
        }
        Behavior::RestAfterPush => &[" /\\_/\\", "( ^.^ )", " / >c[_]"],
        Behavior::Approach => &[" /\\_/\\", "( o.o )", " / > <"],
        Behavior::Explore => {
            if alternate {
                &[" /\\_/\\", "( o.o )", "  > ^ <"]
            } else {
                &[" /\\_/\\", "( o.o )", " > ^<"]
            }
        }
        Behavior::Idle => {
            if ((elapsed * 1.2) as u64).is_multiple_of(8) {
                &[" /\\_/\\", "( -.- )", " > ^ <"]
            } else {
                &[" /\\_/\\", "( o.o )", " > ^ <"]
            }
        }
    }
}

fn current_repo_identity() -> (String, String) {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let canonical = cwd.canonicalize().unwrap_or(cwd);
    let key = canonical.to_string_lossy().into_owned();
    let raw_label = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("forge");
    let label = sanitize_terminal_label(raw_label);
    (key, label)
}

fn sanitize_terminal_label(raw: &str) -> String {
    raw.chars()
        .map(|character| {
            if character.is_control() {
                '�'
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_counts_cjk_cells() {
        assert_eq!(truncate_cells("ab生命cd", 6), "ab生命");
    }

    #[test]
    fn state_bar_has_a_fixed_cell_count() {
        let bar = state_bar(0.5, 12);
        assert_eq!(UnicodeWidthStr::width(bar.as_str()), 14);
    }

    #[test]
    fn modified_keys_do_not_trigger_semantic_events_or_quit() {
        let control_success = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        let alt_quit = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT);

        assert_eq!(key_life_event(&control_success), None);
        assert!(!key_requests_quit(&alt_quit));
    }

    #[test]
    fn repository_labels_cannot_inject_terminal_controls() {
        assert_eq!(
            sanitize_terminal_label("repo\u{1b}[2J\nnext"),
            "repo�[2J�next"
        );
    }
}
