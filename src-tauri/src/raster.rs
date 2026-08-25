//! Raster (image) printing — the Arabic escape hatch for cheap ESC/POS clones.
//!
//! `print.rs` sends CHARACTERS: the page hands over text, the shell encodes it
//! to windows-1256 and the printer's firmware is expected to SHAPE the Arabic
//! (join the letters, run the line right-to-left). Better hardware does. The
//! Xprinter/Rongta clones that fill Egyptian and Gulf counters do not — they
//! map cp1256 byte by byte, so "مطعم" prints as four disconnected isolated
//! letters in Latin order. No amount of codepage fiddling fixes that: the
//! shaping table simply isn't in the ROM.
//!
//! So we stop asking the printer to draw Arabic and draw it ourselves. The web
//! side lays the SAME receipt ops out on a hidden canvas at the printer's dot
//! width (58 mm = 384 dots, 80 mm = 576) using the browser's own HarfBuzz-class
//! shaping, thresholds the result to 1 bit per pixel and ships the packed rows
//! here. This module only wraps them in `GS v 0` and pushes them down the
//! existing wire — same three transports, same cut, same drawer.
//!
//! Two invariants worth keeping:
//!
//!  * **Bands, not one giant blob.** Many clones have a small line buffer and
//!    either truncate or reset on an oversized `GS v 0`. Splitting into
//!    `BAND_ROWS`-tall slices is the portable form, and it also lets a long
//!    receipt start printing while the rest is still on the wire.
//!  * **The page never picks the transport.** `target` names WHICH printer
//!    ("receipt", or a kitchen station id) and the stored config answers the
//!    rest — exactly like `shell_pos_print` / `shell_kot_print`.

use base64::Engine;
use tauri::AppHandle;

use crate::print::{send_bytes, PrinterConfig};

/// Rows per `GS v 0` band. 24 is the classic safe slice (three 8-dot bands)
/// that every clone's line buffer swallows.
const BAND_ROWS: u32 = 24;

/// The widest roll we print (80 mm at 203 dpi is 576 dots; leave headroom for
/// a 112 mm printer rather than reject it outright).
const MAX_WIDTH_DOTS: u32 = 1024;

/// ~2.5 m of paper at 203 dpi. A runaway canvas must not spool the roll.
const MAX_HEIGHT_DOTS: u32 = 20_000;

/// The receipt printer's key — anything else is a kitchen station id.
pub(crate) const RECEIPT_TARGET: &str = "receipt";

// ---------------------------------------------------------------------------
// Byte protocol
// ---------------------------------------------------------------------------

/// The `[start_row, rows)` slices a bitmap of `height` rows is sent in.
///
/// Pure and `pub(crate)` so it can be unit-tested without a printer: the band
/// arithmetic is the one place an off-by-one silently eats the last few dot
/// rows of every receipt (which reads as "the total is missing").
pub(crate) fn split_bands(height: u32, band_rows: u32) -> Vec<(u32, u32)> {
    if height == 0 || band_rows == 0 {
        return Vec::new();
    }

    let mut bands = Vec::with_capacity((height / band_rows + 1) as usize);
    let mut row = 0;
    while row < height {
        let rows = band_rows.min(height - row);
        bands.push((row, rows));
        row += rows;
    }
    bands
}

