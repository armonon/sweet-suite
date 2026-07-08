//! Clean-room TrueType (`.ttf`, `glyf`-outline only) parser + glyph rasterizer, for M4a's
//! Text tool. Hand-written against the OpenType spec's table layouts — no font-parsing crate
//! (`ttf-parser`, `rustybuzz`, `fontdue`, etc.) per the project's clean-room-over-vendoring
//! policy for product-differentiating logic. Font **files** themselves are never bundled or
//! committed (most system fonts, including the one this was tested against, are proprietary
//! and not redistributable) — only this parsing code is original and owned.
//!
//! v1 scope, deliberately: TrueType `glyf` outlines only (no OTF/CFF PostScript outlines);
//! simple glyphs only (no composite/compound glyphs — most of Latin/digits/punctuation in
//! mainstream fonts are simple glyphs, so this covers the ASCII text v1 targets; accented
//! composites like "é" will render blank, a documented gap); `cmap` format 4 only (covers the
//! Unicode Basic Multilingual Plane, i.e. all of ASCII/Latin-1 — format 12's extra reach into
//! astral planes isn't needed for this scope); no kerning/GPOS, no hinting (hinting
//! instructions are parsed-past, never executed — this only affects small-size crispness).

/// Big-endian field reads — every multi-byte TrueType field is big-endian ("network" order).
fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([d[o], d[o + 1]])
}
fn i16_at(d: &[u8], o: usize) -> i16 {
    i16::from_be_bytes([d[o], d[o + 1]])
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

#[derive(Debug, Clone, PartialEq)]
pub enum FontError {
    TooShort,
    NotTrueType,
    MissingTable(&'static str),
    UnsupportedCmap,
    Malformed(&'static str),
}

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FontError::TooShort => write!(f, "file is too short to be a font"),
            FontError::NotTrueType => write!(f, "not a TrueType font (expected sfnt version 0x00010000) — OTF/CFF fonts aren't supported yet"),
            FontError::MissingTable(t) => write!(f, "font is missing the required '{t}' table"),
            FontError::UnsupportedCmap => write!(f, "font has no Unicode cmap subtable in a supported format (need format 4)"),
            FontError::Malformed(what) => write!(f, "malformed font data: {what}"),
        }
    }
}

/// A parsed TrueType font, holding just enough to look up glyphs by character, get their
/// advance width, and extract their outline — everything needed for single-line layout +
/// rasterization. Keeps the whole file in memory (`data`) since `glyf`/`loca`/`cmap` are all
/// referenced lazily by byte offset rather than eagerly unpacked into owned structures.
pub struct Font {
    data: Vec<u8>,
    units_per_em: u16,
    num_glyphs: u16,
    loca_long: bool,
    loca_start: usize,
    glyf_start: usize,
    hmtx_start: usize,
    num_h_metrics: u16,
    cmap_subtable_start: usize,
}

impl Font {
    /// Parse a `.ttf` file's bytes. Locates the table directory, then `head` (units-per-em +
    /// loca format), `maxp` (glyph count), `loca` + `glyf` (outlines), `hhea` + `hmtx`
    /// (advance widths), and a Unicode `cmap` format-4 subtable (character → glyph mapping).
    pub fn parse(data: Vec<u8>) -> Result<Font, FontError> {
        if data.len() < 12 {
            return Err(FontError::TooShort);
        }
        if u32_at(&data, 0) != 0x0001_0000 {
            return Err(FontError::NotTrueType);
        }
        let num_tables = u16_at(&data, 4) as usize;
        if data.len() < 12 + num_tables * 16 {
            return Err(FontError::TooShort);
        }
        let mut tables: std::collections::HashMap<[u8; 4], (usize, usize)> = std::collections::HashMap::new();
        for i in 0..num_tables {
            let rec = 12 + i * 16;
            let tag = [data[rec], data[rec + 1], data[rec + 2], data[rec + 3]];
            let offset = u32_at(&data, rec + 8) as usize;
            let length = u32_at(&data, rec + 12) as usize;
            tables.insert(tag, (offset, length));
        }
        let find = |tag: &[u8; 4], name: &'static str| -> Result<(usize, usize), FontError> {
            tables.get(tag).copied().ok_or(FontError::MissingTable(name))
        };

