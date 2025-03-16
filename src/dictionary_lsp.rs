use crate::config::Config;
use crate::formatting;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::lsp_types::{
    Documentation, Hover, HoverContents, HoverParams, MarkupContent, MarkupKind,
    ParameterInformation, Position, SignatureHelp, SignatureHelpOptions, SignatureHelpParams,
    SignatureInformation,
};
use tower_lsp::{Client, LanguageServer, LspService, Server};

// FIX: function for checking if a character is a CJK character to avoid UTF-8 boundary issues

/// Determines if the character is a CJK (Chinese, Japanese, Korean) character
/// by checking if it falls within the Unicode ranges for CJK characters.
/// This helps properly handle word boundaries for Asian languages.
fn is_cjk_char(c: char) -> bool {
    (c >= '\u{4E00}' && c <= '\u{9FFF}')  // CJK Unified Ideographs
        || (c >= '\u{3400}' && c <= '\u{4DBF}')  // CJK Unified Ideographs Extension A
        || (c >= '\u{20000}' && c <= '\u{2A6DF}')  // CJK Unified Ideographs Extension B
        || (c >= '\u{2A700}' && c <= '\u{2B73F}')  // CJK Unified Ideographs Extension C
        || (c >= '\u{2B740}' && c <= '\u{2B81F}') // CJK Unified Ideographs Extension D
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DictionaryResponse {
    pub word: String,
    pub meanings: Vec<Meaning>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Meaning {
    pub part_of_speech: String,
    pub definitions: Vec<Definition>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Definition {
    pub definition: String,
    pub example: Option<String>,
}

#[derive(Debug)]
struct DictionaryLsp {
    client: Client,
    document_map: Mutex<HashMap<Url, String>>,
    config: Config,
}

#[tower_lsp::async_trait]
impl LanguageServer for DictionaryLsp {
    /// Initializes the language server and advertises server capabilities to the client.
    /// This includes what features we support, such as hover functionality.
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec![" ".to_string()]), // Trigger on space
                    retrigger_characters: None,
                    work_done_progress_options: Default::default(),
                }),
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    /// Handles the opening of a text document by storing its content and
    /// analyzing it for dictionary lookups.
    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        let content = document.text;
        let uri = document.uri;

        self.document_map
            .lock()
            .await
            .insert(uri.clone(), content.clone());

        self.analyze_document(uri, content).await;
    }

    /// Handles document content changes by updating the stored document
    /// and re-analyzing it.
    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(change) = params.content_changes.get(0) {
            let content = change.text.clone();

            self.document_map
                .lock()
                .await
                .insert(uri.clone(), content.clone());

            self.analyze_document(uri, content).await;
        }
    }

    /// Handles the shutdown request from the client.
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    /// Processes hover requests by looking up dictionary definitions for
    /// the word under the cursor.
    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        self.on_hover(params).await
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        self.on_signature_help(params).await
    }
}

