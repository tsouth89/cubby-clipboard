//! Offline screenshot OCR using the native Windows OCR engine
//! (`Windows.Media.Ocr`). Runs fully on-device: no network, no cloud, no extra
//! dependency beyond Windows itself.
//!
//! Called from image capture; also exercised by a self-contained test.

use serde::{Deserialize, Serialize};

/// One recognized word and its bounding box, in the pixel coordinate space of
/// the image handed to the OCR engine (see `OcrLayout::image_width`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrWordBox {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Which recognized line this word came from. Lines arrive top to bottom
    /// and words left to right, so `words` order is reading order and this
    /// index is what tells a selection where to break lines.
    ///
    /// `None` on layouts captured before line indices were recorded; readers
    /// infer bands from the boxes instead (see `commands::ocr_text_layout`).
    #[serde(default)]
    pub line: Option<u32>,
}

/// The per-word boxes plus the pixel dimensions of the image OCR actually ran
/// on. Persisted (encrypted) at capture time so a later search can highlight
/// matched words on the image without re-running OCR (SOU-242: phase 1 stores
/// this; phase 2 renders it).
///
/// The dimensions are essential and easy to overlook: `decode_for_ocr` may
/// downscale a large screenshot before OCR, so the word coordinates are in that
/// (possibly reduced) space, not the full image's. Storing `image_width`/
/// `image_height` lets phase 2 scale the boxes onto the real preview. Without
/// them the boxes are unusable for anything that was downscaled, and the only
/// recovery would be a full re-OCR — exactly what capturing now avoids.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OcrLayout {
    pub image_width: u32,
    pub image_height: u32,
    pub words: Vec<OcrWordBox>,
}

/// The full result of recognizing an image: the assembled text and the word
/// layout.
#[derive(Debug, Clone, Default)]
pub struct OcrRecognition {
    pub text: String,
    pub layout: OcrLayout,
}

/// Rewrite each word box's text to match a saved OCR correction, keeping the
/// original geometry and line indices.
///
/// Scan text edits the assembled block, not the boxes. A 1:1 token rewrite is
/// what makes drag-select copy the correction (SBS-1010). Extra tokens land on
/// the last box; leftover boxes are dropped so a selection cannot yield a
/// pre-correction reading. An empty correction clears every box.
/// Group words into lines when the stored layout predates line indices. Words
/// are already in reading order, so this only has to notice where the text
/// steps down: a word whose vertical center leaves the current line's band
/// starts a new one. Without it every legacy clip would look like a single
/// line, and a multi-line selection would copy back as one run-on.
pub(crate) fn infer_line_indices(words: &[OcrWordBox]) -> Vec<u32> {
    let mut lines = Vec::with_capacity(words.len());
    let mut line = 0u32;
    let mut band_center = f32::NAN;
    let mut band_height = 0.0f32;

    for word in words {
        let center = word.y + word.height / 2.0;
        if band_center.is_nan() {
            band_center = center;
            band_height = word.height.max(1.0);
        } else {
            // Tolerate half a line height of baseline wobble within a line;
            // anything beyond that is the next line.
            let tolerance = band_height.max(word.height).max(1.0) * 0.6;
            if (center - band_center).abs() > tolerance {
                line += 1;
                band_center = center;
                band_height = word.height.max(1.0);
            } else {
                // Track the running center so a gently drifting line stays one.
                band_center = (band_center + center) / 2.0;
                band_height = band_height.max(word.height);
            }
        }
        lines.push(line);
    }
    lines
}

