// Adversarial follow-up spike for the unified-mode design review. Settles the
// VTE mechanism claims the first spike did not cover:
//
//   G1  Does a plain-text reprint over OSC 8-marked cells (jsh's per-keystroke
//       repaint: CUU + CR + ED0 + Print) kill the hyperlink attribute?
//   G2  Does DECSC/DECRC (ESC 7 / ESC 8) save & restore the *hyperlink* state,
//       i.e. can a guest cursor-restore silently drop an injected open link?
//   G3  Do cells filled by erase (CSI K / CSI J) while a link is open carry the
//       hyperlink? (blank rows inside a marked region; probe semantics)
//   G4  Rewrap ON: does the hyperlink attribute follow the text across a width
//       change? (draft §2.4 calls this "verified" — spike 1 never probed it)
//   G5  scroll-on-output default value; does the view still follow output when
//       already at bottom with it off; does a scrolled-up view stay put?
//   G8  Rewrap OFF: is a width *shrink* destructive (content truncated) or
//       just clipped (restored on re-grow)?
//   G6  set_size() vs allocation-driven grid: does an explicit grid stick, and
//       what wins after a real allocation change?
//
//   Xvfb :78 -screen 0 1280x900x24 &
//   DISPLAY=:78 GDK_BACKEND=x11 nix develop -c cargo run --example vte_anchor_spike2

use gtk::glib;
use gtk4 as gtk;
use vte4::prelude::*;
use vte4::Terminal;

use glib::translate::ToGlibPtr;
use std::time::Duration;

async fn sleep_ms(ms: u64) {
    glib::timeout_future(Duration::from_millis(ms)).await;
}

async fn settle(term: &Terminal) {
    let mut last = (i64::MIN, f64::MIN);
    let mut stable = 0;
    for _ in 0..200 {
        sleep_ms(25).await;
        let (_, row) = term.cursor_position();
        let upper = term.vadjustment().unwrap().upper();
        if last == (row, upper) {
            stable += 1;
            if stable >= 2 {
                return;
            }
        } else {
            stable = 0;
            last = (row, upper);
        }
    }
    println!("  !! settle() timed out");
}

fn row_text(term: &Terminal, row: i64) -> String {
    let cols = term.column_count();
    let (text, _len) = term.text_range_format(vte4::Format::Text, row, 0, row, cols);
    text.map(|g| g.to_string()).unwrap_or_default()
}

fn find_row(term: &Terminal, needle: &str, r0: i64, r1: i64) -> Option<i64> {
    (r0.max(0)..=r1).find(|&r| row_text(term, r).contains(needle))
}

/// Probe the hyperlink at (visible col, visible row) in cells.
fn probe(term: &Terminal, vcol: f64, vrow: i64) -> Option<String> {
    let ch = term.char_height() as f64;
    let cw = term.char_width() as f64;
    term.check_hyperlink_at(cw * (vcol + 0.5), vrow as f64 * ch + ch / 2.0)
        .map(|g| g.to_string())
}

/// Visible row of an absolute ring row, using probe-free arithmetic valid while
/// the terminal has never scrolled (adj.value = 0, no trim): vis = ring - 0.
fn vis_of_ring_untrimmed(term: &Terminal, ring: i64) -> i64 {
    let a = term.vadjustment().unwrap();
    ring - a.value() as i64
}

