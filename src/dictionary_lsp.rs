use serde::{Deserialize, Serialize};
// use stardict::{StarDict, StarDictResult};
use std::collections::HashMap;
// use std::path::Path;
use tokio;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::lsp_types::{
    Hover,
    HoverContents,
    HoverParams,
    MarkupContent,
    MarkupKind,
    Position,
    // Make sure other imports are preserved
};
use tower_lsp::{Client, LanguageServer, LspService, Server};

// FIX: function for checking if a character is a CJK character to avoid UTF-8 boundary issues
fn is_cjk_char(c: char) -> bool {
    // CJK Unified Ideographs range
    (c >= '\u{4E00}' && c <= '\u{9FFF}') ||
    // CJK Unified Ideographs Extension A
    (c >= '\u{3400}' && c <= '\u{4DBF}') ||
    // CJK Unified Ideographs Extension B
    (c >= '\u{20000}' && c <= '\u{2A6DF}') ||
    // CJK Unified Ideographs Extension C
    (c >= '\u{2A700}' && c <= '\u{2B73F}') ||
    // CJK Unified Ideographs Extension D
    (c >= '\u{2B740}' && c <= '\u{2B81F}')
}

#[derive(Debug, Serialize, Deserialize)]
struct DictionaryResponse {
    word: String,
    meanings: Vec<Meaning>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Meaning {
    part_of_speech: String,
    definitions: Vec<Definition>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Definition {
    definition: String,
    example: Option<String>,
}

#[derive(Debug)]
struct DictionaryLsp {
    client: Client,
    document_map: Mutex<HashMap<Url, String>>,
}

#[tower_lsp::async_trait]
impl LanguageServer for DictionaryLsp {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                // Add other capabilities as needed
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        let content = document.text;
        let uri = document.uri;

        // Store document content
        self.document_map
            .lock()
            .await
            .insert(uri.clone(), content.clone());

        // Parse document and provide analysis
        self.analyze_document(uri, content).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(change) = params.content_changes.get(0) {
            let content = change.text.clone();

            // Update stored document
            self.document_map
                .lock()
                .await
                .insert(uri.clone(), content.clone());

            // Re-analyze document
            self.analyze_document(uri, content).await;
        }
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        self.on_hover(params).await
    }
}

impl DictionaryLsp {
    async fn analyze_document(&self, uri: Url, content: String) {
        // Parse the document content
        let words = self.parse_document(&content);

        // Example: Send diagnostic for unknown words
        let diagnostics = self.check_words(words).await;

        // Send diagnostics to client
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    fn parse_document(&self, content: &str) -> Vec<String> {
        // Simple word extraction - you can make this more sophisticated
        content
            .split_whitespace()
            .map(|word| {
                word.trim_matches(|c: char| !c.is_alphabetic())
                    .to_lowercase()
            })
            .filter(|word| !word.is_empty())
            .collect()
    }

    async fn check_words(&self, _words: Vec<String>) -> Vec<Diagnostic> {
        // Implement your dictionary checking logic here
        Vec::new() // Placeholder for now
    }

    async fn on_hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let position = params.text_document_position_params.position;
        let document_uri = params.text_document_position_params.text_document.uri;

        if let Ok(content) = std::fs::read_to_string(document_uri.path()) {
            // Extract the word at position
            if let Some(word) = self.get_word_at_position(&content, position) {
                // TODO: add proper dictionary lookup here to get the word meaning and return to the client
                let contents = HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!("**{}**: man", word),
                });

                return Ok(Some(Hover {
                    contents,
                    range: None,
                }));
            }
        }

        Ok(None)
    }

    fn get_word_at_position(&self, content: &str, position: Position) -> Option<String> {
        let lines: Vec<&str> = content.lines().collect();
        if position.line as usize >= lines.len() {
            return None;
        }

        let line = lines[position.line as usize];
        let chars: Vec<char> = line.chars().collect();
        let char_pos = position.character as usize;
        if char_pos >= chars.len() {
            return None;
        }
        //FIX: Construct the word from characters to avoid UTF-8 boundary issues
        let mut start = char_pos;
        let mut end = char_pos;
        while start > 0 && (chars[start - 1].is_alphabetic() || is_cjk_char(chars[start - 1])) {
            start -= 1;
        }
        while end < chars.len() && (chars[end].is_alphabetic() || is_cjk_char(chars[end])) {
            end += 1;
        }
        if start == end {
            None
        } else {
            Some(chars[start..end].iter().collect())
        }
    }
}

#[tokio::main]
pub async fn run_server() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (_service, socket) = LspService::new(|client| DictionaryLsp {
        client,
        document_map: Mutex::new(HashMap::new()),
    });
    Server::new(stdin, stdout, socket).serve(_service).await;
}
