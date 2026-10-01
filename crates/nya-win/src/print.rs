//! Printing a PDF on this computer (print jobs coming from the host).
//!
//! Windows.Data.Pdf renders each page to a bitmap (at most [`MAX_DPI`], with
//! a white background); GDI scales it onto the printer's printable area,
//! keeping the aspect ratio. Works with any installed printer driver and
//! needs no PDF reader. COM must be initialised on the calling thread.

use std::path::Path;

use anyhow::{bail, Context, Result};
use windows::core::{HSTRING, PCWSTR};
use windows::Data::Pdf::{PdfDocument, PdfPage, PdfPageRenderOptions};
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat, BitmapTransform, ColorManagementMode, ExifOrientationMode};
use windows::Storage::StorageFile;
use windows::Storage::Streams::InMemoryRandomAccessStream;
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, GetDeviceCaps, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HORZRES, LOGPIXELSX,
    LOGPIXELSY, SRCCOPY, VERTRES,
};
use windows::Win32::Graphics::Printing::GetDefaultPrinterW;
use windows::Win32::Storage::Xps::{EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};

/// Pages are rendered at most this sharp (memory: an A4 page at 300 dpi is ~35 MB).
pub const MAX_DPI: f64 = 300.0;

/// The default printer's name.
pub fn default_printer() -> Option<String> {
    let mut len = 0u32;
    // SAFETY: first call asks for the size, second fills a buffer of that size.
    unsafe {
        let _ = GetDefaultPrinterW(windows::core::PWSTR::null(), &mut len);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        if !GetDefaultPrinterW(windows::core::PWSTR(buf.as_mut_ptr()), &mut len).as_bool() {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]))
    }
}

pub fn open_pdf(path: &Path) -> Result<PdfDocument> {
    let full = std::fs::canonicalize(path).with_context(|| format!("{}", path.display()))?;
    // StorageFile wants a plain path, not the \\?\ form canonicalize returns.
    let plain = full.to_string_lossy().trim_start_matches(r"\\?\").to_owned();
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(plain))?.get()?;
    Ok(PdfDocument::LoadFromFileAsync(&file)?.get().context("PDF 无法打开")?)
}

/// Render one page about `width` x `height` pixels, BGRA top-down. The size
/// is in DIPs: on a scaled display the bitmap comes out larger, so the real
/// size is returned with the pixels.
pub fn render_page(page: &PdfPage, width: u32, height: u32) -> Result<(Vec<u8>, u32, u32)> {
    let opts = PdfPageRenderOptions::new()?;
    opts.SetDestinationWidth(width)?;
    opts.SetDestinationHeight(height)?;
    let stream = InMemoryRandomAccessStream::new()?;
    page.RenderWithOptionsToStreamAsync(&stream, &opts)?.get()?;
    stream.Seek(0)?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.get()?;
    let (w, h) = (decoder.PixelWidth()?, decoder.PixelHeight()?);
    let data = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Ignore,
            &BitmapTransform::new()?,
            ExifOrientationMode::IgnoreExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )?
        .get()?
        .DetachPixelData()?;
    let px = data.to_vec();
    if px.len() != w as usize * h as usize * 4 {
        bail!("rendered {} bytes for {w}x{h}", px.len());
    }
    Ok((px, w, h))
}

/// Where a page of `page_w` x `page_h` (any unit) goes in a printable area of
/// `area_w` x `area_h` device pixels: centred, as large as fits.
pub fn fit(page_w: f64, page_h: f64, area_w: i32, area_h: i32) -> (i32, i32, i32, i32) {
    let scale = (area_w as f64 / page_w).min(area_h as f64 / page_h);
    let (w, h) = ((page_w * scale).round() as i32, (page_h * scale).round() as i32);
    ((area_w - w) / 2, (area_h - h) / 2, w, h)
}

