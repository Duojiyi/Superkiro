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
        let kind = document_kind(&document.format);
        if kind == DocumentKind::Pdf {
            // A model reads no page of a password-protected PDF.
            if pdf_encrypted(&bytes) {
                return Some((
                    "DOCUMENT_PASSWORD_PROTECTED",
                    format!("附件 {name} 已加密（有密码保护），模型无法读取。请去除密码后再发送。"),
                ));
            }
            if let Some(reason) = refused_reason(document) {
                return Some((
                    reason,
                    format!(
                        "附件 {name} 此前已被上游模型拒绝读取（{}），请换一个文件，或把内容以文本发送。",
                        why_refused(reason)
                    ),
                ));
            }
        }
        match kind {
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
    let kind = document_kind(&document.format);
    if kind == DocumentKind::Pdf {
        // An upstream refused it once: resent, it failed every later turn of the
        // conversation. An encrypted one, refused when it was attached, no model reads.
        if refused_reason(document).is_some() {
            return note(REFUSED_NOTE);
        }
        if reads_pdf && decode(document).is_some_and(|bytes| pdf_encrypted(&bytes)) {
            return note(ENCRYPTED_NOTE);
        }
    }
    match kind {
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

/// A PDF's cost: its pages when they can be counted, else one page per 50 KB, the size of a
/// typical text page.
pub fn pdf_tokens(pdf: &[u8]) -> u64 {
    let pages = pdf_page_count(pdf).unwrap_or_else(|| pdf.len().div_ceil(50 * 1024).max(1));
    pages as u64 * PDF_PAGE_TOKENS
}

/// A PDF's pages: the count its page tree gives, else its page objects, looked for in the
/// clear first and then inside its compressed object streams, where writers since PDF 1.5
/// keep both. None when neither can be found.
pub fn pdf_page_count(pdf: &[u8]) -> Option<usize> {
    count_pages(pdf).or_else(|| count_pages(&object_streams(pdf)))
}

fn count_pages(bytes: &[u8]) -> Option<usize> {
    page_tree_count(bytes).or_else(|| page_objects(bytes))
}

/// `/Count` of the page tree: each `/Type /Pages` node counts the pages under it, so the
/// root's is the largest. An incremental update can leave old page objects behind; the
/// tree counts the pages the document has.
fn page_tree_count(bytes: &[u8]) -> Option<usize> {
    type_positions(bytes, b"/Pages")
        .filter_map(|at| own_count(&bytes[enclosing_dictionary(bytes, at)?..]))
        .max()
        .filter(|&pages| pages > 0)
}

/// How far a page tree node's dictionary is followed, either way from its `/Type`.
const MAX_NODE_BYTES: usize = 64 * 1024;

/// Where the dictionary holding the entry at `at` starts: the `<<` it is inside, past any
/// dictionary before it that closes first (an inherited `/Resources`, say).
fn enclosing_dictionary(bytes: &[u8], at: usize) -> Option<usize> {
    let floor = at.saturating_sub(MAX_NODE_BYTES);
    let mut depth = 0usize;
    let mut end = at;
    while end >= floor + 2 {
        match &bytes[end - 2..end] {
            b">>" => {
                depth += 1;
                end -= 2;
            }
            b"<<" if depth == 0 => return Some(end - 2),
            b"<<" => {
                depth -= 1;
                end -= 2;
            }
            _ => end -= 1,
        }
    }
    None
}

/// The `/Count` of the dictionary `dictionary` starts with: its own entry, not one of a
/// dictionary inside it, nor of an object after it (an outline counts its items).
fn own_count(dictionary: &[u8]) -> Option<usize> {
    let mut depth = 0usize;
    let mut at = 0;
    while at + 1 < dictionary.len().min(MAX_NODE_BYTES) {
        match &dictionary[at..at + 2] {
            b"<<" => {
                depth += 1;
                at += 2;
            }
            b">>" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return None;
                }
                at += 2;
            }
            _ if depth == 1 && dictionary[at..].starts_with(b"/Count") => {
                let digits: Vec<u8> = dictionary[at + 6..]
                    .trim_ascii_start()
                    .iter()
                    .copied()
                    .take_while(u8::is_ascii_digit)
                    .collect();
                return std::str::from_utf8(&digits).ok()?.parse().ok();
            }
            _ => at += 1,
        }
    }
    None
}

/// Page objects (`/Type /Page`, not `/Pages`).
fn page_objects(bytes: &[u8]) -> Option<usize> {
    let pages = type_positions(bytes, b"/Page").count();
    (pages > 0).then_some(pages)
}

