pub mod completion;
pub mod config;
pub mod dictionary_data;
pub mod dictionary_lsp;
pub mod formatting;
pub mod fuzzy;
pub mod hover;
pub mod signature_help;

fn main() {
  dictionary_lsp::run_server();
}