/// Print a PDF on `printer` (default printer if `None`). Returns the printer
/// name and the number of pages printed.
pub fn print_pdf(path: &Path, title: &str, printer: Option<&str>) -> Result<(String, u32)> {
    let printer = match printer {
        Some(p) => p.to_owned(),
        None => default_printer().context("本机没有默认打印机")?,
    };
    let doc = open_pdf(path)?;
    let pages = doc.PageCount()?;
    let name = HSTRING::from(printer.as_str());
    // SAFETY: the DC is created, used and deleted here; buffers outlive the calls.
    unsafe {
        let dc = CreateDCW(windows::core::w!("WINSPOOL"), &name, PCWSTR::null(), None);
        if dc.is_invalid() {
            bail!("打不开打印机 {printer}");
        }
        let result = (|| -> Result<()> {
            let (area_w, area_h) = (GetDeviceCaps(dc, HORZRES), GetDeviceCaps(dc, VERTRES));
            let (dpi_x, dpi_y) = (GetDeviceCaps(dc, LOGPIXELSX) as f64, GetDeviceCaps(dc, LOGPIXELSY) as f64);
            let title = HSTRING::from(title);
            let info = DOCINFOW { cbSize: std::mem::size_of::<DOCINFOW>() as i32, lpszDocName: PCWSTR(title.as_ptr()), ..Default::default() };
            if StartDocW(dc, &info) <= 0 {
                bail!("打印机 {printer} 拒绝了打印任务");
            }
            for i in 0..pages {
                let page = doc.GetPage(i)?;
                let size = page.Size()?; // DIPs (1/96 inch)
                let (x, y, w, h) = fit(size.Width as f64 / 96.0 * dpi_x, size.Height as f64 / 96.0 * dpi_y, area_w, area_h);
                // Render no sharper than the printer, nor than MAX_DPI.
                let render_scale = (MAX_DPI / dpi_x.max(1.0)).min(1.0);
                let (rw, rh) = (((w as f64) * render_scale).round().max(1.0) as u32, ((h as f64) * render_scale).round().max(1.0) as u32);
                let (bgra, rw, rh) = render_page(&page, rw, rh)?;
                let bgr = to_bgr24(&bgra, rw as usize, rh as usize);
                let bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: rw as i32,
                        biHeight: -(rh as i32), // top-down
                        biPlanes: 1,
                        biBitCount: 24,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                StartPage(dc);
                StretchDIBits(dc, x, y, w, h, 0, 0, rw as i32, rh as i32, Some(bgr.as_ptr().cast()), &bmi, DIB_RGB_COLORS, SRCCOPY);
                EndPage(dc);
            }
            EndDoc(dc);
            Ok(())
        })();
        let _ = DeleteDC(dc);
        result?;
    }
    Ok((printer, pages))
}

/// BGRA rows to BGR rows padded to 4 bytes (DIB layout, widest driver support).
fn to_bgr24(bgra: &[u8], w: usize, h: usize) -> Vec<u8> {
    let stride = (w * 3 + 3) & !3;
    let mut out = vec![0u8; stride * h];
    for y in 0..h {
        let src = &bgra[y * w * 4..(y + 1) * w * 4];
        let dst = &mut out[y * stride..y * stride + w * 3];
        for (d, s) in dst.chunks_exact_mut(3).zip(src.chunks_exact(4)) {
            d.copy_from_slice(&s[..3]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page PDF (200 x 100 pt) with a black box at (10,10)-(110,60) pt.
    fn tiny_pdf() -> Vec<u8> {
        let content = "0 0 0 rg 10 10 100 50 re f\n";
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << >> >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for off in offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
        pdf
    }

    #[test]
    fn renders_a_page() {
        crate::com_init();
        let path = std::env::temp_dir().join(format!("nya-print-{}.pdf", std::process::id()));
        std::fs::write(&path, tiny_pdf()).unwrap();
        let doc = open_pdf(&path).unwrap();
        assert_eq!(doc.PageCount().unwrap(), 1);
        let page = doc.GetPage(0).unwrap();
        let size = page.Size().unwrap();
        assert!((size.Width - 200.0 * 96.0 / 72.0).abs() < 1.0, "{}", size.Width);
        let (px, w, h) = render_page(&page, 400, 200).unwrap();
        // Larger on a scaled display, same aspect ratio.
        assert!(w >= 400 && (w as f64 / h as f64 - 2.0).abs() < 0.02, "{w}x{h}");
        // PDF y grows upwards: the box covers 10%..55% across, 20%..70% from the top.
        let at = |fx: f64, fy: f64| px[((fy * h as f64) as usize * w as usize + (fx * w as f64) as usize) * 4];
        assert!(at(0.25, 0.7) < 40, "inside the box is black");
        assert!(at(0.75, 0.7) > 215, "outside is white");
        assert!(at(0.25, 0.2) > 215, "above the box is white");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn fits_centred() {
        assert_eq!(fit(100.0, 200.0, 1000, 1000), (250, 0, 500, 1000));
        assert_eq!(fit(400.0, 100.0, 1000, 1000), (0, 375, 1000, 250));
        assert_eq!(to_bgr24(&[1, 2, 3, 255, 4, 5, 6, 255], 2, 1), vec![1, 2, 3, 4, 5, 6, 0, 0]);
    }
}
