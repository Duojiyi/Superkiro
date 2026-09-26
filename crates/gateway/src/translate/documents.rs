//! Files attached in Kiro's chat.
//!
//! Kiro sends PDF, CSV, DOC, DOCX, XLS, XLSX, HTML, TXT and MD attachments as the message's
//! `documents`. A PDF reaches a model that reads images as a file (Anthropic: a `document`
//! block; OpenAI: a `file` part); a text format as its text. The others no upstream takes.

use kiro_wire::requests::conversation::KiroDocument;

/// The largest attachment forwarded, decoded. Providers cap a whole request near 32 MB.
pub const MAX_DOCUMENT_BYTES: usize = 20 * 1024 * 1024;

/// What a provider charges for a PDF page, read as text and as an image of the page.
const PDF_PAGE_TOKENS: u64 = 2_000;

/// How one format reaches a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Pdf,
    Text,
    Unsupported,
}

/// The text formats sent as their text: Kiro's, and the plain-text files a customer may
/// attach under another name.
const TEXT_FORMATS: &[&str] = &[
    "txt", "md", "markdown", "csv", "tsv", "html", "htm", "json", "jsonl", "xml", "yaml", "yml",
    "toml", "ini", "log", "sql", "js", "jsx", "ts", "tsx", "py", "rs", "go", "java", "kt", "c",
    "h", "cc", "cpp", "hpp", "cs", "rb", "php", "swift", "sh", "bash", "ps1", "css", "scss", "vue",
    "svelte", "lua", "dart", "scala", "r",
];

pub fn document_kind(format: &str) -> DocumentKind {
    let format = format.trim().trim_start_matches('.').to_ascii_lowercase();
    if format == "pdf" {
        DocumentKind::Pdf
    } else if TEXT_FORMATS.contains(&format.as_str()) {
        DocumentKind::Text
    } else {
        DocumentKind::Unsupported
    }
}

/// The file's name as the customer knows it: Kiro sends it without its extension.
pub fn file_name(document: &KiroDocument) -> String {
    let format = document.format.trim().trim_start_matches('.');
    let name = document.name.trim();
    let name = if name.is_empty() { "document" } else { name };
    if format.is_empty() || name.to_ascii_lowercase().ends_with(&format!(".{format}")) {
        name.to_string()
    } else {
        format!("{name}.{format}")
    }
}

pub fn decode(document: &KiroDocument) -> Option<Vec<u8>> {
    base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        document.source.bytes.trim(),
    )
    .ok()
}

/// A text attachment's text. Invalid UTF-8 is replaced rather than refused: an encoding
/// slip in a log should not cost the customer the whole file.
pub fn text_of(document: &KiroDocument) -> Option<String> {
    decode(document).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Why the current message's attachment cannot be sent, as the reason Kiro shows a
/// document refusal for, and the message it shows. `reads_pdf` is whether the model reads
/// images, which a PDF's pages are read as.
pub fn refusal(documents: &[KiroDocument], reads_pdf: bool) -> Option<(&'static str, String)> {
    documents.iter().find_map(|document| {
        let name = file_name(document);
        let Some(bytes) = decode(document) else {
            return Some((
                "DOCUMENT_MODEL_NOT_SUPPORTED",
                format!("无法读取附件 {name}：文件数据已损坏，请重新添加后再发送。"),
            ));
        };
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Some((
                "DOCUMENT_SIZE_EXCEEDED",
                format!(
                    "附件 {name} 超过 {} MB 的上限，请压缩或拆分后再发送。",
                    MAX_DOCUMENT_BYTES / (1024 * 1024)
                ),
            ));
        }
        match document_kind(&document.format) {
            DocumentKind::Text => None,
            DocumentKind::Pdf if reads_pdf => None,
            DocumentKind::Pdf => Some((
                "DOCUMENT_MODEL_NOT_SUPPORTED",
                format!(
                    "当前模型不能读取 PDF 文件（{name}）：请换用支持图片的模型，或把内容以文本发送。"
                ),
            )),
            DocumentKind::Unsupported => Some((
                "DOCUMENT_MODEL_NOT_SUPPORTED",
                format!(
                    "暂不支持读取 .{} 文件（{name}）：请另存为 PDF、TXT 或 Markdown 后再发送。",
                    document.format.trim().trim_start_matches('.').to_ascii_lowercase()
                ),
            )),
        }
    })
}

/// How an attachment reaches the model, as a provider-neutral content part: a PDF as an
/// OpenAI `file` part (the Anthropic adapter makes it a `document` block), a text format as
/// its text, anything else as a note that it was not sent. The current message's
/// attachments were checked by [`refusal`]; an earlier message's that cannot be read become
/// notes, so one old attachment never makes every later turn fail.
pub fn content_part(document: &KiroDocument, reads_pdf: bool) -> serde_json::Value {
    let name = file_name(document);
    let note = |why: &str| {
        serde_json::json!({
            "type": "text",
            "text": format!("[附件 {name} 未发送 / attachment not sent: {why}]")
        })
    };
    match document_kind(&document.format) {
        DocumentKind::Text => match text_of(document) {
            Some(text) => serde_json::json!({
                "type": "text",
                "text": format!("<document name=\"{name}\">\n{text}\n</document>")
            }),
            None => note("文件数据无法读取 / the file data cannot be read"),
        },
        DocumentKind::Pdf if reads_pdf => serde_json::json!({
            "type": "file",
            "file": {
                "filename": name,
                "file_data": format!("data:application/pdf;base64,{}", document.source.bytes.trim()),
            }
        }),
        DocumentKind::Pdf => note("当前模型不能读取 PDF / this model does not read PDF files"),
        DocumentKind::Unsupported => {
            note("当前模型不能读取这种格式 / this format cannot be read by the model")
        }
    }
}

