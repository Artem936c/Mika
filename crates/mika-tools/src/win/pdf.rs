//! Text of PDF pages with what Windows already has: the PDF renderer draws a
//! page and the text recognizer reads it. Works for scans too, needs no library.

use std::path::Path;

use windows::core::HSTRING;
use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::StorageFile;
use windows::Storage::Streams::InMemoryRandomAccessStream;
use windows::Win32::System::Com::COINIT_MULTITHREADED;

/// Picture width a page is drawn at for recognition, pixels.
const PAGE_WIDTH: f32 = 1700.0;

/// Text of the first `max_pages` pages, one string per page.
pub fn pages_text(path: &Path, max_pages: u32) -> Result<Vec<String>, String> {
    let path = HSTRING::from(path.as_os_str());
    super::with_com(COINIT_MULTITHREADED, move || {
        read(&path, max_pages).map_err(|e| format!("PDF не прочитан: {}", e.message()))?
    })
}

fn read(path: &HSTRING, max_pages: u32) -> windows::core::Result<Result<Vec<String>, String>> {
    let engine = match super::screen::ocr_engine() {
        Ok(e) => e,
        Err(e) => return Ok(Err(e)),
    };
    let limit = OcrEngine::MaxImageDimension()? as f32;
    let file = StorageFile::GetFileFromPathAsync(path)?.join()?;
    let doc = PdfDocument::LoadFromFileAsync(&file)?.join()?;
    if doc.IsPasswordProtected()? {
        return Ok(Err("PDF защищён паролем".into()));
    }
    let count = doc.PageCount()?.min(max_pages);
    Ok(Ok(each_page(count, |i| {
        page_text(&doc, &engine, limit, i)
    })?))
}

/// Text of every page. A page that fails comes out empty, so one broken page
/// does not cost the whole document and the numbers of the rest stay right.
/// Fails only when no page could be read.
fn each_page<E: std::fmt::Display>(
    count: u32,
    mut page: impl FnMut(u32) -> Result<String, E>,
) -> Result<Vec<String>, E> {
    let mut pages = Vec::with_capacity(count as usize);
    let mut failed = None;
    let mut read_any = false;
    for i in 0..count {
        match page(i) {
            Ok(text) => {
                read_any = true;
                pages.push(text);
            }
            Err(e) => {
                log::debug!("PDF page not read: page={} error={e}", i + 1);
                failed.get_or_insert(e);
                pages.push(String::new());
            }
        }
    }
    match failed {
        Some(e) if !read_any => Err(e),
        _ => Ok(pages),
    }
}

fn page_text(
    doc: &PdfDocument,
    engine: &OcrEngine,
    limit: f32,
    i: u32,
) -> windows::core::Result<String> {
    let page = doc.GetPage(i)?;
    let size = page.Size()?;
    // The longer side must fit what the recognizer takes.
    let mut width = PAGE_WIDTH.min(limit);
    if size.Width > 0.0 && size.Height > 0.0 {
        let height = width * size.Height / size.Width;
        if height > limit {
            width = limit * size.Width / size.Height;
        }
    }
    let options = PdfPageRenderOptions::new()?;
    options.SetDestinationWidth(width.max(100.0) as u32)?;
    let stream = InMemoryRandomAccessStream::new()?;
    page.RenderWithOptionsToStreamAsync(&stream, &options)?
        .join()?;
    stream.Seek(0)?;
    let bitmap = BitmapDecoder::CreateAsync(&stream)?
        .join()?
        .GetSoftwareBitmapAsync()?
        .join()?;
    let result = engine.RecognizeAsync(&bitmap)?.join()?;
    let mut lines = Vec::new();
    for line in result.Lines()? {
        lines.push(line.Text()?.to_string());
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::each_page;

    #[test]
    fn a_broken_page_does_not_cost_the_document() {
        let pages = each_page(3, |i| {
            if i == 1 {
                Err("render failed")
            } else {
                Ok(format!("page {}", i + 1))
            }
        })
        .unwrap();
        assert_eq!(pages, ["page 1", "", "page 3"]);
        assert_eq!(
            each_page(2, |_| Err::<String, _>("no recognizer")),
            Err("no recognizer")
        );
        assert_eq!(each_page(0, |_| Err::<String, _>("x")), Ok(Vec::new()));
    }

    /// Reads a real PDF: `MIKA_PDF` names it.
    #[test]
    #[ignore = "needs a PDF file and the Windows recognizer"]
    fn live_pdf_pages() {
        let path = std::env::var("MIKA_PDF").expect("MIKA_PDF");
        let pages = super::pages_text(std::path::Path::new(&path), 3).unwrap();
        assert!(!pages.is_empty());
        println!("{}", pages.join("\n----\n"));
    }
}
