use crate::dictionary_data::DictionaryResponse;
use serde::{Deserialize, Serialize};

/// Configuration for markdown formatting styles
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FormattingConfig {
    /// Format for word title (e.g., "**{word}**")
    pub word_format: String,
    /// Format for part of speech (e.g., "_{part}_")
    pub part_of_speech_format: String,
    /// Format for definition numbering (e.g., "{num}. {definition}")
    pub definition_format: String,
    /// Format for examples (e.g., "   > Example: _{example}_")
    pub example_format: String,
    /// Whether to add extra spacing between parts of speech
    pub add_spacing: bool,
}

impl Default for FormattingConfig {
    fn default() -> Self {
        Self {
            word_format: "**{word}**".to_string(),
            part_of_speech_format: "_{part}_".to_string(),
            definition_format: "{num}. {definition}".to_string(),
            example_format: "   > Example: _{example}_".to_string(),
            add_spacing: false,
        }
    }
}

/// Formats a dictionary response as Markdown text with custom styling
pub fn format_definition_as_markdown_with_config(
    word: &str,
    response: &DictionaryResponse,
    config: &FormattingConfig,
) -> String {
    let mut markdown = config.word_format.replace("{word}", word) + "\n";

    for meaning in &response.meanings {
        if config.add_spacing {
            markdown.push('\n');
        }

        markdown.push_str(
            &config
                .part_of_speech_format
                .replace("{part}", &meaning.part_of_speech),
        );
        markdown.push('\n');

        for (i, definition) in meaning.definitions.iter().enumerate() {
            let num = i + 1;
            markdown.push_str(
                &config
                    .definition_format
                    .replace("{num}", &num.to_string())
                    .replace("{definition}", &definition.definition),
            );
            markdown.push('\n');

            if let Some(example) = &definition.example {
                markdown.push_str(&config.example_format.replace("{example}", example));
                markdown.push('\n');
            }
        }
    }

    markdown
}

/// Formats a dictionary response as Markdown text using default styling
pub fn format_definition_as_markdown(word: &str, response: &DictionaryResponse) -> String {
    format_definition_as_markdown_with_config(word, response, &FormattingConfig::default())
}
