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

#[derive(Clone, Debug)]
struct BlockFrame {
    delimiter: &'static str,
    node_type: &'static str,
    line: u32,
    label: String,
    children: Vec<SemanticNode>,
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
    let mut total_lines = 0u32;

    for (index, raw) in source.lines().enumerate() {
        let line = index as u32;
        total_lines = line;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(active) = block.as_mut() {
            if trimmed == active.delimiter {
                let id = format!("0.{}", children.len());
                let closed = node(
                    &id,
                    active.node_type,
                    &active.label,
                    active.line,
                    0,
                    &active.children,
                );
                children.push(closed);
                block = None;
            } else if let Some(child) =
                inline_or_paragraph_node(&format!("block.{}", active.children.len()), trimmed, line)
            {
                active.children.push(child);
            }
            continue;
        }

        if let Some((delimiter, node_type, label)) = block_start(trimmed) {
            block = Some(BlockFrame {
                delimiter,
                node_type,
                line,
                label,
                children: Vec::new(),
            });
            continue;
        }

        if let Some(parsed) = top_level_node(&format!("0.{}", children.len()), trimmed, line) {
            children.push(parsed);
        }
    }

    if let Some(active) = block {
        let id = format!("0.{}", children.len());
        children.push(node(
            &id,
            active.node_type,
            &active.label,
            active.line,
            0,
            &active.children,
        ));
    }

    let root = SemanticNodeBuilder::new(
        "0",
        ROOT_NODE_TYPE,
        LANGUAGE_ID,
        0,
        0,
        total_lines,
        0,
        stable_hash(ROOT_NODE_TYPE, LANGUAGE_ID, &children),
    )
    .children(children)
    .build();

    match serde_json::to_string(&root) {
        Ok(serialized) => serialized,
        Err(err) => format!(r#"{{"error":"Serialisation error: {}"}}"#, err),
    }
}

fn block_start(line: &str) -> Option<(&'static str, &'static str, String)> {
    match line {
        "----" => Some(("----", "listing_block", "listing".to_string())),
        "====" => Some(("====", "example_block", "example".to_string())),
        "...." => Some(("....", "literal_block", "literal".to_string())),
        "____" => Some(("____", "quote_block", "quote".to_string())),
        "****" => Some(("****", "sidebar_block", "sidebar".to_string())),
        _ => None,
    }
}

fn top_level_node(id: &str, line: &str, line_no: u32) -> Option<SemanticNode> {
    if let Some((level, title)) = section_title(line) {
        return Some(node(
            id,
            &format!("section_level_{level}"),
            &title,
            line_no,
            0,
            &inline_children(id, line, line_no),
        ));
    }
    if let Some(label) = attribute_label(line) {
        return Some(node(id, "attribute_entry", &label, line_no, 0, &[]));
    }
    if let Some(label) = anchor_label(line) {
        return Some(node(id, "anchor", &label, line_no, 0, &[]));
    }
    if let Some(label) = include_label(line) {
        return Some(node(id, "include_directive", &label, line_no, 0, &[]));
    }
    if let Some((node_type, label)) = admonition_label(line) {
        return Some(node(
            id,
            node_type,
            &label,
            line_no,
            0,
            &inline_children(id, line, line_no),
        ));
    }
    if let Some(label) = image_label(line) {
        return Some(node(id, "image", &label, line_no, 0, &[]));
    }
    if let Some(label) = list_item_label(line) {
        return Some(node(
            id,
            "list_item",
            &label,
            line_no,
            0,
            &inline_children(id, line, line_no),
        ));
    }
    inline_or_paragraph_node(id, line, line_no)
}

fn inline_or_paragraph_node(id: &str, line: &str, line_no: u32) -> Option<SemanticNode> {
    let children = inline_children(id, line, line_no);
    if children.is_empty() && line.starts_with("//") {
        return None;
    }
    Some(node(id, "paragraph", line, line_no, 0, &children))
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

fn include_label(line: &str) -> Option<String> {
    line.strip_prefix("include::")
        .and_then(|rest| {
            rest.split_once('[')
                .map(|(target, _)| target.trim().to_string())
        })
        .filter(|target| !target.is_empty())
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
        .and_then(|rest| {
            rest.split_once('[')
                .map(|(target, _)| target.trim().to_string())
        })
        .filter(|target| !target.is_empty())
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

fn inline_children(id: &str, line: &str, line_no: u32) -> Vec<SemanticNode> {
    let mut children = Vec::new();
    for link in extract_inline_targets(line, "link:", "link") {
        children.push(node(
            &format!("{id}.{}", children.len()),
            "link",
            &link,
            line_no,
            0,
            &[],
        ));
    }
    for xref in extract_inline_targets(line, "xref:", "xref") {
        children.push(node(
            &format!("{id}.{}", children.len()),
            "xref",
            &xref,
            line_no,
            0,
            &[],
        ));
    }
    children
}

fn extract_inline_targets(line: &str, prefix: &str, scheme: &str) -> Vec<String> {
    let mut labels = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find(prefix) {
        let after_prefix = &rest[start + prefix.len()..];
        let Some((target, after_target)) = after_prefix.split_once('[') else {
            break;
        };
        let target = target.trim();
        if !target.is_empty() {
            labels.push(if scheme == "xref" {
                target.trim_start_matches('#').to_string()
            } else {
                target.to_string()
            });
        }
        rest = after_target;
    }
    labels
}

fn node(
    id: &str,
    node_type: &str,
    label: &str,
    line: u32,
    col: u32,
    children: &[SemanticNode],
) -> SemanticNode {
    SemanticNodeBuilder::new(
        id,
        node_type,
        label,
        line,
        col,
        line,
        col + label.len() as u32,
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
}