async fn g1_overwrite_kills_link() {
    println!("\n== G1: plain reprint over marked prompt cells (jsh repaint idiom) ==");
    let term = Terminal::new();
    term.set_scrollback_lines(1000);
    term.set_allow_hyperlink(true);
    let win = make_window(&term, 900, 620);
    sleep_ms(300).await;
    settle(&term).await;

    // Marked prompt line, as the injector would produce at FTCS A..B.
    term.feed(b"\x1b]8;;block://n/1\x1b\\PROMPT-G1 \xe2\x9d\xaf \x1b]8;;\x1b\\\r\n");
    settle(&term).await;
    let (_, crow) = term.cursor_position();
    let prompt_ring = find_row(&term, "PROMPT-G1", 0, crow).expect("prompt row");
    let v = vis_of_ring_untrimmed(&term, prompt_ring);
    println!(
        "  after initial paint: probe(row {v}) = {:?}",
        probe(&term, 1.0, v)
    );

    // jsh keystroke repaint: cursor is on the line under the prompt (we fed
    // \r\n), so MoveUp(1) + CR + ED0 + reprint plain text + the typed char.
    term.feed(b"\x1b[A\r\x1b[JPROMPT-G1 \xe2\x9d\xaf l");
    settle(&term).await;
    let after = probe(&term, 1.0, v);
    println!(
        "  after plain repaint: probe(row {v}) = {:?}  => marker {}",
        after,
        if after.is_none() {
            "DESTROYED by reprint"
        } else {
            "survived (!)"
        }
    );
    win.close();
}

async fn g2_decsc_decrc() {
    println!("\n== G2: DECSC/DECRC vs hyperlink state ==");
    let term = Terminal::new();
    term.set_scrollback_lines(1000);
    term.set_allow_hyperlink(true);
    let win = make_window(&term, 900, 620);
    sleep_ms(300).await;
    settle(&term).await;

    // Save cursor with NO link open, open link, print X, restore, print Y.
    // If DECRC restores hyperlink state, Y is unlinked.
    term.feed(b"\x1b7\x1b]8;;block://n/2\x1b\\X");
    term.feed(b"\x1b8\x1b[2CY\r\n"); // restore (col 0) then move right 2, print Y at col 2
    settle(&term).await;
    let (_, crow) = term.cursor_position();
    let ring = find_row(&term, "X", 0, crow).unwrap_or(0);
    let v = vis_of_ring_untrimmed(&term, ring);
    let px = probe(&term, 0.0, v); // X at col 0
    let py = probe(&term, 2.0, v); // Y at col 2
    println!(
        "  X(col0)={px:?}  Y(col2 after DECRC)={py:?}  => DECRC {} hyperlink state",
        if py.is_none() {
            "RESTORES (clobbers an open injected link)"
        } else {
            "does NOT touch"
        }
    );
    win.close();
}

async fn g3_erase_fill() {
    println!("\n== G3: erase fill while link open (CSI K / blank rows) ==");
    let term = Terminal::new();
    term.set_scrollback_lines(1000);
    term.set_allow_hyperlink(true);
    let win = make_window(&term, 900, 620);
    sleep_ms(300).await;
    settle(&term).await;

    // Open a link, print AB, erase to end of line (fill), newline (blank row
    // "printed" while link open), print C, close.
    term.feed(b"\x1b]8;;block://n/3\x1b\\AB\x1b[K\r\n\r\nC\x1b]8;;\x1b\\\r\n");
    settle(&term).await;
    let (_, crow) = term.cursor_position();
    let ring_ab = find_row(&term, "AB", 0, crow).unwrap_or(0);
    let v_ab = vis_of_ring_untrimmed(&term, ring_ab);
    let p_text = probe(&term, 0.0, v_ab); // over 'A'
    let p_fill = probe(&term, 10.0, v_ab); // over CSI K fill area
    let p_blank = probe(&term, 0.0, v_ab + 1); // the \r\n-skipped blank row
    println!("  'A' cell => {p_text:?}; K-fill cell => {p_fill:?}; blank row => {p_blank:?}");
    println!(
        "  => a zone-wide OUTPUT wrap {} rows that are blank/erased",
        if p_blank.is_some() {
            "COVERS even"
        } else {
            "MISSES"
        }
    );
    win.close();
}

