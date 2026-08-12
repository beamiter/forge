// Empirical spike: can block zones be durably anchored to rows of ONE long-lived
// VTE scrollback? Feeds a bare (PTY-less) vte4::Terminal and interrogates the
// coordinate APIs. Run under a headless X server:
//
//   Xvfb :78 -screen 0 1280x900x24 &
//   DISPLAY=:78 GDK_BACKEND=x11 nix develop -c cargo run --example vte_anchor_spike
//
// Experiments:
//   A  feed() synchrony — is the grid current when feed returns?
//   B  trim semantics on a bounded scrollback — do absolute rows and the
//      vadjustment stay in one coordinate space (lower rises) or does the
//      adjustment get renumbered (the click_cursor 44/10 capture)?
//   C  [3J and reset(true,true) — which one produces the 44/10 divergence?
//   E  OSC 8 zone markers — check_hyperlink_at as a per-visible-row zone oracle:
//      correctness, scroll-following, probe cost.
//   D  rewrap on width growth — how far a marker row drifts; whether the
//      deprecated vte_terminal_set_rewrap_on_resize(FALSE) still pins rows.
//   F  text_range_format cost at scale (10k-line history).
//   H  height resize — do absolute ring rows and OSC 8 cell markers survive a
//      HEIGHT-only shrink/grow (rewrap on/off; also mid-screen cursor after
//      [H[2J)?

use gtk::glib;
use gtk4 as gtk;
use vte4::prelude::*;
use vte4::Terminal;

use glib::translate::ToGlibPtr;
use std::time::{Duration, Instant};