        let (head_off, _) = find(b"head", "head")?;
        if head_off + 54 > data.len() {
            return Err(FontError::Malformed("head table truncated"));
        }
        let units_per_em = u16_at(&data, head_off + 18);
        let loca_long = i16_at(&data, head_off + 50) != 0;

        let (maxp_off, _) = find(b"maxp", "maxp")?;
        if maxp_off + 6 > data.len() {
            return Err(FontError::Malformed("maxp table truncated"));
        }
        let num_glyphs = u16_at(&data, maxp_off + 4);

        let (loca_start, _) = find(b"loca", "loca")?;
        let (glyf_start, _) = find(b"glyf", "glyf")?;

        let (hhea_off, _) = find(b"hhea", "hhea")?;
        if hhea_off + 36 > data.len() {
            return Err(FontError::Malformed("hhea table truncated"));
        }
        let num_h_metrics = u16_at(&data, hhea_off + 34);

        let (hmtx_start, _) = find(b"hmtx", "hmtx")?;

        let (cmap_off, _) = find(b"cmap", "cmap")?;
        let cmap_subtable_start = find_cmap_format4_subtable(&data, cmap_off)?;

        Ok(Font { data, units_per_em, num_glyphs, loca_long, loca_start, glyf_start, hmtx_start, num_h_metrics, cmap_subtable_start })
    }

    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Unicode character → glyph index, via the `cmap` format-4 subtable. Returns 0
    /// (`.notdef`, conventionally an empty/box glyph) for characters outside the font's BMP
    /// coverage or its format-4 segment table.
    pub fn glyph_id(&self, ch: char) -> u16 {
        let c = ch as u32;
        if c > 0xFFFF {
            return 0; // format 4 only covers the Basic Multilingual Plane.
        }
        cmap_format4_lookup(&self.data, self.cmap_subtable_start, c as u16)
    }

    /// A glyph's horizontal advance width, in font units (divide by `units_per_em` and
    /// multiply by point size to get layout pixels). `hmtx` only stores an explicit entry per
    /// glyph up to `numberOfHMetrics` — later glyphs (typically a monospace tail) reuse the
    /// last entry, per spec.
    pub fn advance_width(&self, glyph_id: u16) -> u16 {
        let i = (glyph_id as usize).min(self.num_h_metrics.saturating_sub(1) as usize);
        let off = self.hmtx_start + i * 4;
        if off + 2 > self.data.len() {
            return 0;
        }
        u16_at(&self.data, off)
    }

    /// A glyph's outline (font units, y-up per the TrueType convention) — empty for
    /// `.notdef`/space (no contours) and for composite glyphs (`numberOfContours < 0`,
    /// explicitly unsupported in v1; see module docs).
    pub fn glyph_outline(&self, glyph_id: u16) -> GlyphOutline {
        let empty = GlyphOutline { contours: Vec::new() };
        if glyph_id >= self.num_glyphs {
            return empty;
        }
        let (start, end) = match self.loca_range(glyph_id) {
            Some(r) => r,
            None => return empty,
        };
        if end <= start {
            return empty; // no outline (e.g. space) — a valid, common case, not an error.
        }
        let g = self.glyf_start + start;
        if g + 10 > self.data.len() {
            return empty;
        }
        let num_contours = i16_at(&self.data, g);
        if num_contours < 0 {
            return empty; // composite glyph — unsupported in v1.
        }
        parse_simple_glyph(&self.data, g, num_contours as usize).unwrap_or(empty)
    }

    fn loca_range(&self, glyph_id: u16) -> Option<(usize, usize)> {
        let i = glyph_id as usize;
        if self.loca_long {
            let a = self.loca_start + i * 4;
            let b = a + 4;
            if b + 4 > self.data.len() + 4 || a + 4 > self.data.len() || b + 4 > self.data.len() {
                return None;
            }
            Some((u32_at(&self.data, a) as usize, u32_at(&self.data, b) as usize))
        } else {
            let a = self.loca_start + i * 2;
            let b = a + 2;
            if b + 2 > self.data.len() {
                return None;
            }
            Some((u16_at(&self.data, a) as usize * 2, u16_at(&self.data, b) as usize * 2))
        }
    }
}

