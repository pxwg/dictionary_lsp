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
  provider: Option<Box<dyn DictionaryProvider + Send + Sync>>,
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
      provider: None,
    }
  }

  #[cfg(test)]
  pub fn get_paths(&self) -> (String, String) {
    (self.dictionary_path.clone(), self.freq_path.clone())
  }

  #[cfg(test)]
  pub fn with_provider(
    mut self,
    provider: impl DictionaryProvider + Send + Sync + 'static,
  ) -> Self {
    self.provider = Some(Box::new(provider));
    self
  }

  async fn create_completion_items(
    &self,
    words: Vec<String>,
    word_start: u32,
  ) -> CompletionResponse {
    let items = words
      .into_iter()
      .map(|word| CompletionItem {
        label: word.clone(),
        kind: Some(CompletionItemKind::TEXT),
        detail: None,
        documentation: None, // We'll get this on resolve
        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
          range: Range {
            start: Position {
              line: 0,
              character: word_start,
            },
            end: Position {
              line: 0,
              character: word_start,
            },
          },
          new_text: word,
        })),
        data: None,
        ..Default::default()
      })
      .collect::<Vec<_>>();

    CompletionResponse::List(CompletionList {
      is_incomplete: false,
      items,
    })
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

    let provider = dictionary_data::create_dictionary_provider(
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

    // Check if the string has Chinese+English pattern
    let mut english_start_idx = None;
    let mut has_chinese = false;

    // Scan from left to right to find English characters after Chinese
    for (i, c) in before_cursor.char_indices() {
      if dictionary_data::is_cjk_char(c) {
        has_chinese = true;
        english_start_idx = None; // Reset if we find another Chinese character
      } else if has_chinese && c.is_alphabetic() && english_start_idx.is_none() {
        // Found first English character after Chinese
        english_start_idx = Some(i);
      }
    }

    // If we found a Chinese+English pattern, return just the English part
    if let Some(start_idx) = english_start_idx {
      let english_part = &before_cursor[start_idx..];
      // Make sure it only contains alphabetic characters
      if english_part.chars().all(|c| c.is_alphabetic()) && !english_part.is_empty() {
        // Count characters (not bytes) before the start of English text
        let start_char_count = line[..start_idx].chars().count() as u32;
        return Some((english_part.to_string(), start_char_count));
      }
    }

    // If no Chinese+English pattern is found, proceed with the original logic
    if !before_cursor.is_empty() {
      if let Some(last_char) = before_cursor.chars().last() {
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::dictionary_data::{DictionaryProvider, DictionaryResponse};
  use mockall::mock;
  use mockall::predicate::*;
  // Mock dictionary provider for testing
  mock! {
    DictionaryProvider {}
    #[async_trait::async_trait]
    impl DictionaryProvider for DictionaryProvider {
  async fn get_meaning(&self, word: &str) -> Result<Option<DictionaryResponse>>;
  fn get_word_at_position(&self, content: &str, position: Position) -> Option<String>;
  async fn find_words_by_prefix(&self, prefix: &str) -> Result<Option<Vec<String>>>;
    }
  }

  fn setup_test_handler() -> CompletionHandler {
    let document_map = Arc::new(Mutex::new(HashMap::new()));
    let dict_path = "test_dict.db".to_string();
    let freq_path = "test_freq.db".to_string();

    let handler = CompletionHandler::new(document_map, dict_path.clone(), freq_path.clone());
    #[cfg(test)]
    assert_eq!(handler.get_paths(), (dict_path, freq_path));
    handler
  }

  // Test empty line
  #[tokio::test]
  async fn test_get_current_word_and_start_empty_line() {
    let handler = setup_test_handler();
    let content = "";
    let position = Position {
      line: 0,
      character: 0,
    };

    let result = handler.get_current_word_and_start(content, position).await;
    assert_eq!(result, None);
  }

  // support for CJK characters
  #[tokio::test]
  async fn test_get_current_word_after_non_utf8_word() {
    let handler = setup_test_handler();
    let content = "你好时间jakdbdwj啊快速导航test再见是建设单位 word";
    let position = Position {
      line: 0,
      character: 21,
    };
    let position_1 = Position {
      line: 0,
      character: 23,
    };

    // should extract the last word "word"
    let result = handler.get_current_word_and_start(content, position).await;
    // if the cursor is at the CJK characters, should return None
    let result_1 = handler
      .get_current_word_and_start(content, position_1)
      .await;
    assert_eq!(result, Some(("test".to_string(), 17)));
    assert_eq!(result_1, None);
  }

  #[tokio::test]
  async fn test_get_current_word_and_start_middle_of_text() {
    let handler = setup_test_handler();
    let content = "some text with multiple words";
    let position = Position {
      line: 0,
      character: 14,
    };

    let result = handler.get_current_word_and_start(content, position).await;
    assert_eq!(result, Some(("with".to_string(), 10)));
  }

  #[tokio::test]
  async fn test_on_completion_document_not_found() {
    let handler = setup_test_handler();
    let params = CompletionParams {
      text_document_position: TextDocumentPositionParams {
        text_document: TextDocumentIdentifier {
          uri: Url::parse("file:///nonexistent.txt").unwrap(),
        },
        position: Position {
          line: 0,
          character: 0,
        },
      },
      context: None,
      work_done_progress_params: WorkDoneProgressParams::default(),
      partial_result_params: PartialResultParams::default(),
    };

    let result = handler.on_completion(params).await;
    assert_eq!(result.unwrap(), None);
  }

  #[tokio::test]
  async fn test_create_completion_items_directly() {
    let handler = setup_test_handler();

    let test_words = vec!["word".to_string(), "world".to_string()];
    let word_start = 6; // Start position for replacement

    let completion_response = handler
      .create_completion_items(test_words, word_start)
      .await;

    match completion_response {
      CompletionResponse::List(list) => {
        assert_eq!(list.items.len(), 2);

        let labels: Vec<&str> = list.items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels[0], "word");
        assert_eq!(labels[1], "world");
        for item in list.items {
          if let Some(CompletionTextEdit::Edit(edit)) = item.text_edit {
            assert_eq!(edit.range.start.character, word_start);
            assert_eq!(edit.range.end.character, word_start);
          } else {
            panic!("Expected CompletionTextEdit::Edit");
          }
        }
      }
      _ => panic!("Expected CompletionResponse::List"),
    }
  }

  #[tokio::test]
  async fn test_prefix_search() {
    let mut mock_dict = MockDictionaryProvider::new();

    let test_prefix = "wo";
    let expected_results = vec!["word".to_string(), "world".to_string()];

    mock_dict
      .expect_find_words_by_prefix()
      .with(mockall::predicate::eq(test_prefix))
      .times(1)
      .returning(move |_| Ok(Some(expected_results.clone())));

    let handler = setup_test_handler().with_provider(mock_dict);

    match &handler.provider {
      Some(provider) => {
        let result = provider.find_words_by_prefix(test_prefix).await;

        match result {
          Ok(Some(words)) => {
            assert_eq!(words.len(), 2);
            assert_eq!(words[0], "word");
            assert_eq!(words[1], "world");
          }
          Ok(None) => panic!("Expected words but got None"),
          Err(e) => panic!("Error finding words: {:?}", e),
        }
      }
      None => panic!("No provider available"),
    }
  }
}
