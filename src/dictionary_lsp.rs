use crate::completion::CompletionHandler;
use crate::config::Config;
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
  completion_handler: CompletionHandler,
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

  /// Processes signature help requests by looking up dictionary definitions for the word under the cursor.
  async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
    self.signature_help_handler.on_signature_help(params).await
  }

  /// Processes completion requests by looking up dictionary definitions for the word under the cursor.
  async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
    self.completion_handler.on_completion(params).await
  }
}

impl DictionaryLsp {
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

    let completion_handler = CompletionHandler::new(
      document_map.clone(),
      config
        .dictionary_path
        .clone()
        .expect("Dictionary path must be set"),
      config.clone(),
    );

    DictionaryLsp {
      client,
      document_map,
      config: config.clone(),
      hover_handler,
      signature_help_handler,
      completion_handler,
    }
  });

  Server::new(stdin, stdout, socket).serve(service).await;
}