pub fn apply_ocr_text_to_layout(mut layout: OcrLayout, text: &str) -> OcrLayout {
    if layout.words.is_empty() {
        return layout;
    }
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        layout.words.clear();
        return layout;
    }

    // A layout stored before line indices were recorded has `line: None` on
    // every box. `commands::ocr_text_layout` falls back to inferred bands for
    // those, so the pane the user corrected was already split into visual
    // lines; resolving the same way here keeps the saved layout matching what
    // they saw, instead of treating a legacy clip as one giant line.
    let inferred = infer_line_indices(&layout.words);
    let resolved: Vec<u32> = layout
        .words
        .iter()
        .zip(inferred.iter())
        .map(|(word, fallback)| word.line.unwrap_or(*fallback))
        .collect();

    // Words are in reading order, so a run of the same resolved index is one
    // line. Grouping by runs rather than by value also absorbs the gaps the
    // engine leaves when it skips an empty word.
    let mut groups: Vec<Vec<OcrWordBox>> = Vec::new();
    let mut previous: Option<u32> = None;
    for (word, line) in layout.words.drain(..).zip(resolved) {
        match previous {
            Some(last) if last == line => groups
                .last_mut()
                .expect("a previous line means a group exists")
                .push(word),
            _ => groups.push(vec![word]),
        }
        previous = Some(line);
    }

    let group_count = groups.len();
    let mut rewritten: Vec<OcrWordBox> = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        // A correction with fewer lines removed these; leaving them selectable
        // would let a drag copy text the user deleted.
        if index >= lines.len() {
            break;
        }
        // More corrected lines than recognized ones: the surplus has no
        // geometry of its own, so it folds into the last line rather than
        // being dropped.
        let line = if index + 1 == group_count && lines.len() > group_count {
            lines[index..].join("\n")
        } else {
            lines[index].to_string()
        };
        rewritten.extend(apply_line_to_boxes(group, &line));
    }
    layout.words = rewritten;
    layout
}

/// Map one corrected line onto the boxes recognized for that line.
fn apply_line_to_boxes(mut boxes: Vec<OcrWordBox>, line: &str) -> Vec<OcrWordBox> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.is_empty() || boxes.is_empty() {
        return Vec::new();
    }

    if tokens.len() > boxes.len() {
        let last = boxes.len() - 1;
        for (word, token) in boxes.iter_mut().take(last).zip(tokens.iter()) {
            word.text = (*token).to_string();
        }
        boxes[last].text = tokens[last..].join(" ");
        return boxes;
    }

    if tokens.len() == 1 && boxes.len() > 1 {
        // Windows OCR emits one box per character for CJK and a line with no
        // spaces, so the whole line is a single token. Placing it on the first
        // box and dropping the rest left one glyph selectable (SBS-1010).
        let chars: Vec<String> = tokens[0].chars().map(|c| c.to_string()).collect();
        if chars.len() == boxes.len() {
            for (word, character) in boxes.iter_mut().zip(chars) {
                word.text = character;
            }
            return boxes;
        }
        // The counts disagree, so which glyph belongs in which rectangle is
        // unknowable. Cover the line with one box instead of dropping the rest
        // of its area.
        let mut merged = union_boxes(&boxes);
        merged.text = tokens[0].to_string();
        return vec![merged];
    }

    boxes.truncate(tokens.len());
    for (word, token) in boxes.iter_mut().zip(tokens) {
        word.text = token.to_string();
    }
    boxes
}

/// The smallest box covering all of `boxes`, keeping the first box's other
/// fields. Caller guarantees a non-empty slice.
fn union_boxes(boxes: &[OcrWordBox]) -> OcrWordBox {
    let mut merged = boxes[0].clone();
    let mut right = merged.x + merged.width;
    let mut bottom = merged.y + merged.height;
    for word in &boxes[1..] {
        merged.x = merged.x.min(word.x);
        merged.y = merged.y.min(word.y);
        right = right.max(word.x + word.width);
        bottom = bottom.max(word.y + word.height);
    }
    merged.width = right - merged.x;
    merged.height = bottom - merged.y;
    merged
}