/// Find the byte offset (within `data`) of a Unicode `cmap` format-4 subtable. Scans the
/// encoding records for a Windows-Unicode-BMP (platform 3, encoding 1) or a Unicode-platform
/// (platform 0, any encoding) entry, per the common-practice lookup order most font tools use.
fn find_cmap_format4_subtable(data: &[u8], cmap_off: usize) -> Result<usize, FontError> {
    if cmap_off + 4 > data.len() {
        return Err(FontError::Malformed("cmap table truncated"));
    }
    let num_tables = u16_at(data, cmap_off + 2) as usize;
    let mut best: Option<usize> = None;
    for i in 0..num_tables {
        let rec = cmap_off + 4 + i * 8;
        if rec + 8 > data.len() {
            break;
        }
        let platform_id = u16_at(data, rec);
        let encoding_id = u16_at(data, rec + 2);
        let offset = cmap_off + u32_at(data, rec + 4) as usize;
        if offset + 2 > data.len() {
            continue;
        }
        let format = u16_at(data, offset);
        if format != 4 {
            continue;
        }
        let is_windows_bmp = platform_id == 3 && encoding_id == 1;
        let is_unicode = platform_id == 0;
        if is_windows_bmp {
            return Ok(offset); // best match — stop looking.
        }
        if is_unicode && best.is_none() {
            best = Some(offset);
        }
    }
    best.ok_or(FontError::UnsupportedCmap)
}

/// `cmap` format-4 character→glyph lookup. Format 4 stores the mapping as a sorted list of
/// contiguous code-point *segments*; the trailing `idRangeOffset` indirection is the one
/// famously fiddly part — a nonzero offset is a byte count *from the position of that same
/// `idRangeOffset` field* into a parallel `glyphIdArray`, not from the table start.
fn cmap_format4_lookup(data: &[u8], sub: usize, c: u16) -> u16 {
    if sub + 14 > data.len() {
        return 0;
    }
    let seg_count = (u16_at(data, sub + 6) / 2) as usize;
    let end_codes = sub + 14;
    let start_codes = end_codes + seg_count * 2 + 2; // +2 skips reservedPad
    let id_deltas = start_codes + seg_count * 2;
    let id_range_offsets = id_deltas + seg_count * 2;
    if id_range_offsets + seg_count * 2 > data.len() {
        return 0;
    }
    for seg in 0..seg_count {
        let end = u16_at(data, end_codes + seg * 2);
        if c > end {
            continue;
        }
        let start = u16_at(data, start_codes + seg * 2);
        if c < start {
            return 0; // segments are sorted ascending by end code — no later segment can match.
        }
        let id_delta = i16_at(data, id_deltas + seg * 2);
        let range_offset_pos = id_range_offsets + seg * 2;
        let id_range_offset = u16_at(data, range_offset_pos);
        if id_range_offset == 0 {
            return c.wrapping_add(id_delta as u16);
        }
        let glyph_addr = range_offset_pos + id_range_offset as usize + 2 * (c - start) as usize;
        if glyph_addr + 2 > data.len() {
            return 0;
        }
        let raw = u16_at(data, glyph_addr);
        if raw == 0 {
            return 0;
        }
        return raw.wrapping_add(id_delta as u16);
    }
    0
}

