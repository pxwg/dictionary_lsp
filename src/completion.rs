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

  /// Completion request handler
  /// ### expected behavior
  /// - If the document is not found, return None
  /// - If the current word is less than 2 characters, return None (boost performance)
  /// - If the provider is available, use it to find words by prefix
  /// wo -> word, world etc.
  /// Wo -> Word, World etc. (respect capitalization)
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

    // Use the existing provider (which might be our mock in tests) if available,
    // otherwise create a new one
    let words = if let Some(provider) = &self.provider {
      match provider.find_words_by_prefix(&current_word).await {
        Ok(Some(words)) => words,
        _ => return Ok(None),
      }
    } else {
      let provider = dictionary_data::create_dictionary_provider(
        Some(self.dictionary_path.clone()),
        Some(self.freq_path.clone()),
      );
      match provider.find_words_by_prefix(&current_word).await {
        Ok(Some(words)) => words,
        _ => return Ok(None),
      }
    };

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

    // Check if the first letter of current_word is uppercase
    let starts_with_uppercase = current_word
      .chars()
      .next()
      .map_or(false, |c| c.is_uppercase());

    // Process words and their meanings
    for (word, meaning) in word_meanings {
      // Apply capitalization if needed
      // Apply capitalization if needed - simplified logic
      let final_word = if starts_with_uppercase && !word.is_empty() {
        let mut capitalized = word.to_string();
        if let Some(first_char) = capitalized.get_mut(0..1) {
          first_char.make_ascii_uppercase();
        }
        capitalized
      } else {
        word.clone()
      };

      let text_edit = TextEdit {
        range: Range {
          start: Position {
            line: position.line,
            character: start_pos,
          },
          end: position,
        },
        new_text: final_word.clone(),
      };

      // let data = serde_json::to_value(word.clone()).unwrap_or_default();

      // Create completion item with pre-fetched documentation if available
      let mut item = CompletionItem {
        label: final_word.clone(),
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

  /// Extracts the current word and its starting position from the content at the given position
  /// This function is enhaced to support CJK characters
  /// ### expected behavior
  /// 你好check$%^&你好你好test <---cursor plased here
  /// should return "test" and the start position of "test" in the line
  /// No need for the space between "你好" and "check" to be considered as a word
  async fn get_current_word_and_start(
    &self,
    content: &str,
    position: Position,
  ) -> Option<(String, u32)> {
    // Get the line at the cursor position
    let line = content.lines().nth(position.line as usize)?;

    // Convert cursor position to byte index
    let char_pos = position.character as usize;
    let mut char_indices = line.char_indices();
    let before_cursor_end = if char_pos == 0 {
      0
    } else {
      // Find the byte index for the character at position
      let mut last_idx = 0;
      let mut last_char_len = 0;

      for _ in 0..char_pos {
        if let Some((idx, c)) = char_indices.next() {
          last_idx = idx;
          last_char_len = c.len_utf8();
        } else {
          return None; // Position is out of bounds
        }
      }
      last_idx + last_char_len
    };

    // Get text before cursor
    let before_cursor = &line[..before_cursor_end];

    // First, check for Chinese+English pattern
    if let Some((english_part, start_char_count)) =
      self.extract_english_after_chinese(before_cursor)
    {
      return Some((english_part, start_char_count));
    }

    // If the last character is CJK, return None
    if let Some(last_char) = before_cursor.chars().last() {
      if dictionary_data::is_cjk_char(last_char) {
        return None;
      }
    }

    // Extract alphabetic word before cursor
    self.extract_alphabetic_word_before_cursor(before_cursor, line)
  }

  // Helper method to extract English part after Chinese characters
  fn extract_english_after_chinese(&self, text: &str) -> Option<(String, u32)> {
    let mut english_start_idx = None;
    let mut has_chinese = false;

    // Scan from left to right
    for (i, c) in text.char_indices() {
      if dictionary_data::is_cjk_char(c) {
        has_chinese = true;
        english_start_idx = None; // Reset on new Chinese character
      } else if has_chinese && c.is_alphabetic() && english_start_idx.is_none() {
        english_start_idx = Some(i);
      }
    }

    // If we found English after Chinese
    if let Some(start_idx) = english_start_idx {
      let english_part = &text[start_idx..];
      if english_part.chars().all(|c| c.is_alphabetic()) && !english_part.is_empty() {
        let start_char_count = text[..start_idx].chars().count() as u32;
        return Some((english_part.to_string(), start_char_count));
      }
    }

    None
  }

  // Helper method to extract alphabetic word before cursor
  fn extract_alphabetic_word_before_cursor(
    &self,
    before_cursor: &str,
    line: &str,
  ) -> Option<(String, u32)> {
    if before_cursor.is_empty() {
      return None;
    }

    let mut start_byte_idx = before_cursor.len();
    let mut word_chars = Vec::new();

    // Scan backwards to find the word
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

/////// Tests ///////
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
    let dict_path = "./test/test_dict.db".to_string();
    let freq_path = "./test/test_freq.db".to_string();

    let handler = CompletionHandler::new(document_map, dict_path.clone(), freq_path.clone());
    #[cfg(test)]
    assert_eq!(handler.get_paths(), (dict_path, freq_path));
    handler
  }

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

  async fn test_complete_end_to_end_workflow(test_prefix: &str, expected_results: Vec<String>) {
    let mut mock_dict = MockDictionaryProvider::new();
    let expectation = expected_results.clone();
    let test_prefix = test_prefix.to_string();

    mock_dict
      .expect_find_words_by_prefix()
      .with(mockall::predicate::eq(test_prefix.clone()))
      .times(1)
      .returning(move |_| Ok(Some(expected_results.clone())));

    let document_map = Arc::new(Mutex::new(HashMap::new()));
    let test_uri = Url::parse("file:///test.txt").unwrap();
    let test_content = format!("Hello &@^#(!(**@*@^#@&@^#)_+_|/?;><>>{}", test_prefix).to_string();
    document_map
      .lock()
      .await
      .insert(test_uri.clone(), test_content.clone());

    let dict_path = "test_dict.db".to_string();
    let freq_path = "test_freq.db".to_string();
    let mut handler = CompletionHandler::new(document_map, dict_path, freq_path);

    handler.provider = Some(Box::new(mock_dict));

    let params = CompletionParams {
      text_document_position: TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: test_uri },
        position: Position {
          line: 0,
          character: test_content.chars().count() as u32,
        },
      },
      context: Some(CompletionContext {
        trigger_kind: CompletionTriggerKind::INVOKED,
        trigger_character: None,
      }),
      work_done_progress_params: WorkDoneProgressParams::default(),
      partial_result_params: PartialResultParams::default(),
    };

    let completion_response = handler.on_completion(params).await;

    match completion_response {
      Ok(Some(CompletionResponse::List(list))) => {
        assert_eq!(list.items.len(), 2);

        let labels: Vec<&str> = list.items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, expectation);

        for item in list.items {
          if let Some(CompletionTextEdit::Edit(edit)) = item.text_edit {
            assert_eq!(
              edit.range.start.character,
              test_content.chars().count() as u32 - test_prefix.chars().count() as u32,
            );
            assert_eq!(
              edit.range.end.character,
              test_content.chars().count() as u32
            );
          } else {
            panic!("Expected CompletionTextEdit::Edit");
          }
        }
      }
      _ => panic!("Expected CompletionResponse::List"),
    }
  }
  /// Test the end-to-end workflow for completion
  /// ### expected behavior
  /// test prefix "wo" should return "word" and "world"
  /// test prefix "Wo" should return "Word" and "World"
  /// Note: there is no need to test: "WO" because it would be the same as "wo" or refers to a
  /// specific appr.
  #[tokio::test]
  async fn test_complete() {
    test_complete_end_to_end_workflow("wor", vec!["word".to_string(), "world".to_string()]).await;
    test_complete_end_to_end_workflow("Wo", vec!["Word".to_string(), "World".to_string()]).await;
  }
}