const MAX_ENCODED_IMAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 16_384;
// Bounds the fully-decoded RGBA buffer (~4 bytes/pixel) and everything derived
// from it. 48 MP keeps 8K desktop screenshots (33 MP) comfortably in range while
// capping the decode near ~192 MB, so the resize / BGRA-swap / WinRT-buffer steps
// that follow don't push the transient peak as high as the old 64 MP (256 MB)
// limit allowed.
const MAX_SOURCE_PIXELS: u64 = 48_000_000;
const MAX_DECODE_ALLOCATION_BYTES: u64 = 256 * 1024 * 1024;

fn source_dimensions(png_bytes: &[u8]) -> Result<(u32, u32), String> {
    use image::io::Reader as ImageReader;
    use std::io::Cursor;

    if png_bytes.len() > MAX_ENCODED_IMAGE_BYTES {
        return Err("screenshot is too large for safe OCR processing".to_string());
    }

    let dimensions = ImageReader::new(Cursor::new(png_bytes))
        .with_guessed_format()
        .map_err(|e| format!("could not inspect screenshot: {e}"))?
        .into_dimensions()
        .map_err(|e| format!("could not inspect screenshot dimensions: {e}"))?;

    validate_source_dimensions(dimensions.0, dimensions.1)?;
    Ok(dimensions)
}

fn validate_source_dimensions(width: u32, height: u32) -> Result<(), String> {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0 || height == 0 {
        return Err("screenshot has invalid dimensions".to_string());
    }
    if width > MAX_SOURCE_DIMENSION || height > MAX_SOURCE_DIMENSION || pixels > MAX_SOURCE_PIXELS {
        return Err(format!(
            "screenshot dimensions {width}x{height} exceed safe OCR limits"
        ));
    }
    Ok(())
}

fn decode_for_ocr(png_bytes: &[u8], max_ocr_dimension: u32) -> Result<image::RgbaImage, String> {
    use image::imageops::FilterType;
    use image::io::{Limits, Reader as ImageReader};
    use std::io::Cursor;

    let _ = source_dimensions(png_bytes)?;
    if max_ocr_dimension == 0 {
        return Err("Windows OCR reported an invalid image limit".to_string());
    }

    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOCATION_BYTES);

    let mut reader = ImageReader::new(Cursor::new(png_bytes))
        .with_guessed_format()
        .map_err(|e| format!("could not inspect screenshot: {e}"))?;
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| format!("could not decode screenshot within safe limits: {e}"))?;

    let image = if image.width() > max_ocr_dimension || image.height() > max_ocr_dimension {
        image.resize(max_ocr_dimension, max_ocr_dimension, FilterType::Lanczos3)
    } else {
        image
    };

    // Consume the decoded image so an already-RGBA source isn't cloned; this
    // keeps a single full-resolution buffer at the peak instead of two.
    Ok(image.into_rgba8())
}

