use crate::config::Config;
use crate::formatting;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::lsp_types::{
    Documentation, Hover, HoverContents, HoverParams, MarkupContent, MarkupKind, Position,
    SignatureHelp, SignatureHelpOptions, SignatureHelpParams, SignatureInformation,
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
        let dict_path = self.get_dictionary_path()?;
        let dictionary = self.read_dictionary_file(&dict_path)?;

        let word_lower = word.to_lowercase();

        // Try exact match first
        if let Some(response) = self.find_exact_match(&dictionary, &word_lower) {
            return Ok(Some(response));
        }

        // Fall back to fuzzy matching
        if let Some(response) = self.find_fuzzy_match(&dictionary, &word_lower) {
            return Ok(Some(response));
        }

        Ok(None)
    }

    /// Determines the path to the dictionary file based on configuration or defaults.
    fn get_dictionary_path(&self) -> Result<std::path::PathBuf> {
        let dict_path = if let Some(path) = &self.config.dictionary_path {
            std::path::PathBuf::from(path)
        } else {
            match dirs::home_dir().map(|p| p.join("dicts/dictionary.json")) {
                Some(path) => path,
                None => {
                    eprintln!("Could not determine home directory");
                    return Err(tower_lsp::jsonrpc::Error::internal_error());
                }
            }
        };

        if !dict_path.exists() {
            eprintln!("Dictionary file not found at {:?}", dict_path);
            return Err(tower_lsp::jsonrpc::Error::internal_error());
        }

        Ok(dict_path)
    }

    /// Reads and parses the dictionary file into a JSON value.
    fn read_dictionary_file(&self, dict_path: &std::path::Path) -> Result<serde_json::Value> {
        match std::fs::read_to_string(dict_path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(dict) => Ok(dict),
                Err(e) => {
                    eprintln!("Error parsing dictionary JSON: {}", e);
                    Err(tower_lsp::jsonrpc::Error::internal_error())
                }
            },
            Err(e) => {
                eprintln!("Error reading dictionary file: {}", e);
                Err(tower_lsp::jsonrpc::Error::internal_error())
            }
        }
    }

    /// Finds an exact match for the word in the dictionary.
    fn find_exact_match(
        &self,
        dictionary: &serde_json::Value,
        word: &str,
    ) -> Option<DictionaryResponse> {
        dictionary
            .get(word)
            .map(|entry| self.parse_dictionary_entry(word, entry, Some(word)))
    }

    /// Attempts to find a close match using fuzzy matching.
    fn find_fuzzy_match(
        &self,
        dictionary: &serde_json::Value,
        word: &str,
    ) -> Option<DictionaryResponse> {
        let max_distance = 2;
        let mut closest_match = None;
        let mut min_distance = max_distance + 1;

        // Find the closest match within our threshold
        if let Some(entries) = dictionary.as_object() {
            for (dict_word, entry) in entries {
                let distance = self.levenshtein_distance(word, dict_word);
                if distance <= max_distance && distance < min_distance {
                    min_distance = distance;
                    closest_match = Some((dict_word.clone(), entry));
                }
            }
        }

        closest_match.map(|(matched_word, entry)| {
            self.parse_dictionary_entry(&matched_word, entry, Some(word))
        })
    }

    // Calculate Levenshtein distance between two strings
    fn levenshtein_distance(&self, s1: &str, s2: &str) -> usize {
        let len1 = s1.chars().count();
        let len2 = s2.chars().count();
        if len1 == 0 {
            return len2;
        }
        if len2 == 0 {
            return len1;
        }

        let s1_chars: Vec<char> = s1.chars().collect();
        let s2_chars: Vec<char> = s2.chars().collect();

        let mut matrix = vec![vec![0; len2 + 1]; len1 + 1];

        for i in 0..=len1 {
            matrix[i][0] = i;
        }
        for j in 0..=len2 {
            matrix[0][j] = j;
        }

        // Fill the matrix
        for i in 1..=len1 {
            for j in 1..=len2 {
                let cost = if s1_chars[i - 1] == s2_chars[j - 1] {
                    0
                } else {
                    1
                };
                matrix[i][j] = std::cmp::min(
                    std::cmp::min(matrix[i - 1][j] + 1, matrix[i][j - 1] + 1),
                    matrix[i - 1][j - 1] + cost,
                );
            }
        }

        matrix[len1][len2]
    }

    // Parse dictionary entry into DictionaryResponse format
    // TODO: return the correct word while fuzzy matching
    fn parse_dictionary_entry(
        &self,
        word: &str,
        entry: &serde_json::Value,
        _original_query: Option<&str>,
    ) -> DictionaryResponse {
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

        DictionaryResponse {
            word: word.to_string(),
            meanings,
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
                    eprintln!("{}", &word);
                    let markdown = formatting::format_definition_as_markdown_with_config(
                        &response.word,
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

        let content = self.get_document_content(&document_uri).await?;

        if let Some(word) = self.get_word_at_position(&content, position) {
            match self.get_meaning(&word).await {
                Ok(Some(response)) => Ok(Some(
                    self.create_signature_help_for_definition(&response.word, &response),
                )),
                Ok(None) => Ok(Some(
                    self.create_signature_help_for_missing_definition(&word),
                )),
                Err(_) => Ok(None),
            }
        } else {
            Ok(None)
        }
    }

    /// Retrieves document content either from the document map or by reading from disk
    async fn get_document_content(&self, document_uri: &Url) -> Result<String> {
        match self.document_map.lock().await.get(document_uri) {
            Some(content) => Ok(content.clone()),
            None => match std::fs::read_to_string(document_uri.path()) {
                Ok(content) => Ok(content),
                Err(_) => Err(tower_lsp::jsonrpc::Error::internal_error()),
            },
        }
    }

    /// Creates signature help object for a word with a definition
    fn create_signature_help_for_definition(
        &self,
        _word: &str,
        response: &DictionaryResponse,
    ) -> SignatureHelp {
        // Format the entire dictionary response as hover-like content
        let value = formatting::format_definition_as_markdown_with_config(
            &response.word,
            response,
            &self.config.formatting,
        );

        let signatures = vec![SignatureInformation {
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::PlainText,
                value: "".to_string(),
            })),
            label: value,
            parameters: None,
            active_parameter: None,
        }];

        SignatureHelp {
            signatures,
            active_signature: Some(0),
            active_parameter: None,
        }
    }

    /// Creates signature help object for a word without a definition
    fn create_signature_help_for_missing_definition(&self, word: &str) -> SignatureHelp {
        let signatures = vec![SignatureInformation {
            label: format!("No definition found for '{}'", word),
            documentation: None,
            parameters: None,
            active_parameter: None,
        }];

        SignatureHelp {
            signatures,
            active_signature: Some(0),
            active_parameter: None,
        }
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