async fn g4_rewrap_on_link_follows() {
    println!("\n== G4: rewrap ON — does the hyperlink follow rewrapped text? ==");
    let term = Terminal::new();
    term.set_scrollback_lines(1000);
    term.set_allow_hyperlink(true);
    let win = make_window(&term, 420, 560); // narrow
    sleep_ms(300).await;
    settle(&term).await;

    let filler = "y".repeat(90);
    for i in 0..20 {
        term.feed(format!("f{i:02} {filler}\r\n").as_bytes());
    }
    term.feed(b"\x1b]8;;block://n/4\x1b\\G4MARK prompt row\x1b]8;;\x1b\\\r\n");
    for i in 0..3 {
        term.feed(format!("tail{i}\r\n").as_bytes());
    }
    settle(&term).await;
    let a = term.vadjustment().unwrap();
    let mark_narrow = find_row(&term, "G4MARK", a.lower() as i64, a.upper() as i64);
    println!(
        "  narrow ({} cols): G4MARK at ring {:?}",
        term.column_count(),
        mark_narrow
    );

    term.set_size_request(980, 560);
    sleep_ms(300).await;
    settle(&term).await;
    let a2 = term.vadjustment().unwrap();
    let mark_wide = find_row(&term, "G4MARK", a2.lower() as i64, a2.upper() as i64);
    println!(
        "  wide ({} cols): G4MARK at ring {:?}",
        term.column_count(),
        mark_wide
    );
    if let Some(r) = mark_wide {
        // Terminal never scrolled? It did — compute vis via bottom pin.
        let (_, crow) = term.cursor_position();
        let av = term.vadjustment().unwrap();
        let off = (crow + 1 - av.upper() as i64).max(0);
        let v = r - off - av.value() as i64;
        let p = probe(&term, 1.0, v);
        println!(
            "  probe at rewrapped row {r} (vis {v}) => {p:?}  => hyperlink {} the text",
            if p.as_deref() == Some("block://n/4") {
                "FOLLOWS"
            } else {
                "did NOT follow"
            }
        );
    }
    win.close();
}

async fn g5_scroll_on_output() {
    println!("\n== G5: scroll-on-output default & bottom-follow semantics ==");
    let term = Terminal::new();
    term.set_scrollback_lines(200);
    let win = make_window(&term, 900, 620);
    sleep_ms(300).await;
    settle(&term).await;
    println!(
        "  fresh terminal: is_scroll_on_output = {}, is_scroll_on_keystroke = {}",
        term.is_scroll_on_output(),
        term.is_scroll_on_keystroke()
    );
    term.set_scroll_on_output(false);

    // Fill past one page, stay at bottom, feed more: does view follow?
    for i in 0..80 {
        term.feed(format!("fill{i:03}\r\n").as_bytes());
    }
    settle(&term).await;
    let a = term.vadjustment().unwrap();
    let at_bottom_before = a.value() >= a.upper() - a.page_size() - 0.5;
    for i in 0..20 {
        term.feed(format!("more{i:03}\r\n").as_bytes());
    }
    settle(&term).await;
    let a1 = term.vadjustment().unwrap();
    let at_bottom_after = a1.value() >= a1.upper() - a1.page_size() - 0.5;
    println!(
        "  at bottom before feed: {at_bottom_before}; still pinned after feed with scroll_on_output=false: {at_bottom_after}"
    );

    // Scroll up to the middle, feed more (no trim yet): does value stay?
    let mid = (a1.upper() - a1.page_size()) / 2.0;
    term.vadjustment().unwrap().set_value(mid);
    sleep_ms(80).await;
    let v_before = term.vadjustment().unwrap().value();
    let top_text_before = row_text(&term, v_before as i64);
    for i in 0..30 {
        term.feed(format!("flood{i:03}\r\n").as_bytes());
    }
    settle(&term).await;
    let v_after = term.vadjustment().unwrap().value();
    let top_text_after = row_text(&term, v_after as i64);
    println!(
        "  scrolled-up feed (no trim): value {v_before} -> {v_after}; top row text {:?} -> {:?} (view {})",
        top_text_before,
        top_text_after,
        if (v_before - v_after).abs() < 0.5 && top_text_before == top_text_after {
            "STABLE"
        } else {
            "MOVED"
        }
    );

    // Now force trim (cap 200) while scrolled up: does VTE compensate value?
    for i in 0..250 {
        term.feed(format!("trim{i:03}\r\n").as_bytes());
    }
    settle(&term).await;
    let a2 = term.vadjustment().unwrap();
    println!(
        "  after trim flood while scrolled up: value={} lower={} upper={} (content at old reading position is {})",
        a2.value(),
        a2.lower(),
        a2.upper(),
        if row_text(&term, a2.value() as i64) == top_text_after { "unchanged" } else { "DIFFERENT (view slid)" }
    );
    win.close();
}

