use crate::config::Config;
use crate::dictionary_data::{self, DictionaryProvider};
use crate::formatting::{self, FormattingConfig};
use futures;
use serde_json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

pub struct CompletionHandler {
  document_map: Arc<Mutex<HashMap<Url, String>>>,
  dictionary_path: String,
  freq_path: String,
}

impl CompletionHandler {
  pub fn new(
    document_map: Arc<Mutex<HashMap<Url, String>>>,
    dictionary_path: String,
    freq_path: String,
  ) -> Self {
    CompletionHandler {
      document_map,
      dictionary_path,
      freq_path,
    }
  }

  pub async fn on_completion(
    &self,
    params: CompletionParams,
  ) -> Result<Option<CompletionResponse>> {
    let document_uri = params.text_document_position.text_document.uri.clone();
    let position = params.text_document_position.position;

    let content = match self.document_map.lock().await.get(&document_uri) {
      Some(content) => content.clone(),
      None => return Ok(None),
    };

    let (current_word, start_pos) = match self.get_current_word_and_start(&content, position).await
    {
      Some(result) => result,
      None => return Ok(None),
    };

    // Early return for very short words (optional, can be configured)
    if current_word.len() < 2 {
      return Ok(None);
    }

    let provider = dictionary_data::SqliteDictionaryProvider::new(
      Some(self.dictionary_path.clone()),
      Some(self.freq_path.clone()),
    );

    let words = match provider.find_words_by_prefix(&current_word).await {
      Ok(Some(words)) => words,
      _ => return Ok(None),
    };

    if words.is_empty() {
      return Ok(None);
    }

    // Pre-allocate with capacity for better performance
    let mut items = Vec::with_capacity(words.len());

    // Fetch all definitions concurrently using futures
    use futures::future::join_all;
    let meaning_futures: Vec<_> = words
      .iter()
      .map(|word| {
        let provider_clone = dictionary_data::SqliteDictionaryProvider::new(
          Some(self.dictionary_path.clone()),
          Some(self.freq_path.clone()),
        );
        let word_clone = word.clone();
        async move {
          let meaning = provider_clone.get_meaning(&word_clone).await.ok().flatten();
          (word_clone, meaning)
        }
      })
      .collect();

    // Wait for all futures to complete
    let word_meanings = join_all(meaning_futures).await;

    // Process words and their meanings
    for (word, meaning) in word_meanings {
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

      // let data = serde_json::to_value(word.clone()).unwrap_or_default();

      // Create completion item with pre-fetched documentation if available
      let mut item = CompletionItem {
        label: word.clone(),
        kind: Some(CompletionItemKind::KEYWORD),
        text_edit: Some(CompletionTextEdit::Edit(text_edit)),
        // data: Some(data),
        ..Default::default()
      };

      if let Some(meaning) = meaning {
        let documentation = formatting::format_definition_as_markdown(&word, &meaning);

        if !documentation.is_empty() {
          item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: documentation,
          }));
        }
      }

      items.push(item);
    }

    let list = CompletionList {
      is_incomplete: items.len() >= Config::get().completion.max_distance as usize,
      items,
    };

    Ok(Some(CompletionResponse::List(list)))
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

  /// Resolves additional information for a completion item by fetching its definition
  pub async fn resolve_completion_item(&self, mut item: CompletionItem) -> Result<CompletionItem> {
    // Extract the word from the item's data
    if let Some(data) = &item.data {
      if let Ok(word) = serde_json::from_value::<String>(data.clone()) {
        // Create a provider to look up the definition
        let provider = dictionary_data::SqliteDictionaryProvider::new(
          Some(self.dictionary_path.clone()),
          Some(self.freq_path.clone()),
        );

        // Get the meaning for the word
        if let Ok(Some(meaning)) = provider.get_meaning(&word).await {
          let mut documentation = String::new();

          // Format the meaning into Markdown for the documentation
          for m in &meaning.meanings {
            documentation.push_str(&format!("### {}\n\n", m.part_of_speech));

            for (i, def) in m.definitions.iter().enumerate() {
              documentation.push_str(&format!("{}. {}\n", i + 1, def.definition));
              if let Some(example) = &def.example {
                documentation.push_str(&format!("> {}\n\n", example));
              }
            }
          }

          // Add the documentation to the completion item
          if !documentation.is_empty() {
            item.documentation = Some(Documentation::MarkupContent(MarkupContent {
              kind: MarkupKind::Markdown,
              value: documentation,
            }));
          }
        }
      }
    }

    Ok(item)
  }
}