/// Packed 1-bit rows → ESC/POS. `data` is row-major, MSB first, **1 = black**
/// (the ESC/POS convention), stride `ceil(width / 8)` bytes per row.
pub(crate) fn encode_raster(
    width_dots: u32,
    height_dots: u32,
    data: &[u8],
    cut: bool,
    drawer: bool,
) -> Result<Vec<u8>, String> {
    if width_dots == 0 || height_dots == 0 {
        return Err("raster bitmap is empty".into());
    }
    if width_dots > MAX_WIDTH_DOTS {
        return Err(format!("raster is too wide ({width_dots} dots)"));
    }
    if height_dots > MAX_HEIGHT_DOTS {
        return Err(format!("raster is too tall ({height_dots} dots)"));
    }

    let stride = width_dots.div_ceil(8) as usize;
    let expected = stride * height_dots as usize;
    if data.len() != expected {
        return Err(format!(
            "raster payload is {} bytes, expected {expected} ({stride} x {height_dots})",
            data.len()
        ));
    }

    let mut out: Vec<u8> = Vec::with_capacity(data.len() + 64);
    out.extend_from_slice(&[0x1B, 0x40]); // ESC @  init
    out.extend_from_slice(&[0x1B, 0x61, 0]); // ESC a  left — the bitmap owns its own margins

    let xl = (stride & 0xFF) as u8;
    let xh = ((stride >> 8) & 0xFF) as u8;

    for (start, rows) in split_bands(height_dots, BAND_ROWS) {
        // GS v 0 m xL xH yL yH — m = 0: normal density, one dot per bit.
        out.extend_from_slice(&[
            0x1D,
            0x76,
            0x30,
            0,
            xl,
            xh,
            (rows & 0xFF) as u8,
            ((rows >> 8) & 0xFF) as u8,
        ]);
        let from = start as usize * stride;
        out.extend_from_slice(&data[from..from + rows as usize * stride]);
    }

    if cut {
        out.extend_from_slice(&[0x1B, 0x64, 4]); // feed clear of the blade
        out.extend_from_slice(&[0x1D, 0x56, 0x42, 0]); // GS V  partial cut
    }
    if drawer {
        out.extend_from_slice(&[0x1B, 0x70, 0, 0x19, 0xFA]); // ESC p
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// `"receipt"` → the till's printer, anything else → that kitchen station's
/// (falling back to the `"default"` station, exactly as a KOT does).
fn resolve_target(app: &AppHandle, target: &str) -> Result<PrinterConfig, String> {
    if target == RECEIPT_TARGET {
        return crate::print::load_config(app).ok_or_else(|| "no printer configured".to_string());
    }

    let station = crate::kitchen::normalize_station(target)?;
    crate::kitchen::resolve_config(&crate::kitchen::load_map(app), &station)
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

/// Prints a pre-rendered monochrome bitmap.
///
/// `data` is base64 (the packed rows described on `encode_raster`) rather than
/// a `Vec<u8>`: an 80 mm receipt is ~40 KB of bits, and a JSON array of 40 000
/// numbers costs an order of magnitude more to serialize across the IPC bridge
/// than the ~55 KB base64 string.
///
/// `drawer` is a REQUEST, honored only when the resolved printer's config
/// enables the kick — same gate as `ReceiptOp::Drawer`, so a kitchen station
/// can never pop the till.
#[tauri::command]
pub async fn shell_print_raster(
    app: AppHandle,
    target: String,
    width: u32,
    height: u32,
    data: String,
    cut: bool,
    drawer: bool,
) -> Result<(), String> {
    let config = resolve_target(&app, target.trim())?;
    config.validate()?;

    let bitmap = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|e| format!("raster payload is not valid base64: {e}"))?;

    let bytes = encode_raster(width, height, &bitmap, cut, drawer && config.drawer_kick)?;

    // Blocking socket/serial/spooler I/O has no business on the async core.
    tauri::async_runtime::spawn_blocking(move || {
        send_bytes(
            &config.interface,
            &config.address,
            config.baud,
            &bytes,
            "Orcaa receipt",
        )
    })
    .await
    .map_err(|e| format!("raster print task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_cover_every_row_exactly_once() {
        let bands = split_bands(100, 24);
        assert_eq!(bands.len(), 5);
        assert_eq!(bands[0], (0, 24));
        assert_eq!(bands[3], (72, 24));
        // The remainder band is short, never padded — padding would print a
        // strip of blank dots between every receipt's last line and the cut.
        assert_eq!(bands[4], (96, 4));
        assert_eq!(bands.iter().map(|(_, rows)| rows).sum::<u32>(), 100);
    }

    #[test]
    fn an_exact_multiple_has_no_short_band() {
        let bands = split_bands(48, 24);
        assert_eq!(bands, vec![(0, 24), (24, 24)]);
    }

    #[test]
    fn nothing_to_print_is_no_bands() {
        assert!(split_bands(0, 24).is_empty());
        assert!(split_bands(100, 0).is_empty());
    }

    #[test]
    fn a_raster_carries_one_gs_v_0_header_per_band() {
        // 8 dots wide (1 byte/row) x 30 rows -> two bands (24 + 6).
        let data = vec![0xFFu8; 30];
        let bytes = encode_raster(8, 30, &data, false, false).unwrap();

        let headers = bytes
            .windows(3)
            .filter(|w| *w == [0x1D, 0x76, 0x30])
            .count();
        assert_eq!(headers, 2);

        // ESC @ + ESC a 0 + two headers (8 bytes each) + the pixel rows.
        assert_eq!(bytes.len(), 2 + 3 + 8 * 2 + 30);
    }

    #[test]
    fn the_band_header_states_the_row_stride_and_row_count() {
        let data = vec![0u8; 2 * 10]; // 16 dots wide, 10 rows
        let bytes = encode_raster(16, 10, &data, false, false).unwrap();
        let at = bytes
            .windows(3)
            .position(|w| w == [0x1D, 0x76, 0x30])
            .unwrap();

        assert_eq!(bytes[at + 3], 0, "m = 0, normal density");
        assert_eq!((bytes[at + 4], bytes[at + 5]), (2, 0), "xL/xH = bytes per row");
        assert_eq!((bytes[at + 6], bytes[at + 7]), (10, 0), "yL/yH = rows");
    }

    #[test]
    fn a_payload_that_does_not_match_its_dimensions_is_refused() {
        assert!(encode_raster(384, 10, &[0u8; 100], false, false).is_err());
        assert!(encode_raster(0, 10, &[], false, false).is_err());
        assert!(encode_raster(384, 0, &[], false, false).is_err());
        assert!(encode_raster(4096, 10, &[0u8; 5120], false, false).is_err());
    }

    #[test]
    fn cut_and_drawer_ride_after_the_bitmap() {
        let data = vec![0u8; 8];
        let quiet = encode_raster(8, 8, &data, false, false).unwrap();
        assert!(!quiet.windows(2).any(|w| w == [0x1D, 0x56]));
        assert!(!quiet.windows(2).any(|w| w == [0x1B, 0x70]));

        let loud = encode_raster(8, 8, &data, true, true).unwrap();
        assert!(loud.windows(4).any(|w| w == [0x1D, 0x56, 0x42, 0]));
        assert!(loud.windows(2).any(|w| w == [0x1B, 0x70]));
    }
}
