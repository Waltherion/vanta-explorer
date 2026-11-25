//! Document previews: PDF pages, Word, Writer, Excel, Calc, PowerPoint and Impress documents
//! read by `office`,
//! ODF/OOXML embedded thumbnails for the rest, highlighted text.
//!
//! Documents are SDR by nature, so unlike images they do not go through the HDR layer, they
//! come back as plain raster (or a text layout) and draw in the egui layer.

use egui::text::LayoutJob;
use std::path::Path;
use std::sync::OnceLock;

pub const TEXT_LIMIT_BYTES: usize = 256 * 1024;
const TEXT_LIMIT_LINES: usize = 3000;

pub enum DocContent {
    Raster { img: egui::ColorImage, total_pages: u32 },
    Text(LayoutJob),
    /// An office document's own content: text, tables, pictures, or a sheet.
    Rich(Box<crate::office::RichDoc>),
}

/// Formats that render to a bitmap rather than to text. Gates BOTH the preview panel and
/// grid thumbnails, which is why SVG belongs here even though it is technically a text file:
/// as source it is noise, as a picture it is the point.
pub fn is_doc(path: &Path) -> bool {
    matches!(
        ext(path).as_str(),
        "pdf" | "odt" | "ods" | "odp" | "odg" | "docx" | "xlsx" | "pptx" | "svg" | "svgz"
    )
}

pub fn is_text(path: &Path) -> bool {
    const EXTS: &[&str] = &[
        "txt",
        "md",
        "rs",
        "py",
        "sh",
        "bash",
        "fish",
        "lua",
        "c",
        "h",
        "cpp",
        "hpp",
        "cc",
        "js",
        "ts",
        "jsx",
        "tsx",
        "css",
        "html",
        "xml",
        "svg",
        "json",
        "jsonc",
        "json5",
        "toml",
        "yaml",
        "yml",
        "ini",
        "conf",
        "cfg",
        "log",
        "csv",
        "tex",
        "sql",
        "go",
        "java",
        "kt",
        "rb",
        "php",
        "pl",
        "zig",
        "nim",
        "vim",
        "el",
        "clj",
        "glsl",
        "frag",
        "vert",
        "desktop",
        "service",
        "env",
        "gitignore",
        "dockerfile",
        "makefile",
        "cmake",
        "nix",
    ];
    let e = ext(path);
    if EXTS.contains(&e.as_str()) {
        return true;
    }
    // Extensionless but classically textual names (Makefile, LICENSE, README).
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| {
            let n = n.to_ascii_lowercase();
            n == "makefile" || n == "license" || n == "readme" || n == "pkgbuild"
        })
        .unwrap_or(false)
}

