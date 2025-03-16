pub mod config;
pub mod dictionary_lsp;
pub mod formatting;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load config
    let config = config::Config::get();

    dictionary_lsp::run_server();

    Ok(())
}