/// Recognize text from PNG-encoded image bytes with the user's installed OCR
/// languages. Returns the recognized text (possibly empty) plus per-word
/// bounding boxes, or an error when no OCR language is available on the machine.
#[cfg(target_os = "windows")]
pub fn recognize_png(png_bytes: &[u8]) -> Result<OcrRecognition, String> {
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Security::Cryptography::CryptographicBuffer;

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|_| "Windows OCR is unavailable (no OCR language installed)".to_string())?;
    let max_ocr_dimension = OcrEngine::MaxImageDimension().map_err(|e| e.to_string())?;

    // Bound source dimensions and decoder allocation, then downscale only when
    // Windows OCR cannot accept the original dimensions. Normal screenshots are
    // passed through at their native resolution.
    let image = decode_for_ocr(png_bytes, max_ocr_dimension)?;
    let (width, height) = image.dimensions();
    let mut bgra = image.into_raw();
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }

    let buffer = CryptographicBuffer::CreateFromByteArray(&bgra).map_err(|e| e.to_string())?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        width as i32,
        height as i32,
    )
    .map_err(|e| e.to_string())?;

    // OCR runs off the capture hot path; poll the single async op to completion.
    // AsyncStatus ABI values: 0 = Started, 1 = Completed, 2 = Canceled, 3 = Error.
    let operation = engine.RecognizeAsync(&bitmap).map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match operation.Status().map_err(|e| e.to_string())?.0 {
            1 => break,
            2 => return Err("Windows OCR was canceled".to_string()),
            3 => return Err("Windows OCR failed".to_string()),
            _ if std::time::Instant::now() >= deadline => {
                return Err("Windows OCR timed out".to_string());
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
    let result = operation.GetResults().map_err(|e| e.to_string())?;

    // Assemble the text line by line so the recognized output keeps the source
    // layout. OcrResult::Text flattens everything into a single space-separated
    // blob with no line breaks, which reads as an unusable run-on for any
    // multi-line image.
    let lines = result.Lines().map_err(|e| e.to_string())?;
    let mut text = String::new();
    let mut words: Vec<OcrWordBox> = Vec::new();
    for index in 0..lines.Size().map_err(|e| e.to_string())? {
        let line = lines.GetAt(index).map_err(|e| e.to_string())?;

        // Capture each word's box (SOU-242). The rect is in the coordinate space
        // of the image we handed the engine (`width`/`height` above, post any
        // `decode_for_ocr` downscale), recorded in the OcrLayout so phase 2 can
        // scale onto the full-size preview.
        let line_words = line.Words().map_err(|e| e.to_string())?;
        for word_index in 0..line_words.Size().map_err(|e| e.to_string())? {
            let word = line_words.GetAt(word_index).map_err(|e| e.to_string())?;
            let word_text = word.Text().map_err(|e| e.to_string())?.to_string();
            if word_text.is_empty() {
                continue;
            }
            let rect = word.BoundingRect().map_err(|e| e.to_string())?;
            words.push(OcrWordBox {
                text: word_text,
                x: rect.X,
                y: rect.Y,
                width: rect.Width,
                height: rect.Height,
                line: Some(index),
            });
        }

        let line_text = line.Text().map_err(|e| e.to_string())?.to_string();
        if line_text.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&line_text);
    }
    Ok(OcrRecognition {
        text,
        layout: OcrLayout {
            image_width: width,
            image_height: height,
            words,
        },
    })
}

