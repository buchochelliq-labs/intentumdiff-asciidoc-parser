use intentumdiff_plugin_sdk::tree::{SemanticNode, SemanticNodeBuilder};

wit_bindgen::generate!({
    path: "wit/plugin.wit",
    world: "parser-plugin",
});

use crate::exports::intentdiff::plugin::parser::ExamplePair;
use crate::exports::intentdiff::plugin::parser::Guest;
use crate::exports::intentdiff::plugin::parser::LanguageInfoRecord;
use crate::exports::intentdiff::plugin::parser::ParserMode;

const LANGUAGE_ID: &str = "asciidoc";
const ROOT_NODE_TYPE: &str = "asciidoc_document";
const DETECT_EXTENSIONS: &[&str] = &[".adoc", ".asciidoc", ".asc"];
const DEFAULT_OLD: &str = "= Old\n";
const DEFAULT_NEW: &str = "= New\n";
const PLUGIN_METADATA: &str = include_str!("../plugin_metadata.info");

/// Source extent of a node: 0-based rows over LF-separated source and UTF-8
/// byte columns. A trailing CR (CRLF input) belongs to no span, so every
/// column is in bounds of the row with or without its CR.
#[derive(Clone, Copy, Debug)]
struct Span {
    start_line: u32,
    start_col: u32,
    end_line: u32,
    end_col: u32,
}

impl Span {
    fn on_line(line: u32, start_col: u32, end_col: u32) -> Self {
        Span {
            start_line: line,
            start_col,
            end_line: line,
            end_col,
        }
    }
}

#[derive(Clone, Debug)]
struct BlockFrame {
    id: String,
    delimiter: &'static str,
    node_type: &'static str,
    verbatim: bool,
    line: u32,
    col: u32,
    label: String,
    children: Vec<SemanticNode>,
    last_line: u32,
    last_end_col: u32,
}

struct AsciidocParser;

fn language_info_for(ids: Vec<String>) -> Vec<LanguageInfoRecord> {
    let metadata = intentumdiff_plugin_sdk::metadata::parse_plugin_metadata(PLUGIN_METADATA);
    ids.into_iter()
        .map(|language_id| {
            let info = metadata.language_or_default(&language_id);
            LanguageInfoRecord {
                language_id: info.language_id,
                language_name: info.language_name,
                language_short_name: info.language_short_name,
                monaco_language: info.monaco_language,
                default_filename: info.default_filename,
                language_file_extensions: info.language_file_extensions,
                author: metadata.author().to_string(),
                plugin_version: metadata.plugin_version().to_string(),
                last_updated: metadata.last_updated().to_string(),
            }
        })
        .collect()
}

fn detect_language_impl(filename: &str, _content: &str) -> String {
    let lower = filename.to_lowercase();
    if DETECT_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
        LANGUAGE_ID.to_string()
    } else {
        String::new()
    }
}

