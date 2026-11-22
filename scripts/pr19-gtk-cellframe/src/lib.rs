//! PR19 non-production paint/selection proof. No GTK bindings or PTY authority.
use jterm_engine::{
    frame::{CellFrame, Stamp},
    grid::TerminalCell,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    Row,
    Column,
    WideGeometry,
    Stale,
    MultiRow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub stamp: Stamp,
    pub row: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph<'a> {
    pub column: usize,
    pub logical_width: usize,
    pub paint_columns: usize,
    pub cell: &'a TerminalCell,
}

pub struct RowPaint<'a> {
    /// Physical cell paint values, including the continuation's background.
    physical: &'a [TerminalCell],
    wrapped: bool,
}

impl<'a> RowPaint<'a> {
    pub fn physical(&self) -> &'a [TerminalCell] {
        self.physical
    }

    pub fn is_wrapped(&self) -> bool {
        self.wrapped
    }

    fn canonical_col(&self, col: usize) -> Result<usize, ProjectionError> {
        let cell = self.physical.get(col).ok_or(ProjectionError::Column)?;
        Ok(if cell.flags.wide_continuation() {
            col - 1
        } else {
            col
        })
    }

    pub fn glyphs(&self) -> impl Iterator<Item = Glyph<'_>> {
        self.physical
            .iter()
            .enumerate()
            .filter(|(_, cell)| !cell.flags.wide_continuation())
            .map(|(column, cell)| {
                let logical_width = if cell.flags.wide() { 2 } else { 1 };
                Glyph {
                    column,
                    logical_width,
                    paint_columns: logical_width.min(self.physical.len() - column),
                    cell,
                }
            })
    }
}

pub fn paint_row(frame: &CellFrame, row: usize) -> Result<RowPaint<'_>, ProjectionError> {
    let row = frame.rows().get(row).ok_or(ProjectionError::Row)?;
    let mut expect_cont = false;
    for cell in row.cells.iter() {
        if cell.flags.wide_continuation() != expect_cont
            || (cell.flags.wide() && cell.flags.wide_continuation())
        {
            return Err(ProjectionError::WideGeometry);
        }
        expect_cont = cell.flags.wide();
    }
    // ScreenState intentionally retains a width-two lead clipped into the only
    // cell of a 1-column viewport. No other pair may end without its continuation.
    if expect_cont && row.cells.len() != 1 {
        return Err(ProjectionError::WideGeometry);
    }
    Ok(RowPaint {
        physical: &row.cells,
        wrapped: row.wrapped,
    })
}

pub fn anchor(frame: &CellFrame, row: usize, col: usize) -> Result<Anchor, ProjectionError> {
    Ok(Anchor {
        stamp: frame.stamp(),
        row,
        col: paint_row(frame, row)?.canonical_col(col)?,
    })
}