/// Where a `/Type` entry names `value` (and not a longer name that starts with it).
fn type_positions<'a>(bytes: &'a [u8], value: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    let mut offset = 0;
    std::iter::from_fn(move || loop {
        let at = offset + find(&bytes[offset..], b"/Type")?;
        offset = at + 5;
        let rest = bytes[offset..].trim_ascii_start();
        if let Some(after) = rest.strip_prefix(value) {
            if !after.first().is_some_and(u8::is_ascii_alphanumeric) {
                return Some(at);
            }
        }
    })
}

/// The most of a PDF's object streams inflated to count its pages.
const MAX_INFLATED_BYTES: u64 = 16 * 1024 * 1024;

/// The contents of a PDF's Flate-compressed object streams (`/Type /ObjStm`), one after the
/// other, at most [`MAX_INFLATED_BYTES`].
fn object_streams(pdf: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    for at in type_positions(pdf, b"/ObjStm").collect::<Vec<_>>() {
        let dictionary_start = pdf[..at]
            .windows(2)
            .rposition(|window| window == b"<<")
            .unwrap_or(at);
        let Some(keyword) = find(&pdf[at..], b"stream") else {
            continue;
        };
        let header = &pdf[dictionary_start..at + keyword];
        if find(header, b"/FlateDecode").is_none() {
            continue;
        }
        let mut data = &pdf[at + keyword + 6..];
        data = data.strip_prefix(b"\r").unwrap_or(data);
        data = data.strip_prefix(b"\n").unwrap_or(data);
        let data = find(data, b"endstream").map_or(data, |end| &data[..end]);
        let room = MAX_INFLATED_BYTES.saturating_sub(out.len() as u64);
        if room == 0 {
            break;
        }
        let _ = flate2::read::ZlibDecoder::new(data)
            .take(room)
            .read_to_end(&mut out);
        out.push(b'\n');
    }
    out
}

/// Whether a PDF is encrypted: its trailer, or the cross-reference stream that stands for
/// it, has an `/Encrypt` entry, a dictionary or a reference to one. The name in a page's
/// text ("the /Encrypt entry") is neither.
pub fn pdf_encrypted(pdf: &[u8]) -> bool {
    let mut offset = 0;
    while let Some(found) = find(&pdf[offset..], b"/Encrypt") {
        offset += found + 8;
        let value = pdf[offset..].trim_ascii_start();
        if value.starts_with(b"<<") || is_reference(value) {
            return true;
        }
    }
    false
}

/// Whether `value` starts with an indirect reference: `12 0 R`.
fn is_reference(value: &[u8]) -> bool {
    let number = |bytes: &[u8]| -> Option<usize> {
        let digits = bytes
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        (digits > 0).then_some(digits)
    };
    let Some(object) = number(value) else {
        return false;
    };
    let rest = &value[object..];
    let generation_at = rest.len() - rest.trim_ascii_start().len();
    if generation_at == 0 {
        return false;
    }
    let rest = &rest[generation_at..];
    let Some(generation) = number(rest) else {
        return false;
    };
    let rest = &rest[generation..];
    let r_at = rest.len() - rest.trim_ascii_start().len();
    r_at > 0
        && rest[r_at..].starts_with(b"R")
        && !rest[r_at + 1..]
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
}

/// The most pages a model reads from the PDFs of one request: Anthropic's 100 on a model
/// with a 200K-token window. A larger window is given more before the gateway refuses
/// ahead of the upstream, which still says no in its own words if it must.
pub fn max_pdf_pages(input_limit: u64) -> usize {
    if input_limit > 200_000 {
        600
    } else {
        100
    }
}

