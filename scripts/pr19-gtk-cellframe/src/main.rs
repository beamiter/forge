use aether_session::Session;
use gtk_cellframe_display_contract::{anchor, paint_row, selected_text, ProjectionError};
use jterm_engine::{
    frame::{CellFrame, Cursor, Limits, Receiver, Row, Screen, Stamp},
    grid::{Color, CursorShape, TerminalCell},
    publication::{Subscriber, Update},
};
use std::io::{self, Write};
use std::time::{Duration, Instant};

// Deliberately fixture-private. No wire compatibility or runtime authority.
const LIMITS: Limits = Limits {
    max_rows: 2,
    max_cols: 4,
    max_cells: 8,
};

fn glyph(ch: char, rgb: (u8, u8, u8)) -> TerminalCell {
    TerminalCell {
        character: ch,
        background: Color::Rgb(rgb.0, rgb.1, rgb.2),
        ..TerminalCell::default()
    }
}

fn frame(rev: u64, cols: usize, screen: Screen) -> CellFrame {
    let mut cells = vec![glyph('A', (208, 25, 36)), glyph('界', (15, 210, 42))];
    cells[1].flags.set_wide(true);
    let mut continuation = glyph('!', (11, 44, 218));
    continuation.flags.set_wide_continuation(true);
    cells.push(continuation);
    if cols == 4 {
        cells.push(glyph('B', (214, 178, 11)));
    }
    if screen == Screen::Alternate {
        cells[0] = glyph('Z', (135, 23, 201));
    }
    CellFrame::new(
        Stamp {
            epoch: 7,
            revision: rev,
            layout: rev,
            screen,
        },
        cols,
        vec![Row {
            cells: cells.into(),
            wrapped: false,
        }]
        .into(),
        Cursor {
            row: 0,
            col: cols - 1,
            pending_wrap: true,
            visible: true,
            shape: CursorShape::Block,
        },
        LIMITS,
    )
    .unwrap()
}

fn publish(publisher: &mut Subscriber, receiver: &mut Receiver, frame: CellFrame) {
    publisher.offer(frame).unwrap();
    let Some(Update::Full(full)) = publisher.take() else {
        panic!("initial/geometry change must publish complete frame");
    };
    let stamp = full.stamp();
    receiver.full(full).unwrap();
    publisher.acknowledge(stamp).unwrap();
}

fn emit(out: &mut impl Write, frame: &CellFrame) -> io::Result<()> {
    let (r, c) = frame.cursor().paint_position();
    writeln!(
        out,
        "{} {} {} {} {}",
        frame.stamp().revision,
        frame.cols(),
        frame.rows().len(),
        r,
        c
    )?;
    for row in 0..frame.rows().len() {
        let p = paint_row(frame, row).unwrap();
        let mut glyphs = p.glyphs().peekable();
        for (column, cell) in p.physical().iter().enumerate() {
            let (r, g, b) = match cell.background {
                Color::Default => (4, 6, 8),
                Color::Rgb(r, g, b) => (r, g, b),
                _ => panic!("unexpected fixture background"),
            };
            let (cp, width) = if glyphs.peek().is_some_and(|g| g.column == column) {
                let g = glyphs.next().unwrap();
                (u32::from(g.cell.character), g.paint_columns)
            } else {
                (0, 0)
            };
            writeln!(out, "{r} {g} {b} {cp} {width}")?;
        }
    }
    Ok(())
}

fn display_frames(
    out: &mut impl Write,
    primary: CellFrame,
    alternate: CellFrame,
) -> io::Result<()> {
    let epoch = primary.stamp().epoch;
    let mut pubr = Subscriber::new(epoch).unwrap();
    let mut recv = Receiver::new(epoch);
    writeln!(out, "PR19_FRAME_V1")?;
    publish(&mut pubr, &mut recv, primary);
    let old = recv.frame().unwrap();
    let a = anchor(old, 0, 2).unwrap();
    assert_eq!(selected_text(old, a, a), Ok("界".into()));
    emit(out, old)?;

    publish(&mut pubr, &mut recv, alternate);
    let next = recv.frame().unwrap();
    let b = anchor(next, 0, 2).unwrap();
    assert_eq!(selected_text(next, a, b), Err(ProjectionError::Stale));
    assert_eq!(selected_text(next, b, b), Ok("界".into()));
    emit(out, next)
}

fn wait_for_session<T>(
    session: &mut Session,
    description: &str,
    mut ready: impl FnMut(&mut Session) -> Result<Option<T>, String>,
) -> Result<T, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        session.poll()?;
        if let Some(result) = ready(session)? {
            return Ok(result);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {description}"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn send(session: &mut Session, mut bytes: &[u8]) -> Result<(), String> {
    wait_for_session(session, "PTY input", |session| {
        bytes = &bytes[session.write(bytes)?..];
        Ok(bytes.is_empty().then_some(()))
    })
}

fn pty_frame(
    session: &mut Session,
    screen: Screen,
    expected_characters: &[char],
) -> Result<CellFrame, String> {
    wait_for_session(session, "styled PTY frame", |session| {
        let frame = session.terminal().frame()?;
        let matches = frame.stamp().screen == screen
            && frame.rows().len() == 2
            && frame.cols() == expected_characters.len() + 1 // wide continuation
            && frame.cursor().visible
            && frame.cursor().paint_position() == (0, frame.cols() - 1)
            && paint_row(&frame, 0)
                .expect("valid shared engine wide-cell geometry")
                .glyphs()
                .map(|glyph| glyph.cell.character)
                .eq(expected_characters.iter().copied());
        Ok(matches.then_some(frame))
    })
}

fn pty_frames() -> Result<(CellFrame, CellFrame), String> {
    // Verify the child sees the actual kernel winsize before producing each
    // reference frame. The producer needs no startup shell configuration.
    const PRIMARY: &str = concat!(
        r#"PS1=; stty -echo && test "$(stty size)" = '2 4' && printf '"#,
        r"\033[2J\033[H\033[48;2;208;25;36mA\033[48;2;15;210;42m界",
        r"\033[48;2;214;178;11mB\033[0m\033[1;4H'",
        "\n",
    );
    const ALTERNATE: &str = concat!(
        r#"test "$(stty size)" = '2 3' && printf '"#,
        r"\033[?1049h\033[H\033[48;2;135;23;201mZ\033[48;2;15;210;42m界",
        r"\033[0m\033[1;3H'",
        "\n",
    );
    let mut session = Session::spawn(2, 4)?;
    send(&mut session, PRIMARY.as_bytes())?;
    let primary = pty_frame(&mut session, Screen::Primary, &['A', '界', 'B'])?;
    session.resize(2, 3)?;
    send(&mut session, ALTERNATE.as_bytes())?;
    let alternate = pty_frame(&mut session, Screen::Alternate, &['Z', '界'])?;
    send(&mut session, b"exit\n")?;
    wait_for_session(&mut session, "PTY exit and drain", |session| {
        Ok(session.is_drained().then_some(()))
    })?;
    Ok((primary, alternate))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (primary, alternate) = match std::env::args().nth(1).as_deref() {
        None => (frame(1, 4, Screen::Primary), frame(2, 3, Screen::Alternate)),
        Some("--pty") => pty_frames()?,
        Some(_) => return Err("expected no arguments or --pty".into()),
    };
    display_frames(&mut io::stdout().lock(), primary, alternate)?;
    Ok(())
}
