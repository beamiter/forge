//! Kitty graphics protocol — APC `\e_G<keys>;<base64>\e\\`.
//!
//! The structural half of the protocol lives in [`jterm_core::kitty_graphics`]
//! and is shared with the rest of the family: control-data parsing, `m=1` chunk
//! assembly, base64 decoding, raw-format length validation and the PNG IHDR
//! sniff. This module keeps only what needs GTK or a reply on the wire:
//!
//! - the GDK decode path ([`gdk::Texture::from_bytes`] for `f=100`, a
//!   [`gdk::MemoryTexture`] for `f=24`/`f=32`),
//! - the `a=q` support probe ([`query_outcome`]),
//! - the PTY responder ([`response_for`]),
//! - the per-block image budget ([`MAX_PENDING_BYTES_PER_BLOCK`], enforced by
//!   the caller in `block_view/mod.rs`).
//!
//! Supported: `a=T` (transmit + display) and `a=t` (transmit only — buffered
//! but not auto-displayed); `a=q` (support probe) is validated and answered but
//! never displayed; `a=d`/`a=p` are answered `ENOTSUP` and dropped. `f=100`
//! (PNG), `f=32` (RGBA, the protocol default) and `f=24` (RGB) with the inline
//! `t=d` transport only. Chunked transmission via `m=1` + final `m=0`, where a
//! continuation may carry only `m=` and an optional `q=`; any other action
//! arriving mid-upload drops the in-flight chunks.
//!
//! libvte does not implement this protocol, so block mode consumes APC G
//! payloads before VTE sees them and renders the decoded image as a GTK
//! Picture appended to the finished block.
//!
//! Memory caps come from [`Caps::BLOCK`], the family's block-terminal budget:
//! 16 MiB encoded per image, 16 MiB decoded, 16 MiB across all in-flight
//! uploads, and a 16384-pixel edge. Oversize payloads are dropped.
//!
//! Unlike anvil (which never answers), commands that carry an `i=`/`I=`
//! identifier receive an `OK`/error reply on the PTY via [`response_for`],
//! following ember — the family's reference responder. See that function for
//! the deliberate divergences.

use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::Cast;

use crate::kitty_graphics as core;
use crate::kitty_graphics::{Action, Assembled, Caps, Error, Format, Step};

/// Memory budget for one block's graphics traffic — the family's block preset.
const CAPS: Caps = Caps::BLOCK;

/// Per-block decoded image bytes cap (sum of all images attached to a block).
/// A decoded image may occupy at most the same budget, so one image can fill a
/// block on its own but two oversize ones cannot.
pub(crate) const MAX_PENDING_BYTES_PER_BLOCK: usize = CAPS.max_decoded_bytes;
/// Bound GTK/GDK object fan-out independently from decoded pixels. Tiny images
/// still allocate a Texture, Picture, signal state and widget-tree nodes.
pub(crate) const MAX_IMAGES_PER_BLOCK: usize = 64;
/// Conservative fixed retained charge for one Texture + Picture pair and its
/// surrounding GTK/GDK bookkeeping. Pixel backing is charged separately.
pub(crate) const RETAINED_BYTES_PER_IMAGE_OBJECT: usize = 64 * 1024;

pub(crate) fn pending_image_bytes_after_admission(
    retained_bytes: usize,
    image_count: usize,
    pixel_bytes: usize,
    encoded_source_backing_bytes: usize,
) -> Option<usize> {
    if image_count >= MAX_IMAGES_PER_BLOCK {
        return None;
    }
    let next = retained_bytes
        .checked_add(pixel_bytes)?
        .checked_add(encoded_source_backing_bytes)?
        .checked_add(RETAINED_BYTES_PER_IMAGE_OBJECT)?;
    (next <= MAX_PENDING_BYTES_PER_BLOCK).then_some(next)
}

/// Parsed result of a single APC G chunk. `Complete` carries a finished image
/// ready to render; `Pending` means more chunks are expected; `Skipped` means
/// the chunk was valid but unsupported (e.g. `a=d`) — caller should drop it.
pub(crate) enum Outcome {
    Complete {
        texture: gdk::Texture,
        /// PNG loaders may retain the encoded GBytes as well as decoded pixel
        /// backing. Raw formats transfer/replace their source Vec, so only PNG
        /// needs this additional retained charge.
        encoded_source_backing_bytes: usize,
    },
    /// Buffered but not for display (`a=t`). Future `a=p` is unsupported, so
    /// these are effectively no-ops; returned distinctly only so the caller
    /// can avoid attaching them to the current block.
    CompleteTransmitOnly,
    Pending,
    Skipped,
    Invalid,
    /// `a=q` support probe passed validation. Never displayed or stored; the
    /// caller only owes the client an `OK` reply (see [`response_for`]).
    QueryOk,
}

