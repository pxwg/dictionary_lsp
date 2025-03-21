use crate::config::Config;
use crate::dictionary_data::{self, DictionaryProvider};
use crate::formatting;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

pub struct CompletionHandler {
  document_map: Arc<Mutex<HashMap<Url, String>>>,
  dictionary_path: String,
  config: Config,
}

impl CompletionHandler {
  pub fn new(
    document_map: Arc<Mutex<HashMap<Url, String>>>,
    dictionary_path: String,
    config: Config,
  ) -> Self {
    CompletionHandler {
      document_map,
      dictionary_path,
      config,
    }
  }

  pub async fn on_completion(
    &self,
    params: CompletionParams,
  ) -> Result<Option<CompletionResponse>> {
    let document_uri = params.text_document_position.text_document.uri.clone();
    let position = params.text_document_position.position;

    if let Some(content) = self.document_map.lock().await.get(&document_uri) {
      if let Some((current_word, start_pos)) =
        self.get_current_word_and_start(content, position).await
      {
        let provider =
          dictionary_data::SqliteDictionaryProvider::new(Some(self.dictionary_path.clone()));

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

            if let Ok(Some(response)) = provider.get_meaning(&word).await {
              let markdown = formatting::format_definition_as_markdown_with_config(
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
              kind: Some(CompletionItemKind::KEYWORD),
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

    // convert character position to byte index correctly
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
        // check if character is in cjk unified ideographs range
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
}