fn parse_asciidoc(source: &str) -> String {
    let mut children = Vec::new();
    let mut block: Option<BlockFrame> = None;
    let mut root_end = (0u32, 0u32);

    for (index, row) in source.split('\n').enumerate() {
        let line = index as u32;
        let raw = row.strip_suffix('\r').unwrap_or(row);
        root_end = (line, raw.len() as u32);
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let indent = (raw.len() - raw.trim_start().len()) as u32;
        let line_end = indent + trimmed.len() as u32;

        if let Some(active) = block.as_mut() {
            active.last_line = line;
            active.last_end_col = line_end;
            if trimmed == active.delimiter {
                children.push(close_block(active));
                block = None;
            } else {
                let child_id = format!("{}.{}", active.id, active.children.len());
                let child = if active.verbatim {
                    // Verbatim content is never an AsciiDoc comment.
                    Some(line_node(&child_id, "paragraph", trimmed, line, indent))
                } else {
                    inline_or_paragraph_node(&child_id, trimmed, line, indent)
                };
                if let Some(child) = child {
                    active.children.push(child);
                }
            }
            continue;
        }

        if let Some((delimiter, node_type, label, verbatim)) = block_start(trimmed) {
            block = Some(BlockFrame {
                id: format!("0.{}", children.len()),
                delimiter,
                node_type,
                verbatim,
                line,
                col: indent,
                label,
                children: Vec::new(),
                last_line: line,
                last_end_col: line_end,
            });
            continue;
        }

        if let Some(parsed) =
            top_level_node(&format!("0.{}", children.len()), trimmed, line, indent)
        {
            children.push(parsed);
        }
    }

    if let Some(active) = block.as_ref() {
        // Unterminated block: extend to the last non-blank line it consumed.
        children.push(close_block(active));
    }

    let root = SemanticNodeBuilder::new(
        "0",
        ROOT_NODE_TYPE,
        LANGUAGE_ID,
        0,
        0,
        root_end.0,
        root_end.1,
        stable_hash(ROOT_NODE_TYPE, LANGUAGE_ID, &children),
    )
    .children(children)
    .build();

    match serde_json::to_string(&root) {
        Ok(serialized) => serialized,
        Err(err) => format!(r#"{{"error":"Serialisation error: {}"}}"#, err),
    }
}

fn close_block(frame: &BlockFrame) -> SemanticNode {
    node(
        &frame.id,
        frame.node_type,
        &frame.label,
        Span {
            start_line: frame.line,
            start_col: frame.col,
            end_line: frame.last_line,
            end_col: frame.last_end_col,
        },
        &frame.children,
    )
}

fn block_start(line: &str) -> Option<(&'static str, &'static str, String, bool)> {
    match line {
        "----" => Some(("----", "listing_block", "listing".to_string(), true)),
        "====" => Some(("====", "example_block", "example".to_string(), false)),
        "...." => Some(("....", "literal_block", "literal".to_string(), true)),
        "____" => Some(("____", "quote_block", "quote".to_string(), false)),
        "****" => Some(("****", "sidebar_block", "sidebar".to_string(), false)),
        _ => None,
    }
}

/// `line` is the trimmed source line; `source_col` is the UTF-8 byte column
/// at which it starts in the raw row.
fn top_level_node(id: &str, line: &str, line_no: u32, source_col: u32) -> Option<SemanticNode> {
    let line_span = Span::on_line(line_no, source_col, source_col + line.len() as u32);
    if let Some((level, title)) = section_title(line) {
        // Semantic title labels must start at the actual source title, not
        // at the '=' heading marker. UTF-8 byte columns include indentation
        // and the whole multi-byte title (#4).
        let title_col = source_col
            + level as u32
            + line[level..].find(title.as_str()).unwrap_or(1) as u32;
        return Some(node(
            id,
            &format!("section_level_{level}"),
            &title,
            Span::on_line(line_no, title_col, title_col + title.len() as u32),
            &inline_children(id, line, line_no, source_col),
        ));
    }
    if let Some(label) = attribute_label(line) {
        return Some(node(id, "attribute_entry", &label, line_span, &[]));
    }
    if let Some(label) = anchor_label(line) {
        return Some(node(id, "anchor", &label, line_span, &[]));
    }
    if let Some(label) = include_label(line) {
        return Some(node(id, "include_directive", &label, line_span, &[]));
    }
    if let Some((node_type, label)) = admonition_label(line) {
        return Some(node(
            id,
            node_type,
            &label,
            line_span,
            &inline_children(id, line, line_no, source_col),
        ));
    }
    if let Some(label) = image_label(line) {
        return Some(node(id, "image", &label, line_span, &[]));
    }
    if let Some(label) = list_item_label(line) {
        return Some(node(
            id,
            "list_item",
            &label,
            line_span,
            &inline_children(id, line, line_no, source_col),
        ));
    }
    inline_or_paragraph_node(id, line, line_no, source_col)
}

fn inline_or_paragraph_node(
    id: &str,
    line: &str,
    line_no: u32,
    source_col: u32,
) -> Option<SemanticNode> {
    let children = inline_children(id, line, line_no, source_col);
    if children.is_empty() && line.starts_with("//") {
        return None;
    }
    Some(node(
        id,
        "paragraph",
        line,
        Span::on_line(line_no, source_col, source_col + line.len() as u32),
        &children,
    ))
}

fn line_node(id: &str, node_type: &str, line: &str, line_no: u32, source_col: u32) -> SemanticNode {
    node(
        id,
        node_type,
        line,
        Span::on_line(line_no, source_col, source_col + line.len() as u32),
        &[],
    )
}

fn section_title(line: &str) -> Option<(usize, String)> {
    let level = line.chars().take_while(|ch| *ch == '=').count();
    if level == 0 || !line[level..].starts_with(' ') {
        return None;
    }
    Some((level, line[level..].trim().to_string()))
}

fn attribute_label(line: &str) -> Option<String> {
    if !line.starts_with(':') {
        return None;
    }
    let end = line[1..].find(':')? + 1;
    let name = line[1..end].trim();
    let value = line[end + 1..].trim();
    if name.is_empty() {
        None
    } else if value.is_empty() {
        Some(name.to_string())
    } else {
        Some(format!("{name}: {value}"))
    }
}

fn anchor_label(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("[[") {
        return rest
            .split_once("]]")
            .map(|(label, _)| label.trim().to_string())
            .filter(|label| !label.is_empty());
    }
    line.strip_prefix("[#")
        .and_then(|rest| rest.split_once(']'))
        .map(|(label, _)| label.trim().to_string())
        .filter(|label| !label.is_empty())
}

/// Label for a `target[attrlist]` macro body. The attribute list carries
/// semantics (include `lines=`/`tag=` filters, image alt text, sizes), so
/// it is kept verbatim; only an empty list collapses to the bare target.
/// Anything that is not a well-formed macro (missing `[` or trailing text)
/// keeps the whole body so no source content is silently dropped (#6).
fn macro_label(rest: &str) -> Option<String> {
    let rest = rest.trim();
    let (target, attrs) = rest.split_once('[')?;
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    match attrs.strip_suffix(']') {
        Some(attrs) if attrs.trim().is_empty() => Some(target.to_string()),
        Some(attrs) => Some(format!("{target}[{}]", attrs.trim())),
        None => Some(format!("{target}[{attrs}")),
    }
}

fn include_label(line: &str) -> Option<String> {
    line.strip_prefix("include::").and_then(macro_label)
}

fn admonition_label(line: &str) -> Option<(&'static str, String)> {
    for (prefix, node_type) in [
        ("NOTE:", "note_admonition"),
        ("TIP:", "tip_admonition"),
        ("IMPORTANT:", "important_admonition"),
        ("WARNING:", "warning_admonition"),
        ("CAUTION:", "caution_admonition"),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some((node_type, rest.trim().to_string()));
        }
    }
    None
}

