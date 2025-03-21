// use crate::completion::CompletionHandler;
use crate::config::Config;
use crate::dictionary_data::{self, DictionaryProvider};
use crate::hover::HoverHandler;
use crate::signature_help::SignatureHelpHandler;
use std::collections::HashMap;
use std::sync::Arc;
use tokio;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::lsp_types::{
  CompletionOptions, CompletionParams, CompletionResponse, Hover, HoverParams, SignatureHelp,
  SignatureHelpOptions, SignatureHelpParams, Url,
};
use tower_lsp::{Client, LanguageServer, LspService, Server};

pub struct DictionaryLsp {
  client: Client,
  document_map: Arc<Mutex<HashMap<Url, String>>>,
  pub config: Config,
  pub hover_handler: HoverHandler,
  signature_help_handler: SignatureHelpHandler,
}

#[tower_lsp::async_trait]
impl LanguageServer for DictionaryLsp {
  /// Initializes the language server and advertises server capabilities to the client.
  /// This includes what features we support, such as hover functionality.
  async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
    Ok(InitializeResult {
      capabilities: ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        signature_help_provider: Some(SignatureHelpOptions {
          trigger_characters: Some(vec![" ".to_string()]),
          retrigger_characters: Some(vec![" ".to_string()]),
          work_done_progress_options: Default::default(),
        }),
        completion_provider: Some(CompletionOptions {
          resolve_provider: Some(false),
          completion_item: Some(CompletionOptionsCompletionItem {
            label_details_support: Some(true),
          }),
          trigger_characters: Some(vec![" ".to_string()]),
          all_commit_characters: None,
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

    self
      .document_map
      .lock()
      .await
      .insert(uri.clone(), content.clone());

    self.analyze_document(uri, content).await;
  }

  /// Handles document content changes by updating the stored document and re-analyzing it.
  async fn did_change(&self, params: DidChangeTextDocumentParams) {
    let uri = params.text_document.uri.clone();
    if let Some(content) = self.document_map.lock().await.get_mut(&uri) {
      for change in params.content_changes {
        if change.range.is_none() {
          *content = change.text;
        }
      }
    }

    if let Some(content) = self.document_map.lock().await.get(&uri) {
      self.analyze_document(uri, content.clone()).await;
    }
  }

  async fn did_close(&self, params: DidCloseTextDocumentParams) {
    self
      .document_map
      .lock()
      .await
      .remove(&params.text_document.uri);
  }

  /// Handles the shutdown request from the client.
  async fn shutdown(&self) -> Result<()> {
    Ok(())
  }

  /// Processes hover requests by looking up dictionary definitions for the word under the cursor.
  async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
    self.hover_handler.on_hover(params).await
  }

  async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
    self.signature_help_handler.on_signature_help(params).await
  }

  //TODO: move the realization into the completion module and call it from here
  async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
    let document_uri = params.text_document_position.text_document.uri.clone();
    let position = params.text_document_position.position;

    if let Some(content) = self.document_map.lock().await.get(&document_uri) {
      if let Some((current_word, start_pos)) =
        self.get_current_word_and_start(content, position).await
      {
        let provider =
          dictionary_data::SqliteDictionaryProvider::new(self.config.dictionary_path.clone());

        if let Ok(Some(words)) = provider.find_words_by_prefix(&current_word).await {
          let mut items = Vec::with_capacity(words.len());

          for word in words {
            let text_edit = TextEdit {
              range: Range {
                start: Position {
                  line: position.line,
                  character: start_pos,
                },
                end: position,
              },
              new_text: word.clone(),
            };
            let mut definition = None;

            if let Ok(Some(response)) = self
              .hover_handler
              .dictionary_provider
              .get_meaning(&word)
              .await
            {
              let markdown = crate::formatting::format_definition_as_markdown_with_config(
                &response.word,
                &response,
                &self.config.formatting,
              );

              definition = Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: markdown,
              }));
            }

            // Create completion item
            let item = CompletionItem {
              label: word.clone(),
              // TODO: add more completion item details and optional config
              kind: Some(CompletionItemKind::PROPERTY),
              documentation: definition,
              text_edit: Some(CompletionTextEdit::Edit(text_edit)),
              ..Default::default()
            };

            items.push(item);
          }

          let list = CompletionList {
            is_incomplete: true,
            items,
          };

          return Ok(Some(CompletionResponse::List(list)));
        }
      }
    }

    Ok(None)
  }
}