impl DictionaryLsp {
    /// Analyzes a document for dictionary lookups and publishes diagnostics.
    /// This function extracts words from the content and checks them against the dictionary.
    async fn analyze_document(&self, uri: Url, content: String) {
        let words = self.parse_document(&content);
        let diagnostics = self.check_words(words).await;

        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    /// Extracts words from the document content by splitting on whitespace and
    /// normalizing them (removing punctuation, converting to lowercase).
    fn parse_document(&self, content: &str) -> Vec<String> {
        content
            .split_whitespace()
            .map(|word| {
                word.trim_matches(|c: char| !c.is_alphabetic())
                    .to_lowercase()
            })
            .filter(|word| !word.is_empty())
            .collect()
    }

    /// Checks words against the dictionary and returns diagnostics.
    /// Currently returns an empty list as implementation is pending.
    async fn check_words(&self, _words: Vec<String>) -> Vec<Diagnostic> {
        Vec::new()
    }

    /// Retrieves the dictionary definition for a given word.
    /// Reads from a local JSON dictionary file and parses the entries.
    async fn get_meaning(&self, word: &str) -> Result<Option<DictionaryResponse>> {
        //TODO: Use a static or cached dictionary path instead of hardcoding
        let dict_path = if let Some(path) = &self.config.dictionary_path {
            std::path::PathBuf::from(path)
        } else {
            match dirs::home_dir().map(|p| p.join("dicts/dictionary.json")) {
                Some(path) => path,
                None => {
                    eprintln!("Could not determine home directory");
                    return Ok(None);
                }
            }
        };

        if !dict_path.exists() {
            eprintln!("Dictionary file not found at {:?}", dict_path);
            return Ok(None);
        }

        match std::fs::read_to_string(&dict_path) {
            Ok(contents) => {
                let dictionary: serde_json::Value = match serde_json::from_str(&contents) {
                    Ok(dict) => dict,
                    Err(e) => {
                        eprintln!("Error parsing dictionary JSON: {}", e);
                        return Ok(None);
                    }
                };

                let word_lower = word.to_lowercase();

                if let Some(entry) = dictionary.get(&word_lower) {
                    let mut meanings = Vec::new();

                    if let Some(obj) = entry.as_object() {
                        for (part_of_speech, defs) in obj {
                            if let Some(defs_array) = defs.as_array() {
                                let definitions = defs_array
                                    .iter()
                                    .map(|def| Definition {
                                        definition: def.as_str().unwrap_or("").to_string(),
                                        example: None,
                                    })
                                    .collect();

                                meanings.push(Meaning {
                                    part_of_speech: part_of_speech.clone(),
                                    definitions,
                                });
                            }
                        }
                    }

                    return Ok(Some(DictionaryResponse {
                        word: word.to_string(),
                        meanings,
                    }));
                }

                Ok(None)
            }
            Err(e) => {
                eprintln!("Error reading dictionary file: {}", e);
                Ok(None)
            }
        }
    }

    /// Handles hover events by finding the word at the cursor position
    /// and fetching its dictionary definition.
    /// TODO: Add support for SQLite database lookups
    /// TODO: Add support for multiple dictionary sources
    async fn on_hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let position = params.text_document_position_params.position;
        let document_uri = params.text_document_position_params.text_document.uri;

        // Get document content from memory or file system
        let content = match self.document_map.lock().await.get(&document_uri) {
            Some(content) => content.clone(),
            None => match std::fs::read_to_string(document_uri.path()) {
                Ok(content) => content,
                Err(_) => return Ok(None),
            },
        };

        // Extract the word at position and look up its meaning
        if let Some(word) = self.get_word_at_position(&content, position) {
            match self.get_meaning(&word).await {
                Ok(Some(response)) => {
                    // Format the response as Markdown
                    let markdown = formatting::format_definition_as_markdown_with_config(
                        &word,
                        &response,
                        &self.config.formatting,
                    );

                    let contents = HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: markdown,
                    });
                    return Ok(Some(Hover {
                        contents,
                        range: None,
                    }));
                }
                Ok(None) => {
                    let contents = HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: format!("No definition found for **{}**", word),
                    });
                    return Ok(Some(Hover {
                        contents,
                        range: None,
                    }));
                }
                Err(_) => {
                    let contents = HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: format!("Error looking up definition for **{}**", word),
                    });
                    return Ok(Some(Hover {
                        contents,
                        range: None,
                    }));
                }
            }
        }

        Ok(None)
    }

    /// Extracts the word at the given cursor position in the document.
    /// Handles both alphabetic characters and CJK characters properly.
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

    /// Handles signature help requests by finding the word at the cursor position
    /// and providing its dictionary definition in signature help format.
    async fn on_signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> Result<Option<SignatureHelp>> {
        let position = params.text_document_position_params.position;
        let document_uri = params.text_document_position_params.text_document.uri;

        // Get document content from memory or file system
        let content = match self.document_map.lock().await.get(&document_uri) {
            Some(content) => content.clone(),
            None => match std::fs::read_to_string(document_uri.path()) {
                Ok(content) => content,
                Err(_) => return Ok(None),
            },
        };

        if let Some(word) = self.get_word_at_position(&content, position) {
            match self.get_meaning(&word).await {
                Ok(Some(response)) => {
                    // Format the entire dictionary response as hover-like content
                    let value = formatting::format_definition_as_markdown_with_config(
                        &word,
                        &response,
                        &self.config.formatting,
                    );

                    // HACK: only show the label part of signature help
                    let signatures = vec![SignatureInformation {
                        documentation: Some(Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::PlainText,
                            value: "".to_string(),
                        })),
                        label: value,
                        parameters: None,
                        active_parameter: None,
                    }];

                    return Ok(Some(SignatureHelp {
                        signatures,
                        active_signature: Some(0),
                        active_parameter: None,
                    }));
                }
                Ok(None) => {
                    // No definition found
                    let signatures = vec![SignatureInformation {
                        label: format!("No definition found for '{}'", word),
                        documentation: None,
                        parameters: None,
                        active_parameter: None,
                    }];

                    return Ok(Some(SignatureHelp {
                        signatures,
                        active_signature: Some(0),
                        active_parameter: None,
                    }));
                }
                Err(_) => return Ok(None),
            }
        }

        Ok(None)
    }

    /// Converts a dictionary response into LSP SignatureInformation format
    /// Each part of speech becomes a separate signature with its definitions as parameters
    fn create_signatures_from_dictionary(
        &self,
        response: &DictionaryResponse,
    ) -> Vec<SignatureInformation> {
        let mut signatures = Vec::new();

        for meaning in &response.meanings {
            // Create parameter information for each definition
            let parameters: Vec<ParameterInformation> = meaning
                .definitions
                .iter()
                .map(|def| ParameterInformation {
                    label: tower_lsp::lsp_types::ParameterLabel::Simple(def.definition.clone()),
                    documentation: def.example.clone().map(|ex| {
                        Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format!("*Example:* {}", ex),
                        })
                    }),
                })
                .collect();

            // Create a signature for this part of speech
            signatures.push(SignatureInformation {
                label: format!("{}:", meaning.part_of_speech),
                documentation: Some(Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!("**{}** _{}_", response.word, meaning.part_of_speech),
                })),
                parameters: Some(parameters),
                active_parameter: None,
            });
        }

        signatures
    }
}

#[tokio::main]
pub async fn run_server() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| DictionaryLsp {
        client,
        document_map: Mutex::new(HashMap::new()),
        config: Config::get(),
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}