fn image_label(line: &str) -> Option<String> {
    line.strip_prefix("image::")
        .or_else(|| line.strip_prefix("image:"))
        .and_then(macro_label)
}

fn list_item_label(line: &str) -> Option<String> {
    let stripped = line.trim_start();
    for marker in ["* ", "- ", ". "] {
        if let Some(label) = stripped.strip_prefix(marker) {
            return Some(label.trim().to_string());
        }
    }
    None
}

fn inline_children(id: &str, line: &str, line_no: u32, source_col: u32) -> Vec<SemanticNode> {
    let mut children = Vec::new();
    for (node_type, prefix) in [("link", "link:"), ("xref", "xref:")] {
        for (target, offset) in extract_inline_targets(line, prefix, node_type) {
            let col = source_col + offset as u32;
            children.push(node(
                &format!("{id}.{}", children.len()),
                node_type,
                &target,
                Span::on_line(line_no, col, col + target.len() as u32),
                &[],
            ));
        }
    }
    children
}

/// Returns each inline macro target with its byte offset within `line`.
fn extract_inline_targets(line: &str, prefix: &str, scheme: &str) -> Vec<(String, usize)> {
    let mut labels = Vec::new();
    let mut base = 0usize;
    while let Some(start) = line[base..].find(prefix) {
        let target_start = base + start + prefix.len();
        let Some(bracket) = line[target_start..].find('[') else {
            break;
        };
        let raw_target = &line[target_start..target_start + bracket];
        let mut offset = target_start + (raw_target.len() - raw_target.trim_start().len());
        let mut target = raw_target.trim();
        if scheme == "xref" {
            let stripped = target.trim_start_matches('#');
            offset += target.len() - stripped.len();
            target = stripped;
        }
        if !target.is_empty() {
            labels.push((target.to_string(), offset));
        }
        base = target_start + bracket + 1;
    }
    labels
}