fn ext(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Render for the preview, on a worker thread; every branch here can block on IO. Office
/// documents show their content; when it cannot be read, the picture they carry of themselves,
/// and when there is none, why the content could not be read.
pub fn render(path: &Path, max_px: u32, page: u32) -> Result<DocContent, String> {
    let e = ext(path);
    if crate::office::handles(&e) {
        return match crate::office::render(path, &e, page) {
            Ok(doc) => Ok(DocContent::Rich(Box::new(doc))),
            Err(why) => thumbnail(path, max_px).map_err(|_| why),
        };
    }
    if e == "pdf" {
        return render_pdf(path, max_px, page).map(|(img, total_pages)| DocContent::Raster { img, total_pages });
    }
    thumbnail(path, max_px)
}

/// A picture of the file for the grid: the first PDF page, an SVG drawn, an office document by
/// the thumbnail it carries; text highlighted.
pub fn thumbnail(path: &Path, max_px: u32) -> Result<DocContent, String> {
    match ext(path).as_str() {
        "pdf" => render_pdf(path, max_px, 0).map(|(img, total_pages)| DocContent::Raster { img, total_pages }),
        "odt" | "ods" | "odp" | "odg" => {
            zip_thumbnail(path, "Thumbnails/thumbnail.png").map(|img| DocContent::Raster { img, total_pages: 1 })
        }
        "docx" | "xlsx" | "pptx" => ooxml_thumbnail(path).map(|img| DocContent::Raster { img, total_pages: 1 }),
        // A broken or unsupported SVG falls back to its source, which is still useful
        // and is what every SVG showed before this arm existed.
        "svg" | "svgz" => render_svg(path, max_px)
            .map(|img| DocContent::Raster { img, total_pages: 1 })
            .or_else(|_| highlight_text(path).map(DocContent::Text)),
        _ => highlight_text(path).map(DocContent::Text),
    }
}

/// SVG rasterised with resvg, on a checkerboard.
///
/// Vectors have no background of their own, and artwork comes both dark-on-transparent and
/// light-on-transparent, against any flat colour one of those is invisible. A checkerboard
/// shows both, and shows honestly where the file is actually transparent. (PDFs get white
/// instead, because a page really is white.)
///
/// Unlike the icon rasteriser this keeps the real aspect ratio, and it scales UP to fill the
/// preview: that is the entire point of a vector.
fn render_svg(path: &Path, max_px: u32) -> Result<egui::ColorImage, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let opts = resvg::usvg::Options {
        // Lets an SVG reference sibling files (fonts, embedded images) the way it expects.
        resources_dir: path.parent().map(|p| p.to_path_buf()),
        ..Default::default()
    };
    // from_data, not from_str: it also accepts gzipped .svgz.
    let tree = resvg::usvg::Tree::from_data(&data, &opts).map_err(|e| e.to_string())?;

    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 {
        return Err("svg has no intrinsic size".into());
    }
    let scale = (max_px as f32 / size.width()).min(max_px as f32 / size.height());
    let w = ((size.width() * scale).round() as u32).max(1);
    let h = ((size.height() * scale).round() as u32).max(1);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or("pixmap allocation failed")?;

    const SQUARE: usize = 12;
    for (i, px) in pixmap.pixels_mut().iter_mut().enumerate() {
        let (x, y) = (i % w as usize, i / w as usize);
        let v = if (x / SQUARE + y / SQUARE).is_multiple_of(2) { 0x9a } else { 0x6e };
        // Opaque, so premultiplied and straight agree.
        *px = resvg::tiny_skia::PremultipliedColorU8::from_rgba(v, v, v, 255)
            .ok_or("checkerboard colour")?;
    }

    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(egui::ColorImage::from_rgba_premultiplied(
        [w as usize, h as usize],
        pixmap.data(),
    ))
}

/// First PDF page through poppler, painted on white, PDFs assume a white page, and our
/// windows are dark, so transparency would render them unreadable.
fn render_pdf(path: &Path, max_px: u32, page_idx: u32) -> Result<(egui::ColorImage, u32), String> {
    let uri = glib::filename_to_uri(path, None).map_err(|e| e.to_string())?;
    let doc = poppler::Document::from_file(&uri, None).map_err(|e| e.to_string())?;
    let n_pages = doc.n_pages() as u32;
    let p_idx = page_idx.min(n_pages.saturating_sub(1)) as i32;
    let page = doc.page(p_idx).ok_or("empty PDF")?;
    let (pw, ph) = page.size();
    if pw <= 0.0 || ph <= 0.0 {
        return Err("degenerate page size".into());
    }
    let scale = (max_px as f64 / pw.max(ph)).min(4.0);
    let (w, h) = ((pw * scale).ceil() as i32, (ph * scale).ceil() as i32);

    let surface =
        cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).map_err(|e| e.to_string())?;
    {
        let ctx = cairo::Context::new(&surface).map_err(|e| e.to_string())?;
        ctx.set_source_rgb(1.0, 1.0, 1.0);
        ctx.paint().map_err(|e| e.to_string())?;
        ctx.scale(scale, scale);
        page.render(&ctx);
    }
    let mut surface = surface;
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().map_err(|e| e.to_string())?;

    // Cairo ARGB32 is PREMULTIPLIED BGRA in native endianness; we painted opaque white
    // underneath, so alpha is 255 everywhere and a channel swap is all that is needed.
    let mut rgba = vec![0u8; w as usize * h as usize * 4];
    for y in 0..h as usize {
        let row = &data[y * stride..];
        for x in 0..w as usize {
            let i = x * 4;
            let o = (y * w as usize + x) * 4;
            rgba[o] = row[i + 2];
            rgba[o + 1] = row[i + 1];
            rgba[o + 2] = row[i];
            rgba[o + 3] = 255;
        }
    }
    Ok((egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        &rgba,
    ), n_pages))
}

