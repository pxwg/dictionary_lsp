// use dictionary_lsp::completion::CompletionHandler;
use dictionary_lsp::config::Config;
use dictionary_lsp::dictionary_data::DictionaryProvider;
use futures::future::join_all;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
  CompletionParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Url,
};

struct MockDictionaryProvider;

#[async_trait::async_trait]
impl DictionaryProvider for MockDictionaryProvider {
  async fn find_words_by_prefix(&self, _prefix: &str) -> Result<Option<Vec<String>>> {
    Ok(Some(vec!["test".to_string(), "testing".to_string()]))
  }
  fn get_word_at_position(&self, content: &str, position: Position) -> Option<String> {
    dictionary_lsp::dictionary_data::extract_word_at_position(content, position)
  }

  async fn get_meaning(
    &self,
    _word: &str,
  ) -> Result<Option<dictionary_lsp::dictionary_data::DictionaryResponse>> {
    Ok(None)
  }
}

fn create_mock_dictionary_provider(_: Option<String>) -> Box<dyn DictionaryProvider> {
  Box::new(MockDictionaryProvider)
}

#[tokio::test]
async fn test_high_load_completion_requests() {
  // Create a document map with a test document
  let document_map = Arc::new(Mutex::new(HashMap::new()));
  let test_url = Url::parse("file:///tests/test.txt").unwrap();

  // Use a longer document to support multiple positions
  let test_document =
    "This is a test document with enough content for multiple positions".to_string();
  document_map
    .lock()
    .await
    .insert(test_url.clone(), test_document);

  // Create a config
  let config = Config::default();

  // Create a completion handler with our mock provider
  let mut handler = CompletionHandler::new(document_map, None, config);

  // Replace the dictionary loader with our mock
  let field = unsafe { &mut *(&mut handler.dictionary_loader as *mut Box<dyn DictionaryProvider>) };
  *field = create_mock_dictionary_provider(None);

  // Create multiple completion requests
  const NUM_REQUESTS: usize = 100;
  let mut futures = Vec::with_capacity(NUM_REQUESTS);

  for i in 0..NUM_REQUESTS {
    let params = CompletionParams {
      text_document_position: TextDocumentPositionParams {
        text_document: TextDocumentIdentifier {
          uri: test_url.clone(),
        },
        position: Position {
          line: 0,
          character: i as u32,
        },
      },
      work_done_progress_params: Default::default(),
      partial_result_params: Default::default(),
      context: None,
    };

    futures.push(handler.on_completion(params));
  }

  // Execute all requests concurrently
  let results = join_all(futures).await;

  // Verify we got the expected number of responses
  assert_eq!(results.len(), NUM_REQUESTS);

  // Check that each response contains completion items
  for result in results {
    match result {
      Ok(Some(response)) => match response {
        tower_lsp::lsp_types::CompletionResponse::Array(items) => {
          assert!(!items.is_empty(), "Completion response should not be empty");
        }
        _ => panic!("Expected CompletionResponse::Array"),
      },
      _ => panic!("Expected Some(CompletionResponse)"),
    }
  }
}