fn node(
    id: &str,
    node_type: &str,
    label: &str,
    span: Span,
    children: &[SemanticNode],
) -> SemanticNode {
    SemanticNodeBuilder::new(
        id,
        node_type,
        label,
        span.start_line,
        span.start_col,
        span.end_line,
        span.end_col,
        stable_hash(node_type, label, children),
    )
    .children(children.to_vec())
    .build()
}

fn stable_hash(node_type: &str, label: &str, children: &[SemanticNode]) -> String {
    let mut value = format!("{node_type}:{label}");
    for child in children {
        value.push('|');
        value.push_str(&child.structural_hash);
    }
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

impl Guest for AsciidocParser {
    fn get_parser_mode() -> ParserMode {
        ParserMode::FullParse
    }

    fn grammar_id() -> String {
        LANGUAGE_ID.to_string()
    }

    fn detect_language(filename: String, content: String) -> String {
        detect_language_impl(&filename, &content)
    }

    fn preprocess_source(source: String) -> String {
        source
    }

    fn example(_language: String) -> ExamplePair {
        ExamplePair {
            old: DEFAULT_OLD.to_string(),
            new: DEFAULT_NEW.to_string(),
        }
    }

    fn process(input: String, _language: String, _filename: String) -> String {
        parse_asciidoc(&input)
    }

    fn trivia_node_types() -> Vec<String> {
        vec![]
    }

    fn language_ids() -> Vec<String> {
        vec![LANGUAGE_ID.to_string()]
    }

    fn language_info() -> Vec<LanguageInfoRecord> {
        language_info_for(Self::language_ids())
    }

    fn priority() -> i32 {
        0
    }
}

export!(AsciidocParser);

#[cfg(test)]
mod tests {
    use super::*;

    fn labels_by_type(node: &SemanticNode, node_type: &str, labels: &mut Vec<String>) {
        if node.node_type == node_type {
            labels.push(node.label.clone());
        }
        for child in &node.children {
            labels_by_type(child, node_type, labels);
        }
    }

    #[test]
    fn parser_mode_is_full_parse() {
        assert_eq!(AsciidocParser::get_parser_mode(), ParserMode::FullParse);
    }

    #[test]
    fn grammar_id_is_language_id() {
        assert_eq!(AsciidocParser::grammar_id(), LANGUAGE_ID);
        assert_eq!(
            AsciidocParser::language_ids(),
            vec![LANGUAGE_ID.to_string()]
        );
    }

    #[test]
    fn detects_asciidoc_extensions() {
        assert_eq!(
            detect_language_impl("README.adoc", DEFAULT_NEW),
            LANGUAGE_ID
        );
        assert_eq!(
            detect_language_impl("guide.asciidoc", DEFAULT_NEW),
            LANGUAGE_ID
        );
    }

    #[test]
    fn process_returns_valid_json() {
        let parsed = parse_asciidoc(DEFAULT_NEW);
        intentumdiff_plugin_sdk::testing::assert_valid_json(&parsed, LANGUAGE_ID);
        intentumdiff_plugin_sdk::testing::assert_root_node_type(&parsed, ROOT_NODE_TYPE, LANGUAGE_ID);
    }

    #[test]
    fn process_extracts_sections_attributes_blocks_links_and_xrefs() {
        let parsed = parse_asciidoc(
            r#"
= IntentumDiff
:revnumber: 1.0
[[install]]
== Install
include::partials/setup.adoc[]
NOTE: See link:https://example.com/docs[docs] and xref:#usage[Usage].
* Run setup
image::screens/review.png[]
----
intentumdiff git main
----
"#,
        );
        let root: SemanticNode = serde_json::from_str(&parsed).unwrap();
        let mut sections = Vec::new();
        let mut attributes = Vec::new();
        let mut anchors = Vec::new();
        let mut includes = Vec::new();
        let mut notes = Vec::new();
        let mut list_items = Vec::new();
        let mut images = Vec::new();
        let mut listings = Vec::new();
        let mut links = Vec::new();
        let mut xrefs = Vec::new();
        labels_by_type(&root, "section_level_1", &mut sections);
        labels_by_type(&root, "section_level_2", &mut sections);
        labels_by_type(&root, "attribute_entry", &mut attributes);
        labels_by_type(&root, "anchor", &mut anchors);
        labels_by_type(&root, "include_directive", &mut includes);
        labels_by_type(&root, "note_admonition", &mut notes);
        labels_by_type(&root, "list_item", &mut list_items);
        labels_by_type(&root, "image", &mut images);
        labels_by_type(&root, "listing_block", &mut listings);
        labels_by_type(&root, "link", &mut links);
        labels_by_type(&root, "xref", &mut xrefs);

        assert!(sections.contains(&"IntentumDiff".to_string()));
        assert!(sections.contains(&"Install".to_string()));
        assert!(attributes.contains(&"revnumber: 1.0".to_string()));
        assert!(anchors.contains(&"install".to_string()));
        assert!(includes.contains(&"partials/setup.adoc".to_string()));
        assert!(notes.iter().any(|label| label.contains("See link")));
        assert!(list_items.contains(&"Run setup".to_string()));
        assert!(images.contains(&"screens/review.png".to_string()));
        assert!(listings.contains(&"listing".to_string()));
        assert!(links.contains(&"https://example.com/docs".to_string()));
        assert!(xrefs.contains(&"usage".to_string()));
    }

    #[test]
    fn section_titles_use_actual_utf8_source_byte_extents() {
        // The former parser used col=0 and label.len(), yielding e.g. the
        // source slice "== Overv" for a node labelled "Overview".
        // Leading indentation and multibyte letters must also be counted.
        let source = "  == Révisions  \n= Service Guide\n";
        let parsed: serde_json::Value =
            serde_json::from_str(&parse_asciidoc(source)).unwrap();
        let sections = parsed["children"].as_array().unwrap();
        assert_eq!(sections.len(), 2);
        let first = &sections[0];
        assert_eq!(first["label"], "Révisions");
        assert_eq!(first["position"]["start_line"].as_u64(), Some(0));
        assert_eq!(first["position"]["start_col"].as_u64(), Some(5));
        assert_eq!(
            first["position"]["end_col"].as_u64(),
            Some(5 + "Révisions".len() as u64)
        );
        let equals: serde_json::Value =
            serde_json::from_str(&parse_asciidoc("== =\n")).unwrap();
        assert_eq!(equals["children"][0]["label"], "=");
        assert_eq!(equals["children"][0]["position"]["start_col"], 3);
        assert_eq!(equals["children"][0]["position"]["end_col"], 4);

        let second = &sections[1];
        assert_eq!(second["label"], "Service Guide");
        assert_eq!(second["position"]["start_line"].as_u64(), Some(1));
        assert_eq!(second["position"]["start_col"].as_u64(), Some(2));
        assert_eq!(
            second["position"]["end_col"].as_u64(),
            Some(2 + "Service Guide".len() as u64)
        );
    }


    fn parse_value(source: &str) -> serde_json::Value {
        serde_json::from_str(&parse_asciidoc(source)).unwrap()
    }

    fn span_of(node: &serde_json::Value) -> (u64, u64, u64, u64) {
        let p = &node["position"];
        (
            p["start_line"].as_u64().unwrap(),
            p["start_col"].as_u64().unwrap(),
            p["end_line"].as_u64().unwrap(),
            p["end_col"].as_u64().unwrap(),
        )
    }

    /// Every span is ordered, in bounds of the LF-separated rows (CR
    /// excluded), lies on UTF-8 boundaries, nests inside its parent, and
    /// every id is globally unique.
    fn assert_well_formed(source: &str) {
        let rows: Vec<&str> = source
            .split('\n')
            .map(|row| row.strip_suffix('\r').unwrap_or(row))
            .collect();
        let tree = parse_value(source);
        let mut ids = std::collections::HashSet::new();
        fn walk(
            node: &serde_json::Value,
            parent: Option<(u64, u64, u64, u64)>,
            rows: &[&str],
            ids: &mut std::collections::HashSet<String>,
            source: &str,
        ) {
            let id = node["id"].as_str().unwrap().to_string();
            assert!(ids.insert(id.clone()), "duplicate id {id} in {source:?}");
            let (sl, sc, el, ec) = span_of(node);
            assert!((sl, sc) <= (el, ec), "unordered span on {id} in {source:?}");
            for (line, col) in [(sl, sc), (el, ec)] {
                let row = rows.get(line as usize).unwrap_or_else(|| {
                    panic!("row {line} out of bounds on {id} in {source:?}")
                });
                assert!(col as usize <= row.len(), "column out of bounds on {id} in {source:?}");
                assert!(row.is_char_boundary(col as usize), "non-UTF-8 boundary on {id}");
            }
            if let Some((psl, psc, pel, pec)) = parent {
                assert!((psl, psc) <= (sl, sc) && (el, ec) <= (pel, pec), "{id} escapes parent");
            }
            for child in node["children"].as_array().unwrap() {
                walk(child, Some((sl, sc, el, ec)), rows, ids, source);
            }
        }
        walk(&tree, None, &rows, &mut ids, source);
    }

    #[test]
    fn include_attributes_are_semantic_and_spanned() {
        // Exact pair from #6: the lines= filter selects a different range.
        let old = parse_value("include::file.txt[lines=1..3]\n");
        let new = parse_value("include::file.txt[lines=4..6]\n");
        let (old, new) = (&old["children"][0], &new["children"][0]);
        assert_eq!(old["node_type"], "include_directive");
        assert_eq!(old["label"], "file.txt[lines=1..3]");
        assert_eq!(new["label"], "file.txt[lines=4..6]");
        assert_ne!(old["structural_hash"], new["structural_hash"]);
        assert_eq!(span_of(old), (0, 0, 0, 29));
        assert_eq!(span_of(new), (0, 0, 0, 29));
        // An empty attribute list keeps the bare target.
        let bare = parse_value("include::partials/setup.adoc[]\n");
        assert_eq!(bare["children"][0]["label"], "partials/setup.adoc");
    }

    #[test]
    fn image_attributes_are_semantic_and_spanned() {
        // Exact pair from #6: the positional attribute is the alt text.
        let old = parse_value("image::logo.png[Safe]\n");
        let new = parse_value("image::logo.png[Danger]\n");
        let (old, new) = (&old["children"][0], &new["children"][0]);
        assert_eq!(old["node_type"], "image");
        assert_eq!(old["label"], "logo.png[Safe]");
        assert_eq!(new["label"], "logo.png[Danger]");
        assert_ne!(old["structural_hash"], new["structural_hash"]);
        assert_eq!(span_of(old), (0, 0, 0, 21));
        assert_eq!(span_of(new), (0, 0, 0, 23));
        // Indented, multibyte alt text with a quoted comma and trailing space.
        let src = "  image::a.png[\"Café, ok\",200]  \n";
        let img = &parse_value(src)["children"][0];
        assert_eq!(img["label"], "a.png[\"Café, ok\",200]");
        assert_eq!(span_of(img), (0, 2, 0, (src.trim_end().len()) as u64));
        // Unterminated or trailing text is preserved rather than dropped.
        assert_eq!(parse_value("image::logo.png[")["children"][0]["label"], "logo.png[");
        assert_eq!(
            parse_value("image:icon.png[] more\n")["children"][0]["label"],
            "icon.png[] more"
        );
        // Whitespace-only edits around the macro stay identical.
        assert_eq!(
            parse_value("image::logo.png[Safe]\n")["children"][0]["structural_hash"],
            parse_value("  image::logo.png[Safe]  \n")["children"][0]["structural_hash"]
        );
    }

    #[test]
    fn listing_blocks_have_exact_spans_and_unique_ids() {
        // Exact input from #7.
        let source = "----\na\n----\n\n----\nb\n----\n";
        let tree = parse_value(source);
        let blocks = tree["children"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["id"], "0.0");
        assert_eq!(span_of(&blocks[0]), (0, 0, 2, 4));
        assert_eq!(blocks[0]["children"][0]["id"], "0.0.0");
        assert_eq!(span_of(&blocks[0]["children"][0]), (1, 0, 1, 1));
        assert_eq!(blocks[1]["id"], "0.1");
        assert_eq!(span_of(&blocks[1]), (4, 0, 6, 4));
        assert_eq!(blocks[1]["children"][0]["id"], "0.1.0");
        assert_eq!(span_of(&blocks[1]["children"][0]), (5, 0, 5, 1));
        assert_eq!(span_of(&tree), (0, 0, 7, 0));
        assert_well_formed(source);
    }

    #[test]
    fn listing_content_comment_lines_are_kept() {
        let tree = parse_value("----\n// code\n----\n");
        assert_eq!(tree["children"][0]["children"][0]["label"], "// code");
    }

    #[test]
    fn inline_link_and_xref_spans_point_at_targets() {
        let source = "  NOTE: See link:https://x.io[docs] and xref:#usage[Usage].\n";
        let note = &parse_value(source)["children"][0];
        let link = &note["children"][0];
        let xref = &note["children"][1];
        let row = source.trim_end_matches('\n');
        let (_, lc, _, le) = span_of(link);
        assert_eq!(&row[lc as usize..le as usize], "https://x.io");
        let (_, xc, _, xe) = span_of(xref);
        assert_eq!(&row[xc as usize..xe as usize], "usage");
        assert_eq!(note["children"][0]["id"], "0.0.0");
        assert_eq!(note["children"][1]["id"], "0.0.1");
    }

    #[test]
    fn spans_and_ids_are_well_formed_across_layouts() {
        for source in [
            "",
            "\n",
            "= T",
            "= T\n",
            "= T\r\n\r\nPara\r\n----\r\nx\r\n----\r\n",
            "----\na\n----\n----\nb\n----",
            "====\nCafé €5 link:https://é.io[é]\n====\n****\nside\n****\n",
            "  ----\n    indented\n  ----\n",
            "----\nunterminated\n\n",
            "....\nlit\n....\n____\nquote\n____\n",
            "image::logo.png[\ninclude::a[lines=1;3]\n:attr: v\n[[id]]\n* item\n",
        ] {
            assert_well_formed(source);
        }
        // Unterminated block ends at its last consumed non-blank line.
        assert_eq!(span_of(&parse_value("----\nunterminated\n\n")["children"][0]), (0, 0, 1, 12));
        // CRLF: CR is excluded from columns.
        let crlf = parse_value("----\r\nx\r\n----\r\n");
        assert_eq!(span_of(&crlf["children"][0]), (0, 0, 2, 4));
    }
}