/// ODF files carry a ready-made PNG at a fixed path inside the zip.
fn zip_thumbnail(path: &Path, entry: &str) -> Result<egui::ColorImage, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut z = archive
        .by_name(entry)
        .map_err(|_| format!("no embedded thumbnail ({entry})"))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut z, &mut bytes).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [img.width() as usize, img.height() as usize],
        img.as_raw(),
    ))
}

/// OOXML thumbnails are optional and the name varies; try the common ones.
fn ooxml_thumbnail(path: &Path) -> Result<egui::ColorImage, String> {
    for entry in [
        "docProps/thumbnail.jpeg",
        "docProps/thumbnail.png",
        "docProps/thumbnail.jpg",
    ] {
        if let Ok(img) = zip_thumbnail(path, entry) {
            return Ok(img);
        }
    }
    Err("no embedded thumbnail (Office writes one only when asked to)".into())
}

/// The syntax to borrow for config formats syntect has none of: JSON with comments reads as JSON,
/// and the `key = value` files with `#` comments (TOML, INI, .conf, .desktop) read well enough
/// as shell, which colors their comments, strings and numbers.
fn borrowed_syntax(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "jsonc" | "json5" => "json",
        "conf" | "cfg" | "ini" | "toml" | "desktop" | "service" | "env" | "rules" | "properties" => "sh",
        _ => return None,
    })
}

fn syntaxes() -> &'static (syntect::parsing::SyntaxSet, syntect::highlighting::Theme) {
    static SETS: OnceLock<(syntect::parsing::SyntaxSet, syntect::highlighting::Theme)> =
        OnceLock::new();
    SETS.get_or_init(|| {
        let ss = syntect::parsing::SyntaxSet::load_defaults_newlines();
        // base16-eighties reads well on dark backgrounds; per-theme mapping can come later.
        let theme = syntect::highlighting::ThemeSet::load_defaults()
            .themes
            .remove("base16-eighties.dark")
            .expect("bundled theme");
        (ss, theme)
    })
}

/// Capped, syntax-highlighted text as an egui LayoutJob (plain data, safe to build off-thread).
fn highlight_text(path: &Path) -> Result<LayoutJob, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let truncated = bytes.len() > TEXT_LIMIT_BYTES;
    let slice = &bytes[..bytes.len().min(TEXT_LIMIT_BYTES)];
    if slice.iter().take(4096).filter(|b| **b == 0).count() > 0 {
        return Err("binary file".into());
    }
    let text = String::from_utf8_lossy(slice);
    let mut job = highlight(path, &text, TEXT_LIMIT_LINES);
    if truncated {
        job.append(
            "\n… (truncated)",
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::monospace(12.0),
                color: egui::Color32::GRAY,
                ..Default::default()
            },
        );
    }
    Ok(job)
}

/// `text` highlighted as the kind of file `path` is (by its extension, else its first line), up to
/// `max_lines` lines. Shared by the preview and the editor.
pub fn highlight(path: &Path, text: &str, max_lines: usize) -> LayoutJob {
    use syntect::easy::HighlightLines;
    use syntect::util::LinesWithEndings;

    let (ss, theme) = syntaxes();
    let syntax = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| ss.find_syntax_by_extension(e).or_else(|| ss.find_syntax_by_extension(borrowed_syntax(e)?)))
        .or_else(|| ss.find_syntax_by_first_line(text.lines().next().unwrap_or("")))
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let mut hl = HighlightLines::new(syntax, theme);
    let mut job = LayoutJob::default();
    let font = egui::FontId::monospace(12.0);

    for (n, line) in LinesWithEndings::from(text).enumerate() {
        if n >= max_lines {
            break;
        }
        let Ok(regions) = hl.highlight_line(line, ss) else {
            job.append(line, 0.0, egui::TextFormat { font_id: font.clone(), color: egui::Color32::LIGHT_GRAY, ..Default::default() });
            continue;
        };
        for (style, span) in regions {
            let c = style.foreground;
            job.append(
                span,
                0.0,
                egui::TextFormat {
                    font_id: font.clone(),
                    color: egui::Color32::from_rgb(c.r, c.g, c.b),
                    ..Default::default()
                },
            );
        }
    }
    job
}