async fn g8_rewrap_off_shrink() {
    println!("\n== G8: rewrap OFF — is a width shrink destructive? ==");
    let term = Terminal::new();
    term.set_scrollback_lines(1000);
    unsafe {
        vte4::ffi::vte_terminal_set_rewrap_on_resize(term.to_glib_none().0, 0);
    }
    let win = make_window(&term, 980, 560); // wide first
    sleep_ms(300).await;
    settle(&term).await;
    let cols_wide = term.column_count();
    let payload = format!("HEAD {} TAIL", "z".repeat(70));
    term.feed(format!("{payload}\r\n").as_bytes());
    settle(&term).await;
    let r = find_row(&term, "HEAD", 0, 40).expect("payload row");
    let before = row_text(&term, r);
    println!(
        "  wide ({cols_wide} cols): row {r} = {:?} (len {})",
        &before[..20.min(before.len())],
        before.len()
    );

    // Shrink hard, then grow back.
    term.set_size_request(1, 1);
    win.set_default_width(340);
    win.set_default_height(560);
    sleep_ms(400).await;
    settle(&term).await;
    let cols_narrow = term.column_count();
    let during = row_text(&term, r);
    term.set_size_request(980, 560);
    sleep_ms(400).await;
    settle(&term).await;
    let cols_back = term.column_count();
    let after = row_text(&term, r);
    println!(
        "  narrow ({cols_narrow} cols): row len {}; regrown ({cols_back} cols): row len {} — TAIL {}",
        during.len(),
        after.len(),
        if after.contains("TAIL") { "PRESERVED (clip only)" } else { "LOST (shrink is destructive)" }
    );
    win.close();
}

async fn g6_set_size_vs_allocation() {
    println!("\n== G6: set_size vs allocation-driven grid ==");
    let term = Terminal::new();
    term.set_scrollback_lines(100);
    let win = make_window(&term, 900, 620);
    sleep_ms(300).await;
    settle(&term).await;
    println!(
        "  allocated grid: {}x{}",
        term.column_count(),
        term.row_count()
    );
    term.set_size(200, 50);
    sleep_ms(150).await;
    println!(
        "  after set_size(200,50): {}x{}",
        term.column_count(),
        term.row_count()
    );
    // Force a real allocation change.
    win.set_default_width(920);
    win.set_default_height(640);
    sleep_ms(400).await;
    println!(
        "  after window resize (reallocation): {}x{} — allocation wins if this reverted to ~window-derived size",
        term.column_count(),
        term.row_count(),
    );
    win.close();
}

fn make_window(term: &Terminal, width: i32, height: i32) -> gtk::Window {
    let win = gtk::Window::builder()
        .default_width(width)
        .default_height(height)
        .build();
    win.set_child(Some(term));
    win.present();
    win
}

async fn run() {
    unsafe {
        println!(
            "libvte {}.{}.{}",
            vte4::ffi::vte_get_major_version(),
            vte4::ffi::vte_get_minor_version(),
            vte4::ffi::vte_get_micro_version()
        );
    }
    g1_overwrite_kills_link().await;
    g2_decsc_decrc().await;
    g3_erase_fill().await;
    g4_rewrap_on_link_follows().await;
    g5_scroll_on_output().await;
    g8_rewrap_off_shrink().await;
    g6_set_size_vs_allocation().await;
    println!("\nSpike 2 complete.");
}

fn main() {
    gtk::init().expect("GTK init (needs a display; run under Xvfb)");
    let ctx = glib::MainContext::default();
    ctx.spawn_local(async {
        run().await;
        std::process::exit(0);
    });
    glib::MainLoop::new(None, false).run();
}