impl DictionaryLsp {
  async fn get_current_word_and_start(
    &self,
    content: &str,
    position: Position,
  ) -> Option<(String, u32)> {
    let lines: Vec<&str> = content.lines().collect();
    if position.line as usize >= lines.len() {
      return None;
    }
    let line = lines[position.line as usize];

    // Convert character position to byte index correctly
    let char_pos = position.character as usize;
    let char_indices = line.char_indices().collect::<Vec<_>>();
    if char_pos > char_indices.len() {
      return None;
    }

    let before_cursor_end = if char_pos == 0 {
      0
    } else if char_pos <= char_indices.len() {
      char_indices[char_pos - 1].0 + char_indices[char_pos - 1].1.len_utf8()
    } else {
      line.len()
    };
    let before_cursor = &line[..before_cursor_end];
    if !before_cursor.is_empty() {
      if let Some(last_char) = before_cursor.chars().last() {
        // Check if character is in CJK Unified Ideographs range
        if dictionary_data::is_cjk_char(last_char) {
          return None;
        }
      }
    }

    let mut start_byte_idx = before_cursor_end;
    let mut word_chars = Vec::new();

    for (i, c) in before_cursor.char_indices().rev() {
      if !c.is_alphabetic() {
        break;
      }
      start_byte_idx = i;
      word_chars.push(c);
    }

    word_chars.reverse();
    let current_word: String = word_chars.into_iter().collect();

    if current_word.is_empty() {
      None
    } else {
      let start_char_count = line[..start_byte_idx].chars().count() as u32;
      Some((current_word, start_char_count))
    }
  }

  /// Analyzes a document for dictionary lookups and publishes diagnostics.
  /// This function extracts words from the content and checks them against the dictionary.
  async fn analyze_document(&self, uri: Url, content: String) {
    let words = self.parse_document(&content);
    let diagnostics = self.check_words(words).await;

    self
      .client
      .publish_diagnostics(uri, diagnostics, None)
      .await;
  }

  /// Extracts words from the document content by splitting on whitespace and
  /// normalizing them (removing punctuation, converting to lowercase).
  fn parse_document(&self, content: &str) -> Vec<String> {
    content
      .split_whitespace()
      .map(|word| {
        word
          .trim_matches(|c: char| !c.is_alphabetic())
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

  // /// Extracts the current word at the given position in the document
  // async fn get_current_word(&self, content: &str, position: Position) -> Option<String> {
  //   let lines: Vec<&str> = content.lines().collect();
  //
  //   // Check if position is valid
  //   if position.line as usize >= lines.len() {
  //     return None;
  //   }
  //
  //   let line = lines[position.line as usize];
  //   if position.character as usize > line.len() {
  //     return None;
  //   }
  //
  //   // Extract word at cursor
  //   let before_cursor = &line[..position.character as usize];
  //   let word_chars: String = before_cursor
  //     .chars()
  //     .rev()
  //     .take_while(|c| c.is_alphabetic())
  //     .collect::<Vec<_>>()
  //     .into_iter()
  //     .rev()
  //     .collect();
  //
  //   if word_chars.is_empty() {
  //     None
  //   } else {
  //     Some(word_chars)
  //   }
  // }
}

#[tokio::main]
pub async fn run_server() {
  let stdin = tokio::io::stdin();
  let stdout = tokio::io::stdout();

  let config = Config::get();

  // Create a shared document map wrapped in an Arc
  let document_map = Arc::new(Mutex::new(HashMap::<Url, String>::new()));

  let (service, socket) = LspService::new(|client| {
    let hover_handler = HoverHandler::new(
      document_map.clone(),
      config
        .dictionary_path
        .clone()
        .expect("Dictionary path must be set"),
      config.clone(),
    );

    let signature_help_handler = SignatureHelpHandler::new(
      document_map.clone(),
      config.dictionary_path.clone(),
      config.clone(),
    );

    DictionaryLsp {
      client,
      document_map,
      config: config.clone(),
      hover_handler,
      signature_help_handler,
    }
  });

  Server::new(stdin, stdout, socket).serve(service).await;
}