#[cfg(not(target_os = "windows"))]
pub fn recognize_png(_png_bytes: &[u8]) -> Result<OcrRecognition, String> {
    Err("Screenshot OCR requires Windows".to_string())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::{decode_for_ocr, recognize_png, validate_source_dimensions};

    #[test]
    fn accepts_large_desktop_screenshots_within_the_memory_budget() {
        assert!(validate_source_dimensions(7680, 4320).is_ok());
    }

    #[test]
    fn rejects_dimensions_that_could_exhaust_ocr_memory() {
        let error = validate_source_dimensions(10_000, 10_000).unwrap_err();
        assert!(error.contains("exceed safe OCR limits"));
    }

    #[test]
    fn downscales_only_images_above_the_windows_ocr_limit() {
        let image = image::DynamicImage::new_rgba8(3200, 1800);
        let mut png = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut png),
                image::ImageOutputFormat::Png,
            )
            .expect("test image should encode");

        let decoded = decode_for_ocr(&png, 2600).expect("test image should decode");
        assert_eq!(decoded.dimensions(), (2600, 1463));
    }

    #[test]
    fn reads_text_from_a_generated_image() {
        // Draw known text to a PNG with System.Drawing so we have a real image.
        let path = std::env::temp_dir().join("cubby-ocr-test.png");
        let target = path.to_string_lossy().replace('\\', "\\\\");
        let script = format!(
            "Add-Type -AssemblyName System.Drawing; \
             $b = New-Object System.Drawing.Bitmap 640,160; \
             $g = [System.Drawing.Graphics]::FromImage($b); \
             $g.Clear([System.Drawing.Color]::White); \
             $f = New-Object System.Drawing.Font('Segoe UI',40); \
             $g.DrawString('CUBBY OCR 12345', $f, [System.Drawing.Brushes]::Black, 10, 40); \
             $g.Dispose(); $b.Save('{target}'); $b.Dispose()"
        );
        let generated = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !generated {
            eprintln!("skipping OCR test: could not generate the sample image");
            return;
        }
        let png = std::fs::read(&path).expect("sample image should be readable");
        let _ = std::fs::remove_file(&path);

        match recognize_png(&png) {
            Ok(recognition) => {
                let text = recognition.text;
                assert!(
                    text.to_uppercase().contains("CUBBY"),
                    "expected OCR to read the image, got: {text:?}"
                );
            }
            // No OCR language pack (e.g. on some CI images) -> skip, don't fail.
            // Only the missing-language case is an acceptable skip; any other
            // failure (bitmap, API, poll) must fail the test loudly.
            Err(error) if error.contains("no OCR language") => {
                eprintln!("skipping OCR assertion: {error}")
            }
            Err(error) => panic!("OCR failed unexpectedly: {error}"),
        }
    }

    #[test]
    fn reads_dark_error_dialogs_and_small_ui_text() {
        let dark_path = std::env::temp_dir().join("cubby-ocr-dark-corpus.png");
        let ui_path = std::env::temp_dir().join("cubby-ocr-ui-corpus.png");
        let dark_target = dark_path.to_string_lossy().replace('\\', "\\\\");
        let ui_target = ui_path.to_string_lossy().replace('\\', "\\\\");
        let script = format!(
            "Add-Type -AssemblyName System.Drawing; \
             $dark = New-Object System.Drawing.Bitmap 1200,260; \
             $g = [System.Drawing.Graphics]::FromImage($dark); \
             $g.Clear([System.Drawing.Color]::FromArgb(32,32,36)); \
             $f = New-Object System.Drawing.Font('Segoe UI',34); \
             $g.DrawString('ERROR 0x80070005 - ACCESS DENIED', $f, [System.Drawing.Brushes]::White, 24, 82); \
             $g.Dispose(); $dark.Save('{dark_target}'); $dark.Dispose(); \
             $ui = New-Object System.Drawing.Bitmap 1920,1080; \
             $g2 = [System.Drawing.Graphics]::FromImage($ui); \
             $g2.Clear([System.Drawing.Color]::White); \
             $f2 = New-Object System.Drawing.Font('Segoe UI',20); \
             $g2.DrawString('Server support-17   Ticket CB-2048', $f2, [System.Drawing.Brushes]::Black, 80, 120); \
             $g2.Dispose(); $ui.Save('{ui_target}'); $ui.Dispose()"
        );
        let generated = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !generated {
            eprintln!("skipping OCR corpus test: could not generate sample images");
            return;
        }

        let dark_png = std::fs::read(&dark_path).expect("dark corpus image should be readable");
        let ui_png = std::fs::read(&ui_path).expect("UI corpus image should be readable");
        let _ = std::fs::remove_file(&dark_path);
        let _ = std::fs::remove_file(&ui_path);

        let dark_text = match recognize_png(&dark_png) {
            Ok(recognition) => recognition.text.to_uppercase(),
            Err(error) if error.contains("no OCR language") => {
                eprintln!("skipping OCR corpus assertion: {error}");
                return;
            }
            Err(error) => panic!("dark OCR corpus failed unexpectedly: {error}"),
        };
        assert!(
            dark_text.contains("ERROR"),
            "dark OCR result: {dark_text:?}"
        );
        assert!(
            dark_text.contains("DENIED"),
            "dark OCR result: {dark_text:?}"
        );

        let ui_text = recognize_png(&ui_png)
            .unwrap_or_else(|error| panic!("small UI OCR corpus failed unexpectedly: {error}"))
            .text
            .to_uppercase();
        assert!(
            ui_text.contains("TICKET"),
            "small UI OCR result: {ui_text:?}"
        );
        assert!(ui_text.contains("2048"), "small UI OCR result: {ui_text:?}");
    }
}