/// Stateful assembler — a thin GTK-side wrapper over the shared one.
pub(crate) struct Assembler {
    inner: core::Assembler,
}

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Assembler {
    pub(crate) fn new() -> Self {
        Self {
            inner: core::Assembler::new(CAPS),
        }
    }

    /// Drop all in-flight state — call when a block ends or the shell resets,
    /// so a half-uploaded image doesn't leak across commands.
    pub(crate) fn reset(&mut self) {
        self.inner.reset();
    }

    /// Parse one APC G payload. `payload` is the bytes between `\e_` and the
    /// terminating `\e\\` (i.e. starts with `G`). Returns the outcome the
    /// caller should act on.
    pub(crate) fn feed(&mut self, payload: &[u8]) -> Outcome {
        let step = match self.inner.feed(payload) {
            Ok(step) => step,
            Err(error) => return outcome_for(error),
        };
        match step {
            Step::NotOurs => Outcome::Invalid,
            Step::NeedMore => Outcome::Pending,
            Step::Ready(assembled) => complete(assembled),
            Step::Other {
                command,
                interrupted,
            } => {
                if interrupted {
                    log::debug!(
                        "kitty graphics: a={} dropped an in-flight chunked upload",
                        command.get("a").unwrap_or("?")
                    );
                }
                if command.action == Action::Query {
                    query_outcome(&command)
                } else {
                    // Silently drop unsupported actions; the caller consumed
                    // the APC bytes already so libvte never sees them as
                    // garbage. The client still gets an ENOTSUP reply.
                    Outcome::Skipped
                }
            }
        }
    }
}

/// Map the shared parser's typed failure onto this app's wire codes:
/// [`Outcome::Invalid`] answers `EINVAL`, [`Outcome::Skipped`] answers
/// `ENOTSUP` — including size failures, which this responder reports as
/// "not supported" rather than a per-cause `ENOSPC`.
fn outcome_for(error: Error) -> Outcome {
    match error {
        Error::Invalid(_) => {
            log::debug!("kitty graphics: {error}");
            Outcome::Invalid
        }
        Error::TooLarge => {
            log::warn!("kitty graphics: dropping oversize image ({error})");
            Outcome::Skipped
        }
        Error::NotSupported(_) => {
            log::debug!("kitty graphics: {error}");
            Outcome::Skipped
        }
    }
}

/// Decode a finished transfer into a texture, honouring `a=t` vs `a=T`.
fn complete(assembled: Assembled) -> Outcome {
    let display = assembled.display;
    let encoded_source_backing_bytes = if assembled.format == Format::Png {
        assembled.bytes.len()
    } else {
        0
    };
    let texture = match texture_for(assembled) {
        Ok(texture) => texture,
        Err(error) => return outcome_for(error),
    };
    if display {
        Outcome::Complete {
            texture,
            encoded_source_backing_bytes,
        }
    } else {
        // Drop the texture — we don't currently honour `a=p` placement.
        drop(texture);
        Outcome::CompleteTransmitOnly
    }
}

fn texture_for(assembled: Assembled) -> Result<gdk::Texture, Error> {
    if assembled.format == Format::Png {
        // The shared assembler already checked the IHDR geometry against CAPS,
        // so a tiny payload cannot make the GdkPixbuf loader allocate a huge
        // canvas.
        let bytes = glib::Bytes::from_owned(assembled.bytes);
        return gdk::Texture::from_bytes(&bytes).map_err(|_| Error::Invalid("PNG data"));
    }
    let (rgba, width, height) = assembled.into_rgba8()?;
    memory_texture(rgba, width, height)
}

