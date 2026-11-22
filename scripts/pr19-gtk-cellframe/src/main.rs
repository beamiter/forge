use gtk_cellframe_display_contract::{anchor, paint_row, selected_text, ProjectionError};
use jterm_engine::{
    frame::{CellFrame, Cursor, Limits, Receiver, Row, Screen, Stamp},
    grid::{Color, CursorShape, TerminalCell},
    publication::{Subscriber, Update},
};
use std::io::{self, Write};

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
            let Color::Rgb(r, g, b) = cell.background else {
                panic!("fixed fixture background")
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

fn main() -> io::Result<()> {
    let mut pubr = Subscriber::new(7).unwrap();
    let mut recv = Receiver::new(7);
    let mut out = io::stdout().lock();
    writeln!(out, "PR19_FRAME_V1")?;
    publish(&mut pubr, &mut recv, frame(1, 4, Screen::Primary));
    let old = recv.frame().unwrap();
    let a = anchor(old, 0, 2).unwrap();
    assert_eq!(selected_text(old, a, a), Ok("界".into()));
    emit(&mut out, old)?;

    publish(&mut pubr, &mut recv, frame(2, 3, Screen::Alternate));
    let next = recv.frame().unwrap();
    let b = anchor(next, 0, 2).unwrap();
    assert_eq!(selected_text(next, a, b), Err(ProjectionError::Stale));
    emit(&mut out, next)
}
