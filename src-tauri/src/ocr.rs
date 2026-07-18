//! Offline screenshot OCR using the native Windows OCR engine
//! (`Windows.Media.Ocr`). Runs fully on-device: no network, no cloud, no extra
//! dependency beyond Windows itself.
//!
//! Called from image capture; also exercised by a self-contained test.

const MAX_ENCODED_IMAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 16_384;
const MAX_SOURCE_PIXELS: u64 = 64_000_000;
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

    Ok(image.to_rgba8())
}

/// Recognize text from PNG-encoded image bytes with the user's installed OCR
/// languages. Returns the recognized text (possibly empty), or an error when no
/// OCR language is available on the machine.
#[cfg(target_os = "windows")]
pub fn recognize_png(png_bytes: &[u8]) -> Result<String, String> {
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
    for pixel in bgra.chunks_exact_mut(4) {
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

    Ok(result.Text().map_err(|e| e.to_string())?.to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn recognize_png(_png_bytes: &[u8]) -> Result<String, String> {
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
            Ok(text) => assert!(
                text.to_uppercase().contains("CUBBY"),
                "expected OCR to read the image, got: {text:?}"
            ),
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
            Ok(text) => text.to_uppercase(),
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
            .to_uppercase();
        assert!(
            ui_text.contains("TICKET"),
            "small UI OCR result: {ui_text:?}"
        );
        assert!(ui_text.contains("2048"), "small UI OCR result: {ui_text:?}");
    }
}