/// Proof of same-row copy for two points from the currently displayed frame.
/// The caller must first verify the attachment binding (a frame stamp is not a lease).
/// Neither multi-row/reflow semantics nor clipboard interaction is implied.
pub fn selected_text(frame: &CellFrame, a: Anchor, b: Anchor) -> Result<String, ProjectionError> {
    if a.stamp != frame.stamp() || b.stamp != frame.stamp() {
        return Err(ProjectionError::Stale);
    }
    if a.row != b.row {
        return Err(ProjectionError::MultiRow);
    }
    let row = paint_row(frame, a.row)?;
    let c0 = row.canonical_col(a.col)?;
    let c1 = row.canonical_col(b.col)?;
    Ok(row
        .glyphs()
        .filter(|glyph| (c0.min(c1)..=c0.max(c1)).contains(&glyph.column))
        .map(|glyph| glyph.cell.character)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_engine::{
        frame::{Cursor, Limits, Receiver, Row, Screen},
        grid::{Color, CursorShape},
        publication::{Subscriber, Update},
    };

    fn frame(revision: u64, layout: u64, screen: Screen, cells: Vec<TerminalCell>) -> CellFrame {
        let cols = cells.len();
        CellFrame::new(
            Stamp {
                epoch: 7,
                revision,
                layout,
                screen,
            },
            cols,
            vec![Row {
                cells: cells.into(),
                wrapped: true,
            }]
            .into(),
            Cursor {
                row: 0,
                col: cols - 1,
                pending_wrap: true,
                visible: true,
                shape: CursorShape::Block,
            },
            Limits {
                max_rows: 1,
                max_cols: 4,
                max_cells: 4,
            },
        )
        .unwrap()
    }

    fn wide() -> Vec<TerminalCell> {
        let mut wide = TerminalCell {
            character: '界',
            ..TerminalCell::default()
        };
        wide.flags.set_wide(true);
        let mut continuation = TerminalCell {
            character: '!',
            background: Color::Indexed(4),
            ..TerminalCell::default()
        };
        continuation.flags.set_wide_continuation(true);
        vec![
            TerminalCell {
                character: 'A',
                ..TerminalCell::default()
            },
            wide,
            continuation,
            TerminalCell {
                character: 'B',
                ..TerminalCell::default()
            },
        ]
    }

    #[test]
    fn physical_background_and_logical_single_glyph_are_separate() {
        let f = frame(1, 1, Screen::Primary, wide());
        let p = paint_row(&f, 0).unwrap();
        assert_eq!(p.physical().len(), 4);
        assert_eq!(p.physical()[2].background, Color::Indexed(4));
        assert!(p.is_wrapped());
        let glyphs: Vec<_> = p
            .glyphs()
            .map(|g| (g.column, g.paint_columns, g.cell.character))
            .collect();
        assert_eq!(glyphs, [(0, 1, 'A'), (1, 2, '界'), (3, 1, 'B')]);
        let lead = anchor(&f, 0, 1).unwrap();
        let tail = anchor(&f, 0, 2).unwrap();
        assert_eq!(lead, tail);
        assert_eq!(selected_text(&f, tail, tail).unwrap(), "界");
        assert_eq!(
            selected_text(&f, anchor(&f, 0, 3).unwrap(), tail).unwrap(),
            "界B"
        );
        assert_eq!(f.cursor().paint_position(), (0, 3));
    }

    #[test]
    fn one_column_engine_wide_lead_preserves_text_and_clips_to_its_cell() {
        use jterm_engine::screen::ScreenState;
        let mut screen = ScreenState::new(1, 1);
        screen.write_char_in_row('界', 2, true);
        let f = frame(1, 1, Screen::Primary, screen.grid[0].to_vec());
        let p = paint_row(&f, 0).unwrap();
        let g = p.glyphs().next().unwrap();
        assert_eq!((g.column, g.logical_width, g.paint_columns), (0, 2, 1));
        let pt = anchor(&f, 0, 0).unwrap();
        assert_eq!(selected_text(&f, pt, pt).unwrap(), "界");
    }

    #[test]
    fn malformed_and_outside_rows_are_rejected_not_guessed() {
        for cells in [
            vec![wide()[2]],
            vec![wide()[0], wide()[1]],
            vec![wide()[1], wide()[3]],
        ] {
            let f = frame(1, 1, Screen::Primary, cells);
            assert_eq!(paint_row(&f, 0).err(), Some(ProjectionError::WideGeometry));
            assert_eq!(anchor(&f, 0, 0), Err(ProjectionError::WideGeometry));
        }
        let f = frame(1, 1, Screen::Primary, wide());
        assert_eq!(anchor(&f, 1, 0), Err(ProjectionError::Row));
        assert_eq!(anchor(&f, 0, 4), Err(ProjectionError::Column));
    }

    #[test]
    fn current_receiver_frame_does_not_copy_old_selection_after_resize_and_screen_change() {
        let old = frame(1, 1, Screen::Primary, wide());
        let new = frame(2, 2, Screen::Alternate, vec![wide()[0]]);
        let mut pubr = Subscriber::new(7).unwrap();
        let mut recv = Receiver::new(7);

        pubr.offer(old).unwrap();
        let Update::Full(f) = pubr.take().unwrap() else {
            panic!()
        };
        let old_stamp = f.stamp();
        recv.full(f).unwrap();
        pubr.acknowledge(old_stamp).unwrap();
        let previous = anchor(recv.frame().unwrap(), 0, 0).unwrap();

        pubr.offer(new).unwrap();
        let Update::Full(next) = pubr.take().unwrap() else {
            panic!()
        };
        recv.full(next).unwrap();
        let current = recv.frame().unwrap();
        let new_selection = anchor(current, 0, 0).unwrap();
        assert_eq!(
            selected_text(current, previous, new_selection),
            Err(ProjectionError::Stale)
        );
        assert_eq!(
            selected_text(current, new_selection, new_selection).unwrap(),
            "A"
        );
    }
}