#[cfg(test)]
mod apply_ocr_text_tests {
    use super::{apply_ocr_text_to_layout, OcrLayout, OcrWordBox};

    fn box_at(text: &str, x: f32, line: u32) -> OcrWordBox {
        OcrWordBox {
            text: text.to_string(),
            x,
            y: if line == 0 { 0.0 } else { 25.0 },
            width: 20.0,
            height: 10.0,
            line: Some(line),
        }
    }

    fn layout(words: Vec<OcrWordBox>) -> OcrLayout {
        OcrLayout {
            image_width: 100,
            image_height: 50,
            words,
        }
    }

    /// SBS-1010, CJK. Windows OCR emits one box per character for CJK and a
    /// line string with no spaces, so a whole line is a single whitespace
    /// token against N boxes. Splitting the block on whitespace collapsed the
    /// line onto its first character's rectangle and dropped the rest, and on
    /// a two-line image the second line landed on the first line's second
    /// character. Drag-select then hit one glyph.
    #[test]
    fn an_unspaced_line_maps_one_character_per_box() {
        let corrected = apply_ocr_text_to_layout(
            layout(vec![
                box_at("你", 0.0, 0),
                box_at("好", 20.0, 0),
                box_at("世", 40.0, 0),
                box_at("界", 60.0, 0),
            ]),
            "你好世果",
        );
        assert_eq!(corrected.words.len(), 4, "every character box must survive");
        let texts: Vec<&str> = corrected.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, vec!["你", "好", "世", "果"]);
        assert_eq!(corrected.words[3].x, 60.0, "geometry must be untouched");
    }

    #[test]
    fn a_second_unspaced_line_stays_on_its_own_boxes() {
        let corrected = apply_ocr_text_to_layout(
            layout(vec![
                box_at("你", 0.0, 0),
                box_at("好", 20.0, 0),
                box_at("世", 0.0, 1),
                box_at("界", 20.0, 1),
            ]),
            "你好\n世果",
        );
        assert_eq!(corrected.words.len(), 4, "no line may be dropped");
        let second: Vec<&str> = corrected
            .words
            .iter()
            .filter(|w| w.line == Some(1))
            .map(|w| w.text.as_str())
            .collect();
        assert_eq!(
            second,
            vec!["世", "果"],
            "line 2 must stay on line 2's boxes"
        );
        assert_eq!(
            corrected.words[2].y, 25.0,
            "line 2 text must not land on a line 1 rectangle"
        );
    }

    /// When the corrected line's character count does not match the box count
    /// there is no way to know which glyph belongs in which rectangle, but the
    /// line must stay selectable across its whole area rather than shrink to
    /// the first box.
    #[test]
    fn an_unspaced_line_of_a_different_length_covers_the_whole_line() {
        let corrected = apply_ocr_text_to_layout(
            layout(vec![
                box_at("你", 0.0, 0),
                box_at("好", 20.0, 0),
                box_at("世", 40.0, 0),
            ]),
            "你好",
        );
        assert_eq!(corrected.words.len(), 1);
        assert_eq!(corrected.words[0].text, "你好");
        assert_eq!(corrected.words[0].x, 0.0);
        assert_eq!(
            corrected.words[0].width, 60.0,
            "the merged box must span the whole line, not just the first glyph"
        );
    }

    /// A user who adds a line has given text with no geometry of its own. It
    /// folds into the last recognized line so the correction is not silently
    /// truncated. The added text joins with a space, the same way surplus
    /// tokens already land on the last box -- one rectangle cannot express a
    /// line break, and the canonical text is stored separately in `ocr_text`.
    #[test]
    fn extra_corrected_lines_fold_into_the_last_line() {
        let corrected = apply_ocr_text_to_layout(
            layout(vec![box_at("one", 0.0, 0), box_at("two", 0.0, 1)]),
            "one\ntwo\nthree",
        );
        assert_eq!(corrected.words.len(), 2);
        assert_eq!(corrected.words[0].text, "one");
        assert_eq!(corrected.words[1].text, "two three");
    }

    /// A layout stored before line indices were recorded has `line: None`
    /// everywhere. `commands::ocr_text_layout` already falls back to inferred
    /// bands, so the pane the user corrected was split into visual lines. If
    /// this side treated the whole clip as one line, the saved `ocr_words`
    /// would disagree with the overlay the user just approved.
    ///
    /// Deliberately unspaced: with space-separated words both groupings
    /// happen to agree, so a Latin fixture here would pass either way and
    /// prove nothing.
    #[test]
    fn a_legacy_layout_without_line_indices_still_splits_into_visual_lines() {
        let legacy = |text: &str, x: f32, y: f32| OcrWordBox {
            text: text.to_string(),
            x,
            y,
            width: 20.0,
            height: 10.0,
            line: None,
        };
        let corrected = apply_ocr_text_to_layout(
            layout(vec![
                legacy("你", 0.0, 0.0),
                legacy("好", 20.0, 0.0),
                legacy("世", 0.0, 40.0),
                legacy("界", 20.0, 40.0),
            ]),
            "你好\n世果",
        );
        assert_eq!(corrected.words.len(), 4, "no band may be dropped");
        assert_eq!(
            corrected
                .words
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>(),
            vec!["你", "好", "世", "果"],
        );
        assert_eq!(
            corrected.words[2].y, 40.0,
            "line 2 text must stay on the lower band"
        );
    }

    #[test]
    fn a_same_length_correction_rewrites_box_text_and_keeps_geometry() {
        // The SBS-1010 failure: OCR misread a URL, the user fixed the assembled
        // block, and drag-select still copied the misspelling because the box
        // text was left alone.
        let corrected = apply_ocr_text_to_layout(
            layout(vec![box_at("htlps://exarnple.com", 10.0, 0)]),
            "https://example.com",
        );
        assert_eq!(corrected.words.len(), 1);
        assert_eq!(corrected.words[0].text, "https://example.com");
        assert_eq!(corrected.words[0].x, 10.0);
        assert_eq!(corrected.words[0].line, Some(0));
        assert_eq!(corrected.image_width, 100);
    }

    #[test]
    fn leftover_boxes_are_dropped_so_a_shorter_correction_cannot_copy_old_words() {
        let corrected = apply_ocr_text_to_layout(
            layout(vec![
                box_at("hello", 0.0, 0),
                box_at("there", 25.0, 0),
                box_at("second", 0.0, 1),
            ]),
            "hello there",
        );
        assert_eq!(
            corrected
                .words
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>(),
            vec!["hello", "there"]
        );
    }

    #[test]
    fn extra_tokens_land_on_the_last_box_instead_of_inventing_geometry() {
        let corrected = apply_ocr_text_to_layout(
            layout(vec![box_at("hello", 0.0, 0), box_at("there", 25.0, 0)]),
            "hello there extra words",
        );
        assert_eq!(corrected.words[0].text, "hello");
        assert_eq!(corrected.words[1].text, "there extra words");
        assert_eq!(corrected.words.len(), 2);
    }

    #[test]
    fn an_empty_correction_clears_every_box() {
        let corrected = apply_ocr_text_to_layout(layout(vec![box_at("stale", 0.0, 0)]), "   ");
        assert!(corrected.words.is_empty());
    }

    #[test]
    fn a_correction_with_no_existing_boxes_does_not_invent_any() {
        let corrected = apply_ocr_text_to_layout(layout(Vec::new()), "https://example.com");
        assert!(corrected.words.is_empty());
    }
}