/// An attachment the model cannot take however short the rest of the conversation is: a
/// PDF with more pages than it reads, or an attachment whose text or pages alone exceed its
/// input. Refused as the attachment it is: refused as a context overflow, Kiro compacted
/// the conversation, which leaves the attachment in place, and it failed again. A PDF whose
/// pages cannot be counted is left to the upstream.
pub fn too_large_for_model(
    documents: &[KiroDocument],
    input_limit: u64,
) -> Option<(&'static str, String)> {
    documents.iter().find_map(|document| {
        let name = file_name(document);
        let tokens = match document_kind(&document.format) {
            DocumentKind::Pdf => {
                let pages = pdf_page_count(&decode(document)?)?;
                let most = max_pdf_pages(input_limit);
                if pages > most {
                    return Some((
                        "DOCUMENT_MAXIMUM_PAGES_EXCEEDED",
                        format!("附件 {name} 共 {pages} 页，超过该模型一次最多读取 {most} 页的上限，请拆分后再发送。"),
                    ));
                }
                pages as u64 * PDF_PAGE_TOKENS
            }
            DocumentKind::Text => crate::usage_estimate::tokens_from_units(
                crate::usage_estimate::token_units(&text_of(document)?),
            ),
            DocumentKind::Unsupported => return None,
        };
        (tokens > input_limit).then(|| {
            (
                "DOCUMENT_SIZE_EXCEEDED",
                format!("附件 {name} 约 {tokens} 个 token，单个文件就超过了该模型 {input_limit} 个 token 的输入上限，请拆分或摘录后再发送。"),
            )
        })
    })
}

/// What a PDF is replaced by for an upstream that reads none (PROVIDER_NO_DOCUMENTS).
pub const NO_PDF_NOTE: &str = "该上游不能读取 PDF / this upstream does not read PDF files";

/// What a PDF an upstream refused is replaced by in the messages after.
const REFUSED_NOTE: &str = "上游模型此前拒绝读取这个文件 / an upstream refused this file";

/// What an encrypted PDF of an earlier message is replaced by.
const ENCRYPTED_NOTE: &str = "文件已加密，模型无法读取 / the file is password-protected";

/// Every PDF in `messages` replaced by a note that says why, for an upstream that reads none.
pub fn pdfs_as_notes(messages: &mut [crate::provider::ChatMessage], why: &str) {
    for message in messages {
        let serde_json::Value::Array(parts) = &mut message.content else {
            continue;
        };
        for part in parts.iter_mut() {
            let is_pdf = part["type"] == "file"
                && part["file"]["file_data"]
                    .as_str()
                    .is_some_and(|data| data.starts_with("data:application/pdf"));
            if is_pdf {
                let name = part["file"]["filename"]
                    .as_str()
                    .unwrap_or("document.pdf")
                    .to_string();
                *part = serde_json::json!({
                    "type": "text",
                    "text": format!("[附件 {name} 未发送 / attachment not sent: {why}]")
                });
            }
        }
    }
}

/// Which Kiro document reason an upstream's refusal of a request with PDFs is, when it is
/// about them: its words name a PDF or a document.
pub fn upstream_refusal_reason(status: u16, body: &str) -> Option<&'static str> {
    if !matches!(status, 400 | 422) {
        return None;
    }
    let body = body.to_ascii_lowercase();
    if !(body.contains("pdf") || body.contains("document")) {
        return None;
    }
    Some(if body.contains("password") || body.contains("encrypt") {
        "DOCUMENT_PASSWORD_PROTECTED"
    } else if body.contains("page") {
        "DOCUMENT_MAXIMUM_PAGES_EXCEEDED"
    } else if body.contains("too large") || body.contains("size") {
        "DOCUMENT_SIZE_EXCEEDED"
    } else {
        "DOCUMENT_MODEL_NOT_SUPPORTED"
    })
}

/// The PDFs one request sends, by name and key: the current message's apart, since an
/// upstream refusing one is refusing the new attachment first.
#[derive(Debug, Default)]
pub struct RequestPdfs {
    current: Vec<(String, [u8; 32])>,
    earlier: Vec<(String, [u8; 32])>,
}

