use crate::config::Config;
use crate::dictionary_data::{create_dictionary_provider, DictionaryProvider};
use crate::formatting;
use std::collections::HashMap;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::{Error, Result};
use tower_lsp::lsp_types::{
  Documentation, MarkupContent, MarkupKind, SignatureHelp, SignatureHelpParams,
  SignatureInformation, Url,
};

pub struct SignatureHelpHandler {
  document_map: Mutex<HashMap<Url, String>>,
  dictionary_loader: Box<dyn DictionaryProvider>,
  config: Config,
}

impl SignatureHelpHandler {
  pub fn new(
    document_map: Mutex<HashMap<Url, String>>,
    dictionary_path: Option<String>,
    config: Config,
  ) -> Self {
    Self {
      document_map,
      dictionary_loader: create_dictionary_provider(dictionary_path),
      config,
    }
  }

  /// Handles signature help requests by finding the word at the cursor position
  /// and providing its dictionary definition in signature help format.
  pub async fn on_signature_help(
    &self,
    params: SignatureHelpParams,
  ) -> Result<Option<SignatureHelp>> {
    let position = params.text_document_position_params.position;
    let document_uri = params.text_document_position_params.text_document.uri;

    let content = self.get_document_content(&document_uri).await?;

    if let Some(word) = self
      .dictionary_loader
      .get_word_at_position(&content, position)
    {
      match self.dictionary_loader.get_meaning(&word).await {
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
        Err(_) => Err(Error::internal_error()),
      },
    }
  }

  /// Creates signature help object for a word with a definition
  fn create_signature_help_for_definition(
    &self,
    _word: &str,
    response: &crate::dictionary_data::DictionaryResponse,
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