/// One point on a glyph outline, in font units (y-up). `on_curve` distinguishes real contour
/// points from quadratic-Bezier control points.
#[derive(Clone, Copy, Debug)]
pub struct GlyphPoint {
    pub x: f32,
    pub y: f32,
    pub on_curve: bool,
}

/// A glyph's outline as a set of closed contours (point lists, implicitly closed — last point
/// connects back to first). Empty for glyphs with no visible marks (space) or unsupported
/// composite glyphs.
pub struct GlyphOutline {
    pub contours: Vec<Vec<GlyphPoint>>,
}

/// Parse a TrueType simple-glyph body (the `glyf` table entry after the shared 10-byte header
/// of `numberOfContours`/bbox) at absolute offset `g`, given `num_contours` (>= 0, checked by
/// the caller) — `endPtsOfContours`, instructions (skipped, hinting only), run-length-encoded
/// point flags, then delta-encoded x then y coordinate arrays.
fn parse_simple_glyph(data: &[u8], g: usize, num_contours: usize) -> Option<GlyphOutline> {
    let mut p = g + 10;
    let mut end_pts = Vec::with_capacity(num_contours);
    for _ in 0..num_contours {
        if p + 2 > data.len() {
            return None;
        }
        end_pts.push(u16_at(data, p) as usize);
        p += 2;
    }
    let num_points = end_pts.last().map(|&e| e + 1).unwrap_or(0);
    if p + 2 > data.len() {
        return None;
    }
    let instruction_len = u16_at(data, p) as usize;
    p += 2 + instruction_len;

    // Flags, run-length encoded: a REPEAT_FLAG bit means the next byte is a repeat count for
    // the same flag byte.
    const ON_CURVE: u8 = 0x01;
    const X_SHORT: u8 = 0x02;
    const Y_SHORT: u8 = 0x04;
    const REPEAT: u8 = 0x08;
    const X_SAME_OR_POS: u8 = 0x10;
    const Y_SAME_OR_POS: u8 = 0x20;

    let mut flags = Vec::with_capacity(num_points);
    while flags.len() < num_points {
        if p >= data.len() {
            return None;
        }
        let f = data[p];
        p += 1;
        flags.push(f);
        if f & REPEAT != 0 {
            if p >= data.len() {
                return None;
            }
            let repeat = data[p];
            p += 1;
            for _ in 0..repeat {
                if flags.len() >= num_points {
                    break;
                }
                flags.push(f);
            }
        }
    }
    if flags.len() != num_points {
        return None;
    }

    let mut xs = Vec::with_capacity(num_points);
    let mut x = 0i32;
    for &f in &flags {
        if f & X_SHORT != 0 {
            if p >= data.len() {
                return None;
            }
            let d = data[p] as i32;
            p += 1;
            x += if f & X_SAME_OR_POS != 0 { d } else { -d };
        } else if f & X_SAME_OR_POS == 0 {
            if p + 2 > data.len() {
                return None;
            }
            x += i16_at(data, p) as i32;
            p += 2;
        } // else: X_SHORT unset AND X_SAME_OR_POS set -> delta is 0, x unchanged.
        xs.push(x);
    }
    let mut ys = Vec::with_capacity(num_points);
    let mut y = 0i32;
    for &f in &flags {
        if f & Y_SHORT != 0 {
            if p >= data.len() {
                return None;
            }
            let d = data[p] as i32;
            p += 1;
            y += if f & Y_SAME_OR_POS != 0 { d } else { -d };
        } else if f & Y_SAME_OR_POS == 0 {
            if p + 2 > data.len() {
                return None;
            }
            y += i16_at(data, p) as i32;
            p += 2;
        }
        ys.push(y);
    }

    let mut contours = Vec::with_capacity(num_contours);
    let mut start = 0usize;
    for &end in &end_pts {
        let mut pts = Vec::with_capacity(end + 1 - start);
        for i in start..=end {
            pts.push(GlyphPoint { x: xs[i] as f32, y: ys[i] as f32, on_curve: flags[i] & ON_CURVE != 0 });
        }
        contours.push(pts);
        start = end + 1;
    }
    Some(GlyphOutline { contours })
}

