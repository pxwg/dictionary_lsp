use crate::config::Config;
use crate::dictionary_data::{create_dictionary_provider, DictionaryProvider};
use crate::formatting;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind, Url};

pub struct HoverHandler {
  document_map: Arc<Mutex<HashMap<Url, String>>>,
  dictionary_provider: Box<dyn DictionaryProvider>,
  config: Config,
}

impl HoverHandler {
  pub fn new(
    document_map: Arc<Mutex<HashMap<Url, String>>>,
    dictionary_path: String,
    config: Config,
  ) -> Self {
    Self {
      document_map,
      dictionary_provider: create_dictionary_provider(Some(dictionary_path)),
      config,
    }
  }

  /// Handles hover events by finding the word at the cursor position
  /// and fetching its dictionary definition.
  pub async fn on_hover(&self, params: HoverParams) -> Result<Option<Hover>> {
    let position = params.text_document_position_params.position;
    let document_uri = params.text_document_position_params.text_document.uri;

    let content = match self.document_map.lock().await.get(&document_uri) {
      Some(content) => content.clone(),
      None => match std::fs::read_to_string(document_uri.path()) {
        Ok(content) => content,
        Err(_) => return Ok(None),
      },
    };

    // Extract the word at position and look up its meaning
    if let Some(word) = self
      .dictionary_provider
      .get_word_at_position(&content, position)
    {
      match self.dictionary_provider.get_meaning(&word).await {
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
}