async fn sleep_ms(ms: u64) {
    glib::timeout_future(Duration::from_millis(ms)).await;
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Adj {
    lower: f64,
    upper: f64,
    value: f64,
    page: f64,
}

fn adj(term: &Terminal) -> Adj {
    let a = term.vadjustment().expect("vte terminal has a vadjustment");
    Adj {
        lower: a.lower(),
        upper: a.upper(),
        value: a.value(),
        page: a.page_size(),
    }
}

fn cursor(term: &Terminal) -> (i64, i64) {
    let (col, row) = term.cursor_position();
    (col, row)
}

fn report_state(term: &Terminal, label: &str) {
    let (ccol, crow) = cursor(term);
    let a = adj(term);
    println!(
        "  [{label}] cursor=(col {ccol}, row {crow})  adj: lower={} upper={} value={} page={}  grid={}x{}",
        a.lower,
        a.upper,
        a.value,
        a.page,
        term.column_count(),
        term.row_count()
    );
}

/// Wait until cursor row and adjustment upper are stable for two consecutive
/// 25 ms samples (VTE processes feed() asynchronously in main-loop callbacks).
async fn settle(term: &Terminal) {
    let mut last = (i64::MIN, f64::MIN);
    let mut stable = 0;
    for _ in 0..200 {
        sleep_ms(25).await;
        let (_, row) = cursor(term);
        let upper = adj(term).upper;
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
    println!("  !! settle() timed out — state may be in motion");
}

/// Extract one absolute row as plain text (end-exclusive column bound is fine
/// for a probe; VTE clamps).
fn row_text(term: &Terminal, row: i64) -> String {
    let cols = term.column_count();
    let (text, _len) = term.text_range_format(vte4::Format::Text, row, 0, row, cols);
    text.map(|g| g.to_string()).unwrap_or_default()
}

/// Scan [r0, r1] for the first row whose text contains `needle`.
fn find_row(term: &Terminal, needle: &str, r0: i64, r1: i64) -> Option<i64> {
    (r0..=r1).find(|&r| row_text(term, r).contains(needle))
}

fn feed_lines(term: &Terminal, lines: impl IntoIterator<Item = String>) {
    let mut buf = String::new();
    for l in lines {
        buf.push_str(&l);
        buf.push_str("\r\n");
    }
    term.feed(buf.as_bytes());
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

async fn exp_a_feed_synchrony(term: &Terminal) {
    println!("\n== EXP A: feed() synchrony ==");
    let before = cursor(term);
    let t0 = Instant::now();
    term.feed(b"SYNC_A_MARKER\r\n");
    let immediately = cursor(term);
    println!(
        "  cursor before feed: {:?}; immediately after feed(): {:?}  ({})",
        before,
        immediately,
        if before == immediately {
            "grid NOT yet updated => processing is deferred"
        } else {
            "grid ALREADY updated => processing is synchronous"
        }
    );
    // How long until the marker is visible on the grid?
    let mut iters = 0u32;
    loop {
        let (_, crow) = cursor(term);
        if find_row(term, "SYNC_A_MARKER", (crow - 3).max(0), crow).is_some() {
            break;
        }
        iters += 1;
        if iters > 400 {
            println!("  !! marker never appeared");
            return;
        }
        sleep_ms(5).await;
    }
    println!(
        "  marker visible after {:?} (~{} × 5 ms poll iterations)",
        t0.elapsed(),
        iters
    );
}

async fn exp_b_trim(term: &Terminal) {
    println!("\n== EXP B: bounded-scrollback trim (scrollback_lines=100, no reset) ==");
    report_state(term, "before");
    for chunk in 0..3 {
        feed_lines(term, (0..100).map(|i| format!("L{:04}", chunk * 100 + i)));
        settle(term).await;
    }
    report_state(term, "after 300 lines");
    let (_, crow) = cursor(term);
    let a = adj(term);
    println!(
        "  interpretation: cursor row {} vs adj.upper {} => {}",
        crow,
        a.upper,
        if (a.upper - (crow + 1) as f64).abs() < 2.0 {
            "SAME coordinate space (absolute rows; adj.lower rises on trim)"
        } else {
            "DIVERGED (adjustment renumbered relative to ring rows)"
        }
    );
    // Which absolute rows are still extractable?
    let lower = a.lower as i64;
    let probe_evicted = row_text(term, (lower - 10).max(0));
    let probe_low = row_text(term, lower);
    let probe_high = row_text(term, crow - 1);
    println!(
        "  extraction: row {} (below adj.lower) => {:?}; row {} (= adj.lower) => {:?}; row {} (cursor-1) => {:?}",
        (lower - 10).max(0),
        probe_evicted,
        lower,
        probe_low,
        crow - 1,
        probe_high
    );
}

async fn exp_c_clear_and_reset(term: &Terminal) {
    println!("\n== EXP C: [3J then reset(true,true) — hunting the 44/10 divergence ==");
    term.feed(b"\x1b[3J");
    settle(term).await;
    report_state(term, "after [3J (clear scrollback)");
    let (_, crow_3j) = cursor(term);
    println!(
        "  cursor row after [3J: {} (did [3J rebase ring rows?)",
        crow_3j
    );

    // Do rows written before [3J still extract? (zone eviction semantics)
    let pre_3j = row_text(term, 300);
    println!(
        "  extraction of pre-[3J row 300 (was \"L0299\"): {:?} => old rows {}",
        pre_3j,
        if pre_3j.is_empty() {
            "DIE on [3J (zones must evict)"
        } else {
            "SURVIVE [3J"
        }
    );

    term.reset(true, true);
    term.feed(b"\x1b[H\x1b[2J\x1b[3J");
    feed_lines(term, (0..5).map(|i| format!("POSTRESET{}", i)));
    settle(term).await;
    report_state(term, "after reset(true,true) + clears + 5 lines");
    let (_, crow) = cursor(term);
    let a = adj(term);
    if (crow + 1) as f64 > a.upper + 1.0 {
        println!(
            "  => DIVERGENCE reproduced: cursor row {} > adj.upper {} — reset keeps ring rows, adjustment restarts (this is the click_cursor 44/10 case)",
            crow, a.upper
        );
    } else {
        println!(
            "  => no divergence: cursor row {} vs adj.upper {} — reset rebased both spaces together",
            crow, a.upper
        );
    }
}

/// Space-B→Space-A lift: ring row = adjustment row + offset, where the offset
/// is sampled while the cursor sits on the ring's last content row (true at a
/// prompt and during bottom-anchored output) — the click_cursor formula.
fn rebase_offset(term: &Terminal) -> i64 {
    let (_, crow) = cursor(term);
    (crow + 1 - adj(term).upper as i64).max(0)
}

async fn exp_e_hyperlink_zones(term: &Terminal) {
    println!("\n== EXP E: OSC 8 zone markers probed via check_hyperlink_at ==");
    // Feed 5 fake blocks: a hyperlink-wrapped prompt line + 3 output lines
    // each. Zone 3's prompt is long enough to soft-wrap (both rows should
    // carry the URI).
    let mut buf = String::new();
    for z in 0..5 {
        let cmd = if z == 3 {
            "x".repeat(160)
        } else {
            "fake-command".to_string()
        };
        buf.push_str(&format!(
            "\x1b]8;;block://zone/{z}\x1b\\PROMPT-{z} \u{276f} {cmd}\x1b]8;;\x1b\\\r\n"
        ));
        for o in 0..3 {
            buf.push_str(&format!("output {z}.{o}\r\n"));
        }
    }
    term.feed(buf.as_bytes());
    settle(term).await;
    report_state(term, "after 5 zones");

    let ch = term.char_height() as f64;
    let cw = term.char_width() as f64;
    let rows = term.row_count();
    let a = adj(term);
    let off = rebase_offset(term);
    println!(
        "  char cell = {}x{} px; probe x = 1.5 cells; rebase offset (ring - adj) = {off}",
        cw, ch
    );
    let mut hits = Vec::new();
    for v in 0..rows {
        let y = v as f64 * ch + ch / 2.0;
        let uri = term.check_hyperlink_at(cw * 1.5, y);
        let ring_row = a.value as i64 + v + off;
        let txt = row_text(term, ring_row);
        let txt_short: String = txt.chars().take(28).collect();
        if let Some(u) = &uri {
            hits.push((v, u.to_string()));
            println!("  vis row {v:2} (ring {ring_row:3}) uri={u:<18} text={txt_short:?}");
        }
    }
    println!(
        "  {} visible rows carry a zone URI (expect 6: 5 prompts, zone 3 wrapped onto 2 rows)",
        hits.len()
    );

    // EXP E2: probe-based calibration closes the coordinate loop without any
    // bottom-of-screen assumption. Find PROMPT-0's ring row by text, find its
    // visible row by URI, derive offset, predict PROMPT-4's visible row.
    let (_, crow_e2) = cursor(term);
    let ring_p0 = find_row(term, "PROMPT-0", (crow_e2 - 40).max(0), crow_e2);
    let ring_p4 = find_row(term, "PROMPT-4", (crow_e2 - 40).max(0), crow_e2);
    let vis_p0 = hits
        .iter()
        .find(|(_, u)| u == "block://zone/0")
        .map(|(v, _)| *v);
    if let (Some(rp0), Some(rp4), Some(vp0)) = (ring_p0, ring_p4, vis_p0) {
        let calibrated = rp0 - (a.value as i64 + vp0);
        let predicted_v4 = rp4 - calibrated - a.value as i64;
        let y = predicted_v4 as f64 * ch + ch / 2.0;
        let uri = term.check_hyperlink_at(cw * 1.5, y);
        println!(
            "  E2 calibration: PROMPT-0 ring {rp0} vis {vp0} => offset {calibrated} (naive formula said {off}); predicted PROMPT-4 vis {predicted_v4} => probe {:?} ({})",
            uri,
            if uri.as_deref() == Some("block://zone/4") {
                "CALIBRATION EXACT"
            } else {
                "calibration failed"
            }
        );
    } else {
        println!("  E2 calibration: prerequisites missing (p0 {ring_p0:?} p4 {ring_p4:?} vis {vis_p0:?})");
    }

    // Cost: 1000 probes.
    let t0 = Instant::now();
    let mut n = 0i64;
    for i in 0..1000 {
        let v = (i % rows) as f64;
        if term
            .check_hyperlink_at(cw * 1.5, v * ch + ch / 2.0)
            .is_some()
        {
            n += 1;
        }
    }
    println!(
        "  1000 check_hyperlink_at probes: {:?} ({:.2} µs each, {n} hits)",
        t0.elapsed(),
        t0.elapsed().as_micros() as f64 / 1000.0
    );

    // Scroll a prompt row INTO scrollback and probe it there, then push enough
    // lines to trim zone 0-3 off the bounded history and verify the survivor.
    feed_lines(term, (0..50).map(|i| format!("push{i:03}")));
    settle(term).await;
    let (_, crow) = cursor(term);
    let off2 = rebase_offset(term);
    let survivor = find_row(term, "PROMPT-4", (crow - 90).max(0), crow);
    match survivor {
        Some(r) => {
            // Scroll so the survivor is the 3rd visible row.
            let a3 = adj(term);
            let target = ((r - off2 - 2) as f64).clamp(a3.lower, a3.upper - a3.page);
            term.vadjustment().unwrap().set_value(target);
            sleep_ms(80).await;
            let a4 = adj(term);
            let v = r - off2 - a4.value as i64;
            let uri = term.check_hyperlink_at(cw * 1.5, v as f64 * ch + ch / 2.0);
            println!(
                "  after 50-line push: PROMPT-4 at ring row {r}, shown at vis row {v} => uri {:?} (survives scroll into scrollback: {})",
                uri,
                uri.as_deref() == Some("block://zone/4")
            );
        }
        None => println!("  !! PROMPT-4 not found after push — unexpected"),
    }

    // Now push just far enough that zones 0..2 fall off the bounded history
    // while zone 4 survives — pinning "a surviving row's marker is untouched
    // by trims of OTHER rows".
    feed_lines(term, (0..32).map(|i| format!("trimpush{i:03}")));
    settle(term).await;
    let (_, crow2) = cursor(term);
    let gone = find_row(term, "PROMPT-0", (crow2 - 140).max(0), crow2);
    let kept = find_row(term, "PROMPT-4", (crow2 - 140).max(0), crow2);
    println!(
        "  after 32 more lines (cap 100): PROMPT-0 findable: {}; PROMPT-4 findable: {}",
        gone.is_some(),
        kept.is_some()
    );
    if let Some(r) = kept {
        let off3 = rebase_offset(term);
        let a5 = adj(term);
        let target = ((r - off3 - 2) as f64).clamp(a5.lower, a5.upper - a5.page);
        term.vadjustment().unwrap().set_value(target);
        sleep_ms(80).await;
        let a6 = adj(term);
        let v = r - off3 - a6.value as i64;
        let uri = term.check_hyperlink_at(cw * 1.5, v as f64 * ch + ch / 2.0);
        println!(
            "  PROMPT-4 after trim: vis row {v} => uri {:?} (hyperlink attr survives trimming of OTHER rows: {})",
            uri,
            uri.as_deref() == Some("block://zone/4")
        );
    }
    // Back to bottom for whatever runs next.
    let ab = adj(term);
    term.vadjustment().unwrap().set_value(ab.upper - ab.page);
    sleep_ms(50).await;
}

async fn exp_d_rewrap(disable_rewrap: bool) {
    println!(
        "\n== EXP D: rewrap on width growth (rewrap {}) ==",
        if disable_rewrap {
            "DISABLED via FFI"
        } else {
            "default ON"
        }
    );
    let term = Terminal::new();
    term.set_scrollback_lines(1000);
    if disable_rewrap {
        unsafe {
            vte4::ffi::vte_terminal_set_rewrap_on_resize(term.to_glib_none().0, 0);
            let now = vte4::ffi::vte_terminal_get_rewrap_on_resize(term.to_glib_none().0);
            println!("  vte_terminal_get_rewrap_on_resize => {} (0 = off)", now);
        }
    }
    // Narrow window first.
    let win = make_window(&term, 420, 560);
    sleep_ms(200).await;
    settle(&term).await;
    let cols_narrow = term.column_count();
    // 30 long filler lines that wrap at the narrow width, then a short marker.
    let filler = "x".repeat(90);
    feed_lines(&term, (0..30).map(|i| format!("f{i:02} {filler}")));
    term.feed(b"RWMARK_UNIQUE\r\n");
    feed_lines(&term, (0..3).map(|i| format!("tail{i}")));
    settle(&term).await;
    let a = adj(&term);
    let mark_before = find_row(&term, "RWMARK_UNIQUE", a.lower as i64, a.upper as i64);
    let (_, crow_before) = cursor(&term);
    println!(
        "  narrow: {} cols; marker at abs row {:?}; cursor row {}; upper {}",
        cols_narrow, mark_before, crow_before, a.upper
    );

    // Force the window wider: raise the terminal's own minimum width.
    term.set_size_request(980, 560);
    sleep_ms(300).await;
    settle(&term).await;
    let cols_wide = term.column_count();
    let a2 = adj(&term);
    let mark_after = find_row(&term, "RWMARK_UNIQUE", a2.lower as i64, a2.upper as i64);
    let (_, crow_after) = cursor(&term);
    println!(
        "  wide:   {} cols; marker at abs row {:?}; cursor row {}; upper {}",
        cols_wide, mark_after, crow_after, a2.upper
    );
    match (mark_before, mark_after) {
        (Some(b), Some(w)) => println!(
            "  => marker row drift: {} ({} - {}); cursor drift {}; {}",
            w - b,
            w,
            b,
            crow_after - crow_before,
            if w == b {
                "rows PINNED across width change"
            } else {
                "rows REWRAPPED (anchors invalidated)"
            }
        ),
        _ => println!("  !! marker lost across resize"),
    }
    win.close();
}

// ---- EXP H: height resize vs ring rows and OSC 8 markers -------------------

#[derive(Clone, Copy, Debug)]
struct HSnap {
    crow: i64,
    a: Adj,
    grid_rows: i64,
    /// rebase offset (ring space - adjustment space) sampled at snapshot time
    off: i64,
    marker_ring: Option<i64>,
    link_ring: Option<i64>,
    uri_vis: Option<i64>,
}

fn h_new_term(disable_rewrap: bool) -> Terminal {
    let term = Terminal::new();
    term.set_scrollback_lines(1000);
    term.set_allow_hyperlink(true);
    if disable_rewrap {
        unsafe {
            vte4::ffi::vte_terminal_set_rewrap_on_resize(term.to_glib_none().0, 0);
        }
    }
    term
}

/// 60 short numbered lines (no soft-wrap, so height is the only variable) with
/// ONE OSC 8 hyperlink line and ONE unique text marker line spliced in after
/// `link_at` of them.
fn h_feed_content(term: &Terminal, scen: &str, uri: &str, link_at: usize) {
    feed_lines(term, (0..link_at).map(|i| format!("{scen} line {i:03}")));
    term.feed(
        format!("\x1b]8;;{uri}\x1b\\HLINK_{scen} anchored-link-line\x1b]8;;\x1b\\\r\n").as_bytes(),
    );
    term.feed(format!("HMARK_{scen}_UNIQUE marker-line\r\n").as_bytes());
    feed_lines(term, (link_at..60).map(|i| format!("{scen} line {i:03}")));
}

/// Record cursor, adjustment, marker/link ring rows (by text), and which
/// visible row the URI probe hits at x = 0.5 cells (column 0).
fn h_snapshot(term: &Terminal, label: &str, uri: &str, marker: &str, link: &str) -> HSnap {
    let (_, crow) = cursor(term);
    let a = adj(term);
    let off = rebase_offset(term);
    let marker_ring = find_row(term, marker, a.lower as i64, a.upper as i64);
    let link_ring = find_row(term, link, a.lower as i64, a.upper as i64);
    let ch = term.char_height() as f64;
    let cw = term.char_width() as f64;
    let rows = term.row_count();
    let mut uri_vis = None;
    for v in 0..rows {
        if term
            .check_hyperlink_at(cw * 0.5, v as f64 * ch + ch / 2.0)
            .as_deref()
            == Some(uri)
        {
            uri_vis = Some(v);
            break;
        }
    }
    println!(
        "  [{label}] cursor row {crow}; adj lower={} upper={} value={} page={}; grid {}x{}; marker ring {:?}; link ring {:?}; URI probe vis row {:?} (top vis ring {})",
        a.lower,
        a.upper,
        a.value,
        a.page,
        term.column_count(),
        rows,
        marker_ring,
        link_ring,
        uri_vis,
        a.value as i64 + off
    );
    HSnap {
        crow,
        a,
        grid_rows: rows,
        off,
        marker_ring,
        link_ring,
        uri_vis,
    }
}

fn h_verdict(tag: &str, before: &HSnap, after: &HSnap) {
    let fmt = |b: Option<i64>, a: Option<i64>| match (b, a) {
        (Some(b), Some(a)) => format!("{:+}", a - b),
        _ => "n/a (lost)".to_string(),
    };
    let expected_vis = after
        .link_ring
        .map(|r| r - (after.a.value as i64 + after.off));
    let exact = after.uri_vis.is_some() && after.uri_vis == expected_vis;
    println!(
        "  => {tag}: marker ring drift {}; link ring drift {}; cursor drift {:+}; grid rows {} -> {}; top vis ring {} -> {}; URI probe at vis {:?} (expected {:?}) => {}",
        fmt(before.marker_ring, after.marker_ring),
        fmt(before.link_ring, after.link_ring),
        after.crow - before.crow,
        before.grid_rows,
        after.grid_rows,
        before.a.value as i64 + before.off,
        after.a.value as i64 + after.off,
        after.uri_vis,
        expected_vis,
        if exact { "probe EXACT" } else { "probe MISMATCH/none" }
    );
}

/// Shrink then grow the terminal HEIGHT only (width request stays 600 px).
/// Prefers an in-place shrink: lower the size_request minimum, then drop the
/// window's default size (GTK4's replacement for gtk_window_resize). If the
/// window refuses under no-WM Xvfb, falls back to a SECOND window at the small
/// height fed identical bytes, comparing ring rows across the two.
async fn h_shrink_grow_flow(
    scen: &str,
    uri: &str,
    disable_rewrap: bool,
    shrink_tag: &str,
    grow_tag: &str,
) {
    println!(
        "\n-- {scen} flow: height shrink then grow (rewrap {}) --",
        if disable_rewrap {
            "DISABLED via FFI"
        } else {
            "default ON"
        }
    );
    let marker = format!("HMARK_{scen}_UNIQUE");
    let link = format!("HLINK_{scen}");
    let term = h_new_term(disable_rewrap);
    term.set_size_request(600, 600);
    let win = make_window(&term, 640, 620);
    sleep_ms(300).await;
    settle(&term).await;
    h_feed_content(&term, scen, uri, 56);
    settle(&term).await;
    let tall = h_snapshot(&term, "tall", uri, &marker, &link);

    term.set_size_request(600, 280);
    win.set_default_size(640, 300);
    sleep_ms(300).await;
    settle(&term).await;
    if term.row_count() < tall.grid_rows {
        println!("  shrink method: IN-PLACE (lowered size_request + set_default_size)");
        let short = h_snapshot(&term, "short", uri, &marker, &link);
        h_verdict(shrink_tag, &tall, &short);
        term.set_size_request(600, 600);
        win.set_default_size(640, 620);
        sleep_ms(300).await;
        settle(&term).await;
        let tall2 = h_snapshot(&term, "tall-again", uri, &marker, &link);
        h_verdict(grow_tag, &short, &tall2);
        h_verdict(
            &format!("{scen} round-trip tall->short->tall"),
            &tall,
            &tall2,
        );
    } else {
        println!(
            "  shrink method: SECOND WINDOW (in-place shrink refused; rows stayed {})",
            term.row_count()
        );
        let term2 = h_new_term(disable_rewrap);
        term2.set_size_request(600, 280);
        let win2 = make_window(&term2, 640, 300);
        sleep_ms(300).await;
        settle(&term2).await;
        h_feed_content(&term2, scen, uri, 56);
        settle(&term2).await;
        let short = h_snapshot(
            &term2,
            "short (2nd window, identical bytes)",
            uri,
            &marker,
            &link,
        );
        h_verdict(shrink_tag, &tall, &short);
        // Growing works in place even without a WM: raise the minimum back.
        term2.set_size_request(600, 600);
        sleep_ms(300).await;
        settle(&term2).await;
        let tall2 = h_snapshot(&term2, "grown (2nd window)", uri, &marker, &link);
        h_verdict(grow_tag, &short, &tall2);
        h_verdict(&format!("{scen} round-trip vs 1st tall"), &tall, &tall2);
        win2.close();
    }
    win.close();
}

/// H4: height change while the cursor sits mid-screen after [H[2J. The link +
/// marker are fed EARLY (after 20 lines) so they are already in scrollback when
/// the clear erases the visible screen.
async fn h4_clear_then_resize() {
    println!("\n-- H4: height change with cursor mid-screen after [H[2J --");
    let uri = "block://h/4";
    let marker = "HMARK_H4_UNIQUE";
    let link = "HLINK_H4";
    let term = h_new_term(false);
    term.set_size_request(600, 600);
    let win = make_window(&term, 640, 620);
    sleep_ms(300).await;
    settle(&term).await;
    h_feed_content(&term, "H4", uri, 20);
    settle(&term).await;
    term.feed(b"\x1b[H\x1b[2J");
    feed_lines(&term, (0..5).map(|i| format!("H4 postclear {i}")));
    settle(&term).await;
    let before = h_snapshot(&term, "post-clear tall", uri, marker, link);
    let a = adj(&term);
    let erased = find_row(&term, "H4 line 059", a.lower as i64, a.upper as i64);
    println!(
        "  pre-clear on-screen line \"H4 line 059\" after [2J: {:?} ({})",
        erased,
        if erased.is_some() {
            "screen content SURVIVES in ring"
        } else {
            "screen content ERASED by [2J"
        }
    );

    term.set_size_request(600, 280);
    win.set_default_size(640, 300);
    sleep_ms(300).await;
    settle(&term).await;
    let (after, how) = if term.row_count() < before.grid_rows {
        (
            h_snapshot(&term, "post-clear short", uri, marker, link),
            "IN-PLACE SHRINK",
        )
    } else {
        term.set_size_request(600, 780);
        win.set_default_size(640, 800);
        sleep_ms(300).await;
        settle(&term).await;
        (
            h_snapshot(&term, "post-clear grown", uri, marker, link),
            "IN-PLACE GROW (shrink refused)",
        )
    };
    println!("  H4 height-change method: {how}");
    h_verdict("H4 clear+resize", &before, &after);

    // The link lives in scrollback (invisible to the viewport probe); scroll it
    // to the 3rd visible row and probe it there.
    if let Some(r) = after.link_ring {
        let off = rebase_offset(&term);
        let av = adj(&term);
        let target = ((r - off - 2) as f64).clamp(av.lower, av.upper - av.page);
        term.vadjustment().unwrap().set_value(target);
        sleep_ms(80).await;
        let a2 = adj(&term);
        let v = r - off - a2.value as i64;
        let ch = term.char_height() as f64;
        let cw = term.char_width() as f64;
        let got = term.check_hyperlink_at(cw * 0.5, v as f64 * ch + ch / 2.0);
        println!(
            "  scrolled link into view: ring {r} at vis row {v} => probe {:?} ({})",
            got,
            if got.as_deref() == Some(uri) {
                "URI SURVIVES clear+resize"
            } else {
                "URI LOST"
            }
        );
    }
    win.close();
}

async fn exp_h_height_resize() {
    println!("\n== EXP H: height resize vs absolute ring rows and OSC 8 markers ==");
    h_shrink_grow_flow(
        "H1",
        "block://h/1",
        false,
        "H1 height-shrink (rewrap ON)",
        "H2 height-grow-back (rewrap ON)",
    )
    .await;
    h_shrink_grow_flow(
        "H3",
        "block://h/3",
        true,
        "H3 height-shrink (rewrap OFF)",
        "H3 height-grow-back (rewrap OFF)",
    )
    .await;
    h4_clear_then_resize().await;
}

async fn exp_f_extraction_cost() {
    println!("\n== EXP F: text_range_format cost at 10k-line history ==");
    let term = Terminal::new();
    term.set_scrollback_lines(12_000);
    let win = make_window(&term, 900, 620);
    sleep_ms(200).await;
    for chunk in 0..20 {
        feed_lines(
            &term,
            (0..500).map(|i| format!("bulk line {:05} with some payload text", chunk * 500 + i)),
        );
        settle(&term).await;
    }
    let a = adj(&term);
    let (r0, r1) = (a.lower as i64, a.upper as i64 - 1);
    println!("  history: rows [{r0}, {r1}] ({} rows)", r1 - r0 + 1);

    let cols = term.column_count();
    let t0 = Instant::now();
    let (full, len) = term.text_range_format(vte4::Format::Text, r0, 0, r1, cols);
    println!(
        "  full-range extraction: {:?} for {} bytes ({} rows)",
        t0.elapsed(),
        len,
        r1 - r0 + 1
    );
    drop(full);

    let vis_rows = term.row_count();
    let top = a.value as i64;
    let t1 = Instant::now();
    for _ in 0..100 {
        let _ = term.text_range_format(vte4::Format::Text, top, 0, top + vis_rows - 1, cols);
    }
    println!(
        "  viewport ({} rows) extraction ×100: {:?} ({:.0} µs each)",
        vis_rows,
        t1.elapsed(),
        t1.elapsed().as_micros() as f64 / 100.0
    );

    let t2 = Instant::now();
    for r in top..top + vis_rows {
        let _ = row_text(&term, r);
    }
    println!(
        "  per-row extraction of one viewport: {:?} ({:.0} µs/row)",
        t2.elapsed(),
        t2.elapsed().as_micros() as f64 / vis_rows as f64
    );
    win.close();
}

async fn run() {
    println!("VTE anchor spike — vte4 0.10 (feature v0_76), libvte reported below");
    // libvte version via FFI (safe wrapper not available at v0_76 in this crate).
    unsafe {
        let major = vte4::ffi::vte_get_major_version();
        let minor = vte4::ffi::vte_get_minor_version();
        let micro = vte4::ffi::vte_get_micro_version();
        println!("libvte {}.{}.{}", major, minor, micro);
    }

    let term = Terminal::new();
    term.set_scrollback_lines(100);
    term.set_allow_hyperlink(true);
    let _win = make_window(&term, 900, 620);
    sleep_ms(300).await; // allow realize/allocate
    settle(&term).await;
    report_state(&term, "fresh terminal");

    exp_a_feed_synchrony(&term).await;
    exp_b_trim(&term).await;
    exp_c_clear_and_reset(&term).await;
    exp_e_hyperlink_zones(&term).await;
    exp_d_rewrap(false).await;
    exp_d_rewrap(true).await;
    exp_h_height_resize().await;
    exp_f_extraction_cost().await;

    println!("\nSpike complete.");
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