/// Flatten a glyph contour's on/off-curve points into a plain polyline (line segments only),
/// resolving TrueType's two curve conventions: an on-off-on triple is a quadratic Bezier
/// (subdivided into `STEPS` segments); two consecutive off-curve points have an *implied*
/// on-curve point at their midpoint (so the curve chain never actually breaks).
fn flatten_contour(pts: &[GlyphPoint]) -> Vec<(f32, f32)> {
    if pts.is_empty() {
        return Vec::new();
    }
    const STEPS: usize = 8;
    // Rotate so the contour starts on an on-curve point (inserting the implied midpoint if
    // the contour begins off-curve, which does happen — e.g. a curve that wraps around).
    let start_idx = pts.iter().position(|p| p.on_curve).unwrap_or(0);
    let n = pts.len();
    let ordered: Vec<GlyphPoint> = (0..n).map(|i| pts[(start_idx + i) % n]).collect();
    let first_on = if ordered[0].on_curve {
        (ordered[0].x, ordered[0].y)
    } else {
        // Contour is entirely off-curve (rare, but valid) — synthesize the implied start.
        let last = ordered[n - 1];
        ((ordered[0].x + last.x) * 0.5, (ordered[0].y + last.y) * 0.5)
    };

    let mut out = vec![first_on];
    let mut cur = first_on;
    let mut i = 0usize;
    while i < n {
        let p = ordered[i];
        if p.on_curve {
            if i != start_idx || out.len() > 1 {
                out.push((p.x, p.y));
            }
            cur = (p.x, p.y);
            i += 1;
            continue;
        }
        // Off-curve control point: find the following on-curve endpoint (implied midpoint if
        // the very next point is also off-curve).
        let ctrl = (p.x, p.y);
        let next = ordered[(i + 1) % n];
        let end = if next.on_curve { (next.x, next.y) } else { ((p.x + next.x) * 0.5, (p.y + next.y) * 0.5) };
        for s in 1..=STEPS {
            let t = s as f32 / STEPS as f32;
            let mt = 1.0 - t;
            let x = mt * mt * cur.0 + 2.0 * mt * t * ctrl.0 + t * t * end.0;
            let y = mt * mt * cur.1 + 2.0 * mt * t * ctrl.1 + t * t * end.1;
            out.push((x, y));
        }
        cur = end;
        i += 1;
    }
    out
}