/// Upload RGBA pixels as a GDK texture. `f=24` was already expanded to RGBA
/// with full opacity by the shared decoder.
fn memory_texture(rgba: Vec<u8>, width: u32, height: u32) -> Result<gdk::Texture, Error> {
    let stride = (width as usize).checked_mul(4).ok_or(Error::TooLarge)?;
    let width = i32::try_from(width).map_err(|_| Error::TooLarge)?;
    let height = i32::try_from(height).map_err(|_| Error::TooLarge)?;
    let bytes = glib::Bytes::from_owned(rgba);
    Ok(
        gdk::MemoryTexture::new(width, height, gdk::MemoryFormat::R8g8b8a8, &bytes, stride)
            .upcast(),
    )
}

/// Validate an `a=q` support probe. `kitten icat` (and other well-behaved
/// clients) transmit a tiny sample image with `a=q` and block until the
/// terminal answers, so probes must be validated — not silently skipped like
/// anvil does. Nothing is buffered or displayed; chunking (`m=`) is ignored
/// because known clients probe in one APC.
fn query_outcome(command: &core::Command<'_>) -> Outcome {
    if let Err(error) = command.require_direct_transport() {
        return outcome_for(error);
    }
    let encoded = command.payload_b64.as_bytes();
    let significant = encoded.iter().filter(|b| !b.is_ascii_whitespace()).count();
    if significant > CAPS.max_encoded_bytes {
        return outcome_for(Error::TooLarge);
    }
    if significant == 0 {
        return Outcome::Invalid;
    }
    let decoded = match core::decode_base64(encoded, CAPS.max_decoded_bytes) {
        Ok(decoded) => decoded,
        Err(error) => return outcome_for(error),
    };
    let checked = match command.format {
        Format::Png => core::png_dimensions(&decoded, &CAPS).map(|_| ()),
        format => {
            let Some((width, height)) = command.declared() else {
                return Outcome::Invalid;
            };
            core::raw_layout(width, height, format, &CAPS).and_then(|layout| {
                if decoded.len() == layout.source_bytes {
                    Ok(())
                } else {
                    Err(Error::Invalid("raw image length does not match s= and v="))
                }
            })
        }
    };
    match checked {
        Ok(()) => Outcome::QueryOk,
        Err(error) => outcome_for(error),
    }
}

/// The identifiers a reply echoes back, scanned leniently.
///
/// Deliberately not read through [`core::parse_command`]: the commands that
/// most need an answer are the ones the shared parser *rejected*, and a client
/// blocked on its `i=` correlation key still owes to hear `EINVAL`.
struct ReplyKeys {
    id: Option<u32>,
    number: Option<u32>,
    placement: Option<u32>,
    quiet: u8,
}

impl ReplyKeys {
    /// `None` when the payload is not a graphics command, is not UTF-8, or
    /// carries more control data than the shared parser would have read.
    fn scan(payload: &[u8]) -> Option<Self> {
        let rest = payload.strip_prefix(b"G")?;
        // Split once at `;`: base64 padding belongs to the data section and
        // must never be read back as another control pair.
        let control = match memchr::memchr(b';', rest) {
            Some(index) => &rest[..index],
            None => rest,
        };
        if control.len() > CAPS.max_control_bytes {
            return None;
        }
        let control = std::str::from_utf8(control).ok()?;
        let mut keys = Self {
            id: None,
            number: None,
            placement: None,
            quiet: 0,
        };
        for (key, value) in control.split(',').filter_map(|pair| pair.split_once('=')) {
            // Last write wins, matching how the shared parser resolves
            // duplicate keys.
            match key {
                "i" => keys.id = value.parse().ok(),
                "I" => keys.number = value.parse().ok(),
                "p" => keys.placement = value.parse().ok(),
                "q" => keys.quiet = value.parse().ok().filter(|q| *q <= 2).unwrap_or(0),
                _ => {}
            }
        }
        Some(keys)
    }
}