impl RequestPdfs {
    pub fn of(
        request: &kiro_wire::requests::conversation::GenerateAssistantResponseRequest,
    ) -> Self {
        use kiro_wire::requests::conversation::Message;
        let pdfs = |documents: &[KiroDocument]| -> Vec<(String, [u8; 32])> {
            documents
                .iter()
                .filter(|document| document_kind(&document.format) == DocumentKind::Pdf)
                .map(|document| (file_name(document), refusal_key(document)))
                .collect()
        };
        let state = &request.conversation_state;
        Self {
            current: pdfs(&state.current_message.user_input_message.documents),
            earlier: state
                .history
                .iter()
                .filter_map(|message| match message {
                    Message::User(user) => Some(pdfs(&user.user_input_message.documents)),
                    Message::Assistant(_) => None,
                })
                .flatten()
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.current.is_empty() && self.earlier.is_empty()
    }

    /// The current message's first PDF, by name.
    pub fn current(&self) -> Option<&str> {
        self.current.first().map(|(name, _)| name.as_str())
    }

    /// An upstream refused this request for `reason`, about its PDFs: the current
    /// message's when it has any, else the earlier ones. They are remembered, so that sent
    /// again from an earlier message each is a note and the conversation can go on, and
    /// the refusal names them.
    pub fn refused(&self, reason: &'static str) -> String {
        let blamed = if self.current.is_empty() {
            &self.earlier
        } else {
            &self.current
        };
        for (_, key) in blamed {
            remember_refused_key(*key, reason);
        }
        let names: Vec<&str> = blamed.iter().map(|(name, _)| name.as_str()).collect();
        format!(
            "上游模型服务不能读取附件 {}（{}）。之后这个文件会以一条说明代替，可以继续对话；需要其中的内容时，请换一个文件或把内容以文本发送。",
            names.join("、"),
            why_refused(reason)
        )
    }
}

/// A document reason in words.
pub fn why_refused(reason: &str) -> &'static str {
    match reason {
        "DOCUMENT_PASSWORD_PROTECTED" => "文件已加密",
        "DOCUMENT_MAXIMUM_PAGES_EXCEEDED" => "页数超过了该模型的上限",
        "DOCUMENT_SIZE_EXCEEDED" => "文件过大",
        _ => "文件无法被解析",
    }
}

/// How long an upstream's refusal of a file is remembered, and for how many files.
const REFUSAL_MEMORY_SECS: u64 = 7 * 86_400;
const REFUSAL_MEMORY_FILES: usize = 4_096;

type RefusedFiles = std::collections::HashMap<[u8; 32], (&'static str, u64)>;

/// PDFs an upstream refused, by the SHA-256 of their data: the reason, and until when.
static REFUSED: std::sync::LazyLock<std::sync::Mutex<RefusedFiles>> =
    std::sync::LazyLock::new(Default::default);

fn refusal_key(document: &KiroDocument) -> [u8; 32] {
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        document.source.bytes.trim().as_bytes(),
    );
    let mut key = [0u8; 32];
    key.copy_from_slice(digest.as_ref());
    key
}

/// Remember that an upstream refused `document` for `reason`: sent again in the messages
/// after, it is a note, and attached again, it is refused before it is sent.
pub fn remember_refused(document: &KiroDocument, reason: &'static str) {
    remember_refused_key(refusal_key(document), reason);
}

fn remember_refused_key(key: [u8; 32], reason: &'static str) {
    let now = crate::now_secs();
    let mut refused = REFUSED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    refused.retain(|_, (_, until)| *until > now);
    if refused.len() >= REFUSAL_MEMORY_FILES {
        if let Some(oldest) = refused
            .iter()
            .min_by_key(|(_, (_, until))| *until)
            .map(|(key, _)| *key)
        {
            refused.remove(&oldest);
        }
    }
    refused.insert(key, (reason, now + REFUSAL_MEMORY_SECS));
}