/// Estimated input tokens of an attachment in quarter-token units, as the usage estimate
/// counts: a PDF by its pages, a text format by its text, anything else as its note.
pub fn estimate_units(format: &str, base64: &str) -> u64 {
    let decoded = || {
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, base64.trim())
            .unwrap_or_default()
    };
    match document_kind(format) {
        DocumentKind::Pdf => pdf_tokens(&decoded()) * 4,
        DocumentKind::Text => {
            crate::usage_estimate::token_units(&String::from_utf8_lossy(&decoded()))
        }
        DocumentKind::Unsupported => 64,
    }
}

/// A PDF's cost: its pages when its page objects are readable, else one page per 50 KB, the
/// size of a typical text page.
pub fn pdf_tokens(pdf: &[u8]) -> u64 {
    let pages = pdf_page_count(pdf).unwrap_or_else(|| pdf.len().div_ceil(50 * 1024).max(1));
    pages as u64 * PDF_PAGE_TOKENS
}

/// Page objects written in the clear (`/Type /Page`, not `/Pages`). A PDF that keeps them in
/// compressed object streams shows none.
fn pdf_page_count(pdf: &[u8]) -> Option<usize> {
    let mut pages = 0;
    let mut rest = pdf;
    while let Some(at) = find(rest, b"/Type") {
        rest = &rest[at + 5..];
        let value = rest.trim_ascii_start();
        if let Some(after) = value.strip_prefix(b"/Page") {
            if !after.first().is_some_and(u8::is_ascii_alphanumeric) {
                pages += 1;
            }
        }
    }
    (pages > 0).then_some(pages)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiro_wire::requests::conversation::KiroDocumentSource;

    fn document(name: &str, format: &str, bytes: &[u8]) -> KiroDocument {
        KiroDocument {
            name: name.into(),
            format: format.into(),
            source: KiroDocumentSource {
                bytes: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
            },
        }
    }

    #[test]
    fn formats_are_read_as_pdf_text_or_not_at_all() {
        assert_eq!(document_kind("pdf"), DocumentKind::Pdf);
        for text in ["md", "TXT", "csv", "html", "json", "rs"] {
            assert_eq!(document_kind(text), DocumentKind::Text, "{text}");
        }
        for other in ["docx", "doc", "xlsx", "xls", "zip", ""] {
            assert_eq!(document_kind(other), DocumentKind::Unsupported, "{other}");
        }
        assert_eq!(file_name(&document("spec", "md", b"")), "spec.md");
        assert_eq!(file_name(&document("spec.md", "md", b"")), "spec.md");
    }

    #[test]
    fn only_what_no_path_can_take_is_refused() {
        let md = document("notes", "md", b"# hi");
        let pdf = document("paper", "pdf", b"%PDF-1.4");
        let docx = document("report", "docx", b"PK");
        assert_eq!(refusal(&[md.clone(), pdf.clone()], true), None);
        let (reason, message) = refusal(&[pdf], false).unwrap();
        assert_eq!(reason, "DOCUMENT_MODEL_NOT_SUPPORTED");
        assert!(message.contains("paper.pdf"), "{message}");
        let (reason, message) = refusal(&[md, docx], true).unwrap();
        assert_eq!(reason, "DOCUMENT_MODEL_NOT_SUPPORTED");
        assert!(message.contains(".docx"), "{message}");
    }

    #[test]
    fn text_is_sent_as_text_and_a_pdf_as_a_file() {
        let part = content_part(&document("notes", "md", "# 标题".as_bytes()), true);
        assert_eq!(part["type"], "text");
        assert!(part["text"].as_str().unwrap().contains("# 标题"));
        let part = content_part(&document("paper", "pdf", b"%PDF-1.4"), true);
        assert_eq!(part["file"]["filename"], "paper.pdf");
        assert!(part["file"]["file_data"]
            .as_str()
            .unwrap()
            .starts_with("data:application/pdf;base64,"));
        // A model that cannot read it gets a note, never the bytes.
        let part = content_part(&document("paper", "pdf", b"%PDF-1.4"), false);
        assert_eq!(part["type"], "text");
        assert!(!part.to_string().contains("JVBER"));
    }

    #[test]
    fn a_pdf_costs_its_pages() {
        let pdf = b"%PDF-1.4 1 0 obj <</Type /Pages /Count 2>> 2 0 obj <</Type /Page>> \
                    3 0 obj <</Type/Page /Parent 1 0 R>>";
        assert_eq!(pdf_tokens(pdf), 2 * PDF_PAGE_TOKENS);
        // Pages in compressed object streams: estimated by size.
        assert_eq!(pdf_tokens(&vec![b'x'; 120 * 1024]), 3 * PDF_PAGE_TOKENS);
    }
}