/// **Pure**: rasterize one glyph's outline into an antialiased coverage mask, in pixel space
/// (`scale` = pixels per font unit; typically `point_size / units_per_em`). Returns
/// `(mask, width, height, origin_x, origin_y)` where `(origin_x, origin_y)` is the outline's
/// pixel-space bounding-box top-left, in the same y-*down* convention the rest of this app's
/// pixel buffers use (font outlines are y-*up*, so this flips the vertical axis).
///
/// Fill rule: even-odd (reuses the same scanline algorithm as `rasterize_selection_mask`'s
/// `Polygon` arm), not TrueType's own nonzero winding rule. For the vast majority of simple
/// Latin/digit/punctuation glyphs — a single outer contour, or an outer contour plus
/// oppositely-wound inner holes like "O"/"A" — even-odd and nonzero agree exactly. They can
/// diverge for pathological same-direction overlapping contours, which essentially don't
/// occur in mainstream font glyph data. A deliberate reuse of already-tested code over new,
/// unverified winding-accumulation logic (see DECISIONS.md for why that trade was made this
/// session in particular).
pub fn rasterize_glyph(outline: &GlyphOutline, scale: f32) -> (Vec<u8>, usize, usize, f32, f32) {
    let polylines: Vec<Vec<(f32, f32)>> = outline.contours.iter().map(|c| flatten_contour(c)).collect();
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for line in &polylines {
        for &(x, y) in line {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if !x0.is_finite() || x1 <= x0 || y1 <= y0 {
        return (Vec::new(), 0, 0, 0.0, 0.0);
    }

    // Pixel space is y-down (matching the rest of the app's buffers); font space is y-up, so
    // the vertical axis flips here — a glyph's top (max font y) becomes the mask's row 0.
    let px0 = (x0 * scale).floor();
    let px1 = (x1 * scale).ceil();
    let py0 = (-y1 * scale).floor();
    let py1 = (-y0 * scale).ceil();
    let width = (px1 - px0).max(1.0) as usize;
    let height = (py1 - py0).max(1.0) as usize;

    const SUPERSAMPLE: usize = 4;
    let mut mask = vec![0u8; width * height];
    for row in 0..height {
        for col in 0..width {
            let target_font_x = (px0 + col as f32 + 0.5) / scale;
            let mut hits = 0u32;
            for s in 0..SUPERSAMPLE {
                let pixel_y = py0 + row as f32 + (s as f32 + 0.5) / SUPERSAMPLE as f32;
                let sample_font_y = -pixel_y / scale;
                // Even-odd crossing test (see doc comment above for why even-odd, not
                // TrueType's nonzero winding rule).
                let mut xs: Vec<f32> = Vec::new();
                for line in &polylines {
                    let n = line.len();
                    if n < 2 {
                        continue;
                    }
                    for k in 0..n {
                        let (ax, ay) = line[k];
                        let (bx, by) = line[(k + 1) % n];
                        if (ay <= sample_font_y && by > sample_font_y) || (by <= sample_font_y && ay > sample_font_y) {
                            let t = (sample_font_y - ay) / (by - ay);
                            xs.push(ax + t * (bx - ax));
                        }
                    }
                }
                xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
                for pair in xs.chunks_exact(2) {
                    if target_font_x >= pair[0] && target_font_x < pair[1] {
                        hits += 1;
                        break;
                    }
                }
            }
            mask[row * width + col] = ((hits * 255) / SUPERSAMPLE as u32) as u8;
        }
    }
    (mask, width, height, px0, py0)
}

/// **Pure**: lay out one line of text (v1 scope: no wrapping, no bidi, no kerning/GPOS —
/// just cmap lookup + hmtx advance accumulation, left to right). Returns each character's
/// glyph id paired with the pen x-offset (font units, from the line's start) its outline's
/// local origin should be placed at; multiply by `point_size / units_per_em` for pixels.
pub fn layout_line(font: &Font, text: &str) -> Vec<(u16, f32)> {
    let mut pen_x = 0.0f32;
    let mut out = Vec::with_capacity(text.chars().count());
    for ch in text.chars() {
        let gid = font.glyph_id(ch);
        out.push((gid, pen_x));
        pen_x += font.advance_width(gid) as f32;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real, standard macOS system font — used only to *read/parse* at test time, never
    /// committed to the repo (Arial's font file itself isn't ours to redistribute; only this
    /// parsing code is). Skips gracefully (not a failure) on any machine without it, since a
    /// real-world `.ttf` exercises edge cases a hand-built synthetic fixture would miss.
    const TEST_FONT_PATH: &str = "/System/Library/Fonts/Supplemental/Arial.ttf";

    fn load_test_font() -> Option<Font> {
        let bytes = std::fs::read(TEST_FONT_PATH).ok()?;
        match Font::parse(bytes) {
            Ok(f) => Some(f),
            Err(e) => panic!("Arial.ttf failed to parse: {e}"),
        }
    }

    #[test]
    fn parses_arial_and_maps_ascii_characters_to_real_glyphs() {
        let Some(font) = load_test_font() else {
            eprintln!("skipping: {TEST_FONT_PATH} not present on this machine");
            return;
        };
        // units_per_em is conventionally a power-of-two-ish round number (1000 or 2048 are
        // the two overwhelmingly common values) — a sanity check that `head` parsed correctly.
        assert!(font.units_per_em() >= 100 && font.units_per_em() <= 4096, "implausible units_per_em: {}", font.units_per_em());

        for ch in ['A', 'Z', 'a', 'z', '0', '9'] {
            let gid = font.glyph_id(ch);
            assert_ne!(gid, 0, "expected a real glyph for {ch:?}, got .notdef (0)");
        }
        // A character essentially no font maps (Private Use Area) should fall through cleanly
        // to .notdef rather than panicking or returning a bogus id.
        assert_eq!(font.glyph_id('\u{E000}'), 0);
    }

    #[test]
    fn space_has_a_glyph_but_no_visible_outline() {
        let Some(font) = load_test_font() else {
            eprintln!("skipping: {TEST_FONT_PATH} not present on this machine");
            return;
        };
        let gid = font.glyph_id(' ');
        // Space maps to a real glyph slot (has metrics/advance) but must render nothing.
        let outline = font.glyph_outline(gid);
        assert!(outline.contours.is_empty(), "space should have no visible contours");
        assert!(font.advance_width(gid) > 0, "space should still have a nonzero advance width");
    }

    #[test]
    fn capital_h_rasterizes_with_two_separated_vertical_strokes() {
        let Some(font) = load_test_font() else {
            eprintln!("skipping: {TEST_FONT_PATH} not present on this machine");
            return;
        };
        let gid = font.glyph_id('H');
        let outline = font.glyph_outline(gid);
        assert!(!outline.contours.is_empty(), "'H' should have at least one contour");

        // Rasterize at a size where the strokes are comfortably several pixels wide, so this
        // isn't sensitive to single-pixel antialiasing noise.
        let scale = 64.0 / font.units_per_em() as f32; // ~64px em size
        let (mask, width, height, _ox, _oy) = rasterize_glyph(&outline, scale);
        assert!(width > 10 && height > 10, "unexpectedly tiny raster: {width}x{height}");

        // Sample near the top of the glyph (well above the crossbar, which in Arial sits
        // almost exactly at the vertical midline — sampling at height/2 would land ON it,
        // where there'd wrongly seem to be no gap): 'H' should show ink near the left edge
        // (left stroke), nothing in the middle (the gap between the strokes, above the
        // crossbar), and ink again near the right edge (right stroke).
        let sample_row = height / 5;
        let row_start = sample_row * width;
        let left_col = width / 8;
        let right_col = width - 1 - width / 8;
        let gap_col = width / 2;
        assert!(mask[row_start + left_col] > 128, "expected ink in the left stroke at col {left_col}, row {sample_row}");
        assert!(mask[row_start + right_col] > 128, "expected ink in the right stroke at col {right_col}, row {sample_row}");
        assert!(mask[row_start + gap_col] < 128, "expected no ink in the gap between H's strokes at col {gap_col}, row {sample_row}");
    }

    #[test]
    fn layout_line_accumulates_advances_left_to_right() {
        let Some(font) = load_test_font() else {
            eprintln!("skipping: {TEST_FONT_PATH} not present on this machine");
            return;
        };
        let laid_out = layout_line(&font, "AVA");
        assert_eq!(laid_out.len(), 3);
        // Pen positions strictly increase (nonzero advances, no overlap) — true for any real
        // font, proportional or monospace. The first character always starts at pen x=0.
        assert_eq!(laid_out[0].1, 0.0);
        assert!(laid_out[1].1 > laid_out[0].1, "second glyph should start after the first");
        assert!(laid_out[2].1 > laid_out[1].1, "third glyph should start after the second");
        // The two 'A's are the same glyph, so they advance by the same amount — the gap
        // between glyph 0 and glyph 1's pen position should equal the gap between 2 and the
        // (hypothetical) next, i.e. glyph 0's own advance width.
        let a_gid = font.glyph_id('A');
        assert_eq!(laid_out[0].0, a_gid);
        assert_eq!(laid_out[2].0, a_gid);
        assert_eq!(laid_out[1].1 - laid_out[0].1, font.advance_width(a_gid) as f32);
    }
}