/// Why an upstream refused `document`, if one did.
pub fn refused_reason(document: &KiroDocument) -> Option<&'static str> {
    let refused = REFUSED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if refused.is_empty() {
        return None;
    }
    refused
        .get(&refusal_key(document))
        .filter(|(_, until)| *until > crate::now_secs())
        .map(|(reason, _)| *reason)
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
        // Nothing to count by: estimated by size.
        assert_eq!(pdf_tokens(&vec![b'x'; 120 * 1024]), 3 * PDF_PAGE_TOKENS);
    }

    /// A PDF 1.5 file as writers make it: its objects, the page tree among them, inside a
    /// Flate-compressed object stream.
    fn compressed_pdf(pages: usize) -> Vec<u8> {
        use std::io::Write;
        let mut objects = format!("1 0 2 60 <</Type /Pages /Kids [3 0 R] /Count {pages}>>");
        for page in 0..pages {
            objects.push_str(&format!(
                " <</Type /Page /Parent 2 0 R /Contents {page} 0 R>>"
            ));
        }
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(objects.as_bytes()).unwrap();
        let data = encoder.finish().unwrap();
        let mut pdf =
            b"%PDF-1.5\n5 0 obj\n<</Type /ObjStm /N 2 /First 8 /Filter /FlateDecode /Length "
                .to_vec();
        pdf.extend_from_slice(format!("{}>>\nstream\n", data.len()).as_bytes());
        pdf.extend_from_slice(&data);
        pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
        pdf
    }

    #[test]
    fn the_page_tree_counts_its_own_pages_only() {
        // A node with inherited resources before its count, and an outline after it.
        let pdf = b"1 0 obj <</Resources <</Font <</F1 5 0 R>>>> /Type /Pages \
                    /Kids [2 0 R] /Count 7>> endobj 9 0 obj <</Type /Outlines /Count 40>>";
        assert_eq!(page_tree_count(pdf), Some(7));
        // A node whose count is past this one's end is not counted by it.
        let pdf = b"<</Type /Pages /Kids [2 0 R]>> <</Type /Outlines /Count 40>>";
        assert_eq!(page_tree_count(pdf), None);
    }

    #[test]
    fn only_an_encrypt_entry_marks_a_pdf_encrypted() {
        assert!(pdf_encrypted(
            b"trailer <</Size 9 /Encrypt 8 0 R /Root 1 0 R>>"
        ));
        assert!(pdf_encrypted(
            b"<</Type /XRef /Encrypt<</Filter /Standard>>>>"
        ));
        assert!(!pdf_encrypted(
            b"BT (Set the /Encrypt entry to protect it) Tj ET"
        ));
        assert!(!pdf_encrypted(b"/Encrypt 12 Tf"));
    }

    #[test]
    fn pages_are_counted_inside_compressed_object_streams() {
        let pdf = compressed_pdf(3);
        assert!(
            find(&pdf, b"/Type /Page ").is_none(),
            "pages are compressed"
        );
        assert_eq!(pdf_page_count(&pdf), Some(3));
        assert_eq!(pdf_tokens(&pdf), 3 * PDF_PAGE_TOKENS);
        // The tree's count wins over page objects an incremental update left behind.
        let updated = b"<</Type /Pages /Count 2>> <</Type /Page>> <</Type /Page>> <</Type /Page>>";
        assert_eq!(pdf_page_count(updated), Some(2));
    }

    #[test]
    fn a_pdf_alone_too_long_or_locked_is_refused_as_the_attachment_it_is() {
        let long = document("manual", "pdf", &compressed_pdf(150));
        let (reason, message) = too_large_for_model(std::slice::from_ref(&long), 200_000).unwrap();
        assert_eq!(reason, "DOCUMENT_MAXIMUM_PAGES_EXCEEDED");
        assert!(
            message.contains("150") && message.contains("100"),
            "{message}"
        );
        // A larger window reads more pages, until the pages alone exceed it.
        assert_eq!(too_large_for_model(&[long], 1_000_000), None);
        let (reason, _) =
            too_large_for_model(&[document("big", "pdf", &compressed_pdf(90))], 150_000).unwrap();
        assert_eq!(reason, "DOCUMENT_SIZE_EXCEEDED");
        // Uncountable: the upstream decides.
        assert_eq!(
            too_large_for_model(&[document("scan", "pdf", &[b'x'; 90_000])], 1_000),
            None
        );

        let locked = document(
            "locked",
            "pdf",
            b"%PDF-1.4 trailer <</Root 1 0 R /Encrypt 9 0 R>>",
        );
        // Sent again from an earlier message, it is a note.
        assert_eq!(content_part(&locked, true)["type"], "text");
        let (reason, _) = refusal(&[locked], true).unwrap();
        assert_eq!(reason, "DOCUMENT_PASSWORD_PROTECTED");
    }

    #[test]
    fn a_pdf_an_upstream_refused_is_a_note_afterwards() {
        let refused = document("refused-once", "pdf", b"%PDF-1.4 refused-once");
        assert_eq!(
            upstream_refusal_reason(
                400,
                "messages.0.content.0.pdf: The PDF specified was not valid"
            ),
            Some("DOCUMENT_MODEL_NOT_SUPPORTED")
        );
        assert_eq!(
            upstream_refusal_reason(400, "A maximum of 100 PDF pages may be provided"),
            Some("DOCUMENT_MAXIMUM_PAGES_EXCEEDED")
        );
        assert_eq!(upstream_refusal_reason(400, "max_tokens: too large"), None);
        remember_refused(&refused, "DOCUMENT_MODEL_NOT_SUPPORTED");
        let part = content_part(&refused, true);
        assert_eq!(part["type"], "text");
        assert!(part["text"].as_str().unwrap().contains("refused-once.pdf"));
        let (reason, _) = refusal(&[refused], true).unwrap();
        assert_eq!(reason, "DOCUMENT_MODEL_NOT_SUPPORTED");
        // Another file is not affected.
        let other = document("other", "pdf", b"%PDF-1.4 other");
        assert_eq!(content_part(&other, true)["type"], "file");
    }
}
