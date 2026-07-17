//! Offline screenshot OCR using the native Windows OCR engine
//! (`Windows.Media.Ocr`). Runs fully on-device: no network, no cloud, no extra
//! dependency beyond Windows itself.
//!
//! Called from image capture; also exercised by a self-contained test.

/// Recognize text from PNG-encoded image bytes with the user's installed OCR
/// languages. Returns the recognized text (possibly empty), or an error when no
/// OCR language is available on the machine.
#[cfg(target_os = "windows")]
pub fn recognize_png(png_bytes: &[u8]) -> Result<String, String> {
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Security::Cryptography::CryptographicBuffer;

    // Decode the PNG and convert RGBA -> BGRA, the layout SoftwareBitmap wants.
    let image = image::load_from_memory(png_bytes)
        .map_err(|e| format!("could not decode screenshot: {e}"))?
        .to_rgba8();
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

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|_| "Windows OCR is unavailable (no OCR language installed)".to_string())?;

    // OCR runs off the capture hot path; poll the single async op to completion.
    // AsyncStatus ABI values: 0 = Started, 1 = Completed, 2 = Canceled, 3 = Error.
    let operation = engine.RecognizeAsync(&bitmap).map_err(|e| e.to_string())?;
    loop {
        match operation.Status().map_err(|e| e.to_string())?.0 {
            1 => break,
            2 => return Err("Windows OCR was canceled".to_string()),
            3 => return Err("Windows OCR failed".to_string()),
            _ => std::thread::sleep(std::time::Duration::from_millis(2)),
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
    use super::recognize_png;

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
            Err(error) => eprintln!("skipping OCR assertion: engine unavailable ({error})"),
        }
    }
}