/// Build the PTY reply owed for a processed APC G payload, or `None` when the
/// protocol expects silence. Reply semantics follow ember, the family's most
/// complete responder:
/// - only commands carrying an `i=`/`I=` identifier are answered (the id is
///   the client's correlation key; kitty itself stays silent without one);
/// - `q=1` suppresses `OK`, `q=2` also suppresses errors;
/// - a non-zero `p=` placement id is echoed back.
///
/// Deliberate divergences from ember, kept small because this responder sits
/// on top of the shared structural assembler rather than a full placement
/// table:
/// - every unsupported-but-well-formed command answers `ENOTSUP` instead of
///   per-cause `ENOENT`/`ENOSPC` codes;
/// - a chunked upload rejected at any chunk keeps no tombstone for the aborted
///   id, so its remaining chunks are each rejected as "continuation without a
///   transfer in progress" — silently, since a legal continuation carries no
///   `i=` to answer;
/// - `q=` is read per-chunk from the payload being answered, not remembered
///   across an upload the way the shared assembler remembers it.
pub(crate) fn response_for(payload: &[u8], outcome: &Outcome) -> Option<Vec<u8>> {
    let keys = ReplyKeys::scan(payload)?;
    if keys.id.is_none() && keys.number.is_none() {
        return None;
    }
    let body = match outcome {
        // Chunked uploads are answered once, after the final chunk.
        Outcome::Pending => return None,
        Outcome::Complete { .. } | Outcome::CompleteTransmitOnly | Outcome::QueryOk => {
            if keys.quiet >= 1 {
                return None;
            }
            "OK"
        }
        Outcome::Invalid => {
            if keys.quiet >= 2 {
                return None;
            }
            "EINVAL:invalid graphics payload"
        }
        Outcome::Skipped => {
            if keys.quiet >= 2 {
                return None;
            }
            "ENOTSUP:action, format, transport, or size not supported"
        }
    };
    let mut fields = Vec::with_capacity(3);
    if let Some(id) = keys.id {
        fields.push(format!("i={id}"));
    }
    if let Some(number) = keys.number {
        fields.push(format!("I={number}"));
    }
    if let Some(placement) = keys.placement.filter(|p| *p != 0) {
        fields.push(format!("p={placement}"));
    }
    Some(format!("\x1b_G{};{body}\x1b\\", fields.join(",")).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_base64(input: &[u8]) -> Vec<u8> {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let a = chunk[0];
            let b = chunk.get(1).copied().unwrap_or(0);
            let c = chunk.get(2).copied().unwrap_or(0);
            out.push(TABLE[(a >> 2) as usize]);
            out.push(TABLE[(((a & 0x03) << 4) | (b >> 4)) as usize]);
            out.push(if chunk.len() > 1 {
                TABLE[(((b & 0x0f) << 2) | (c >> 6)) as usize]
            } else {
                b'='
            });
            out.push(if chunk.len() > 2 {
                TABLE[(c & 0x3f) as usize]
            } else {
                b'='
            });
        }
        out
    }

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    fn feed(payload: &[u8]) -> Outcome {
        Assembler::new().feed(payload)
    }

    // ---- shared caps -----------------------------------------------------

    #[test]
    fn the_block_budget_is_the_shared_block_preset() {
        assert_eq!(MAX_PENDING_BYTES_PER_BLOCK, 16 * 1024 * 1024);
        assert_eq!(CAPS, Caps::BLOCK);
        assert_eq!(CAPS.max_dimension, 16_384);
    }

    #[test]
    fn tiny_images_are_bounded_by_object_count_and_fixed_cost() {
        let one = pending_image_bytes_after_admission(0, 0, 4, 0).unwrap();
        assert_eq!(one, RETAINED_BYTES_PER_IMAGE_OBJECT + 4);
        assert!(pending_image_bytes_after_admission(one, MAX_IMAGES_PER_BLOCK, 4, 0).is_none());
    }

    #[test]
    fn image_admission_rejects_byte_overflow_and_budget_overrun() {
        assert!(pending_image_bytes_after_admission(usize::MAX, 0, 1, 0).is_none());
        assert!(pending_image_bytes_after_admission(
            MAX_PENDING_BYTES_PER_BLOCK - RETAINED_BYTES_PER_IMAGE_OBJECT,
            0,
            1,
            0
        )
        .is_none());
    }

    #[test]
    fn png_encoded_backing_is_part_of_the_retained_budget() {
        let exact_backing = MAX_PENDING_BYTES_PER_BLOCK - RETAINED_BYTES_PER_IMAGE_OBJECT - 4;
        assert_eq!(
            pending_image_bytes_after_admission(0, 0, 4, exact_backing),
            Some(MAX_PENDING_BYTES_PER_BLOCK)
        );
        assert!(pending_image_bytes_after_admission(0, 0, 4, exact_backing + 1).is_none());
    }

    // ---- outcomes --------------------------------------------------------

    #[test]
    fn rejects_non_g_payload() {
        assert!(matches!(feed(b""), Outcome::Invalid));
        assert!(matches!(feed(b"X"), Outcome::Invalid));
    }

    #[test]
    fn unsupported_action_is_skipped() {
        assert!(matches!(feed(b"Ga=d,i=1;"), Outcome::Skipped));
        assert!(matches!(feed(b"Ga=p,i=1;"), Outcome::Skipped));
    }

    #[test]
    fn file_transport_is_skipped() {
        assert!(matches!(
            feed(b"Ga=T,f=100,t=f;L3RtcC9hLnBuZw=="),
            Outcome::Skipped
        ));
        // t=t and t=s are recognised and refused the same way.
        assert!(matches!(feed(b"Ga=T,f=100,t=t,i=1;AAAA"), Outcome::Skipped));
        assert!(matches!(feed(b"Ga=T,f=100,t=s,i=1;AAAA"), Outcome::Skipped));
    }

    #[test]
    fn chunked_assembly_accumulates_then_decodes_rgb_pixel() {
        let mut a = Assembler::new();
        // 1×1 red pixel as raw RGB (f=24): bytes 0xFF 0x00 0x00 -> "/wAA"
        // Split across two chunks via m=1 / m=0.
        let first = a.feed(b"Ga=T,f=24,s=1,v=1,i=7,m=1;/w");
        assert!(matches!(first, Outcome::Pending));
        // Continuations carry only m= (and optionally q=): repeating the
        // metadata would start a second upload for i=7.
        let second = a.feed(b"Gm=0;AA");
        match second {
            Outcome::Complete { .. } => {}
            _ => panic!("expected complete texture"),
        }
    }

    #[test]
    fn a_continuation_that_repeats_metadata_is_invalid() {
        // Standardized by the shared parser: the protocol sends metadata on the
        // first chunk only, so a second one is a client that lost track of its
        // own transfer.
        let mut a = Assembler::new();
        assert!(matches!(
            a.feed(b"Ga=T,f=24,s=1,v=1,i=7,m=1;/w"),
            Outcome::Pending
        ));
        assert!(matches!(a.feed(b"Ga=T,i=7,m=0;AA"), Outcome::Invalid));
        // The abort clears the slot, so an honest retry still works.
        assert!(matches!(
            a.feed(b"Ga=T,f=24,s=1,v=1,i=7,m=1;/w"),
            Outcome::Pending
        ));
        assert!(matches!(a.feed(b"Gm=0;AA"), Outcome::Complete { .. }));
    }

    #[test]
    fn transmit_only_uploads_are_reported_separately() {
        assert!(matches!(
            feed(b"Ga=t,f=32,s=1,v=1,i=4;AQIDBA=="),
            Outcome::CompleteTransmitOnly
        ));
        assert!(matches!(
            feed(b"Ga=T,f=32,s=1,v=1,i=4;AQIDBA=="),
            Outcome::Complete { .. }
        ));
    }

    #[test]
    fn reset_drops_in_flight_uploads() {
        let mut a = Assembler::new();
        assert!(matches!(
            a.feed(b"Ga=T,f=32,s=1,v=1,i=9,m=1;AQID"),
            Outcome::Pending
        ));
        a.reset();
        // Nothing is in flight, so the continuation has nowhere to land.
        assert!(matches!(a.feed(b"Gm=0;BA=="), Outcome::Invalid));
    }

    // ---- standardizations adopted from the shared module -----------------

    #[test]
    fn format_now_defaults_to_rgba_not_png() {
        // Pre-hoist forge defaulted f= to PNG; the protocol default is RGBA,
        // so an f=-less command now means "raw RGBA, s=/v= required".
        assert!(matches!(
            feed(b"Ga=T,i=5,s=1,v=1;AQIDBA=="),
            Outcome::Complete { .. }
        ));
        // …and without s=/v= it is a malformed raw transfer, not a PNG.
        assert!(matches!(feed(b"Ga=T,i=5;AQIDBA=="), Outcome::Invalid));
    }

    #[test]
    fn raw_payload_length_must_match_exactly() {
        // Trailing slack used to be accepted; it no longer is.
        assert!(matches!(
            feed(b"Ga=T,f=32,i=13,s=1,v=1;AQIDBAU="),
            Outcome::Invalid
        ));
        assert!(matches!(
            feed(b"Ga=T,f=32,i=13,s=1,v=1;AQID"),
            Outcome::Invalid
        ));
        assert!(matches!(
            feed(b"Ga=T,f=32,i=13,s=1,v=1;AQIDBA=="),
            Outcome::Complete { .. }
        ));
    }

    #[test]
    fn non_standard_format_aliases_are_not_supported() {
        for alias in ["png", "jpeg", "rgba", "0"] {
            let payload = format!("Ga=T,i=1,f={alias},s=1,v=1;AQIDBA==");
            assert!(
                matches!(feed(payload.as_bytes()), Outcome::Skipped),
                "{alias}"
            );
        }
    }

    #[test]
    fn id_and_number_are_mutually_exclusive() {
        assert!(matches!(
            feed(b"Ga=T,f=32,i=1,I=2,s=1,v=1;AQIDBA=="),
            Outcome::Invalid
        ));
    }

    #[test]
    fn base64_rejects_garbage_interior_padding_and_impossible_lengths() {
        assert!(matches!(
            feed(b"Ga=T,f=32,i=1,s=1,v=1;!!!!"),
            Outcome::Invalid
        ));
        // Interior padding.
        assert!(matches!(
            feed(b"Ga=T,f=32,i=1,s=1,v=1;AQ=IDBA=="),
            Outcome::Invalid
        ));
        // len % 4 == 1 is not a possible base64 encoding.
        assert!(matches!(
            feed(b"Ga=T,f=32,i=1,s=1,v=1;AQIDB"),
            Outcome::Invalid
        ));
    }

    #[test]
    fn embedded_whitespace_is_stripped_across_chunks() {
        let mut a = Assembler::new();
        assert!(matches!(
            a.feed(b"Ga=T,f=32,i=14,s=1,v=1,m=1;AQ\r\n"),
            Outcome::Pending
        ));
        assert!(matches!(
            a.feed(b"Gm=0; ID  BA== \n"),
            Outcome::Complete { .. }
        ));
    }

    // ---- caps ------------------------------------------------------------

    #[test]
    fn oversized_raw_dimensions_are_skipped_without_overflow() {
        let cases: &[&[u8]] = &[
            b"Ga=T,f=32,s=4294967295,v=4294967295;AAAA",
            b"Ga=T,f=24,s=16384,v=16384;AAAA",
            b"Ga=T,f=32,s=16385,v=1;AAAA",
        ];
        for payload in cases {
            assert!(matches!(feed(payload), Outcome::Skipped));
        }
    }

    #[test]
    fn zero_dimensions_are_invalid_not_oversize() {
        assert!(matches!(
            feed(b"Ga=T,f=32,i=1,s=0,v=1;AAAA"),
            Outcome::Invalid
        ));
    }

    #[test]
    fn oversized_png_ihdr_is_skipped_before_gdk_decode() {
        let encoded = encode_base64(&png_header(CAPS.max_dimension + 1, 1));
        let mut payload = b"Ga=T,f=100;".to_vec();
        payload.extend_from_slice(&encoded);
        assert!(matches!(feed(&payload), Outcome::Skipped));
    }

    #[test]
    fn f100_that_is_not_a_png_is_invalid() {
        let mut payload = b"Ga=T,f=100,i=2;".to_vec();
        payload.extend_from_slice(&encode_base64(b"not a PNG at all"));
        assert!(matches!(feed(&payload), Outcome::Invalid));
    }

    // ---- a=q support probe -----------------------------------------------

    #[test]
    fn query_probe_validates_raw_pixels() {
        // kitten icat's support probe: 1×1 RGB sample under a=q.
        let mut a = Assembler::new();
        let payload = b"Ga=q,i=31,s=1,v=1,f=24,t=d;AAAA";
        let outcome = a.feed(payload);
        assert!(matches!(outcome, Outcome::QueryOk));
        assert_eq!(
            response_for(payload, &outcome).as_deref(),
            Some(b"\x1b_Gi=31;OK\x1b\\".as_slice())
        );
    }

    #[test]
    fn query_probe_validates_a_png_sample() {
        let mut payload = b"Ga=q,i=32,f=100;".to_vec();
        payload.extend_from_slice(&encode_base64(&png_header(1, 1)));
        assert!(matches!(feed(&payload), Outcome::QueryOk));
    }

    #[test]
    fn query_probe_rejects_bad_or_oversize_payloads() {
        // Undecodable body.
        assert!(matches!(
            feed(b"Ga=q,i=1,s=1,v=1,f=24;!!!!"),
            Outcome::Invalid
        ));
        // Payload that does not match the advertised dimensions.
        assert!(matches!(
            feed(b"Ga=q,i=1,s=2,v=2,f=24;AAAA"),
            Outcome::Invalid
        ));
        // Empty body.
        assert!(matches!(feed(b"Ga=q,i=1,s=1,v=1,f=24;"), Outcome::Invalid));
        // Dimensions beyond the family cap.
        assert!(matches!(
            feed(b"Ga=q,i=1,s=16385,v=1,f=32;AAAA"),
            Outcome::Skipped
        ));
        // Non-direct transports are refused before anything is decoded.
        assert!(matches!(
            feed(b"Ga=q,i=1,s=1,v=1,f=24,t=f;AAAA"),
            Outcome::Skipped
        ));
    }

    #[test]
    fn a_probe_never_buffers_anything() {
        let mut a = Assembler::new();
        assert!(matches!(
            a.feed(b"Ga=q,i=31,s=1,v=1,f=24;AAAA"),
            Outcome::QueryOk
        ));
        // Nothing was stored, so a bare continuation has nowhere to land.
        assert!(matches!(a.feed(b"Gm=0;AAAA"), Outcome::Invalid));
    }

    // ---- responder -------------------------------------------------------

    #[test]
    fn responses_require_an_identifier() {
        assert_eq!(
            response_for(b"Ga=t,f=24,s=1,v=1;AAAA", &Outcome::Invalid),
            None
        );
        assert_eq!(
            response_for(b"Ga=T;AAAA", &Outcome::CompleteTransmitOnly),
            None
        );
        assert_eq!(response_for(b"not graphics", &Outcome::Invalid), None);
    }

    #[test]
    fn responses_echo_ids_and_map_outcomes() {
        assert_eq!(
            response_for(
                b"Ga=t,i=41,s=1,v=1,f=24;AAAA",
                &Outcome::CompleteTransmitOnly
            )
            .as_deref(),
            Some(b"\x1b_Gi=41;OK\x1b\\".as_slice())
        );
        assert_eq!(
            response_for(b"GI=13,a=T;AAAA", &Outcome::Invalid).as_deref(),
            Some(b"\x1b_GI=13;EINVAL:invalid graphics payload\x1b\\".as_slice())
        );
        assert_eq!(
            response_for(b"Ga=d,i=5,p=17;", &Outcome::Skipped).as_deref(),
            Some(
                b"\x1b_Gi=5,p=17;ENOTSUP:action, format, transport, or size not supported\x1b\\"
                    .as_slice()
            )
        );
    }

    #[test]
    fn responses_answer_commands_the_shared_parser_rejected() {
        // The reply keys are scanned leniently on purpose: a client blocked on
        // its i= must hear EINVAL even when the command never parsed.
        let payload = b"Ga=T,i=1,I=2,s=1,v=1,f=32;AQIDBA==";
        let outcome = feed(payload);
        assert!(matches!(outcome, Outcome::Invalid));
        assert_eq!(
            response_for(payload, &outcome).as_deref(),
            Some(b"\x1b_Gi=1,I=2;EINVAL:invalid graphics payload\x1b\\".as_slice())
        );
    }

    #[test]
    fn responses_ignore_base64_padding_in_the_data_section() {
        // Splitting once at ';' keeps `=` padding out of the control scan.
        assert_eq!(
            response_for(b"Ga=T,i=8;AQIDBA==", &Outcome::Invalid).as_deref(),
            Some(b"\x1b_Gi=8;EINVAL:invalid graphics payload\x1b\\".as_slice())
        );
    }

    #[test]
    fn responses_wait_for_the_final_chunk() {
        assert_eq!(response_for(b"Ga=T,i=7,m=1;/w", &Outcome::Pending), None);
    }

    #[test]
    fn quiet_levels_suppress_responses() {
        assert_eq!(
            response_for(b"Ga=q,i=31,q=1,s=1,v=1,f=24;AAAA", &Outcome::QueryOk),
            None
        );
        // q=1 still reports errors …
        assert!(response_for(b"Ga=T,i=2,q=1;!!!!", &Outcome::Invalid).is_some());
        // … q=2 silences those too.
        assert_eq!(response_for(b"Ga=T,i=2,q=2;!!!!", &Outcome::Invalid), None);
        assert_eq!(response_for(b"Ga=d,i=3,q=2;", &Outcome::Skipped), None);
    }
}
