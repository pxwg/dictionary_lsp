# Dictionary LSP

A personal dictionary query tool, implemented with the LSP language server and `rust`.

## Introduction

<div align="center">
  <img src="./fig/showcase_hover.png" alt="textDocument/hover example" width="80%">
</div>

<div align="center">
  <img src="./fig/showcase_sig.png" alt="textDocument/signatureHelp example" width="80%">
</div>

Dictionary LSP is a dictionary query tool written in `rust` based on the LSP protocol. It can use `textDocument/hover` and `textDocument/signatureHelp` to help you quickly look up word definitions in editors that support the LSP protocol, such as Neovim. It also implements `texDocument/CompletionItem` with simple fuzzy matching for autocomplete (inspired by [blink-cmp-dictionary](https://github.com/Kaiser-Yang/blink-cmp-dictionary), though currently with slow response and in need of improved fuzzy matching speed and accuracy).

**Give it a try!** Most mainstream editors now have simple LSP support. LSP-based plugins will help you reduce dependency on editor-specific plugins, allowing you to look up word definitions using the editor's native features (which is usually the most intuitive way!).

This is a project that will continuously update as the author grows. More features based on LSP characteristics will be added in the future 😆

## Installation, Usage and Configuration

Simply clone this project and compile it with `cargo build --release`. We don't provide any dictionary files; you'll need to download them from elsewhere and convert them to a structure like:

```json
"passion": {
  "noun": [
    "激情，酷爱，热爱，强烈感情，耶稣受难 (故事)"
  ]
}
```

Place this file at `~/dicts/dictionary.json` (the default dictionary storage location) to complete the configuration. Since JSON file parsing requires poor IO performance (thus we don't support fuzzy search for JSON dictionary sources), we also provide SQLite database support. You can convert your dictionary to a SQLite database and place it at `~/dicts/dictionary.db`. For conversion methods, refer to [#1](https://github.com/pxwg/dictionary_lsp/issues/1).

If you want to configure preview styles, dictionary paths, etc., you can use (these may not be default configurations):
```toml
# ~/.config/dictionary_lsp/config.toml
dictionary_path = "/path/to/your/dictionary.json" # JSON supported dictionary
freq_path = "/path/to/your/freq.db" # frequency database for auto completion and fuzzy search ordered by frequency
# dictionary_path = "/path/to/your/dictionary.db" # SQLite supported dictionary
[formatting]
word_format = "**{word}**"
part_of_speech_format = "*{part}*"
definition_format = "{num}. {definition}"
example_format = "> *{example}*"
add_spacing = true
[completion]
max_distance = 2 # Maximum distance for fuzzy search
enabled = true
# TODO: better fuzzy search algorithm and more configurations
```
The content in `{}` will be passed to variables.

Different clients have different methods to configure LSP. For [neovim](https://github.com/neovim/neovim) with [nvim-lspconfig](https://github.com/neovim/nvim-lspconfig) installed, you can use:
```lua
      local configs = require("lspconfig.configs")
      local lspconfig = require("lspconfig")
      if not configs.dictionary then
        configs.dictionary = {
          default_config = {
            filetypes = { "markdown" },
            cmd = { vim.fn.expand("$HOME/dictionary_lsp/target/release/dictionary_lsp") },
            root_dir = function(fname)
              local startpath = fname
              return vim.fs.dirname(vim.fs.find(".git", { path = startpath, upward = true })[1]) or vim.fn.getcwd()
            end,
          },
        }
      end

      -- Then set it up
      lspconfig.dictionary.setup({})
```
Place this in your `init.lua` to use it. For other editors, refer to the configuration methods of your corresponding LSP plugins.

You can send `textDocument/executeCommand` command `dictionary.enable_cmp` to control configurations that may hinder quick lookups, such as disabling autocomplete. If you have a very fast LSP source and don't want to be hindered by this LSP during autocomplete, you can make good use of this command to reconcile the conflict between dictionary lookups and quick completions.

## Reference Data Sources

We intentionally don't provide dictionary data sources because they are very large datasets, and we don't want to include them in the project. If you need dictionary data sources, you can refer to the following (all under MIT license):

- [ECDICT](https://github.com/skywind3000/ECDICT) provides a CSV database containing many words with different parts of speech. You need to convert it to JSON or SQLite database. Note that this data source contains many non-word data, and currently we can't implement phrase lookup. This feature will be implemented later;
- [Natural Language Corpus Data: Beautiful Data](https://norvig.com/ngrams/) provides the correspondence between words and frequencies, which can be used as a small-scale database for autocompletion and indexing. The given word frequencies are weighted when searching in the SQLite database.

## TODO

Here are some things I want to do; particularly desired ones are marked with ⭐. If you have any ideas or suggestions, feel free to submit an issue or PR.

- [x] Basic word lookup functionality
- [x] Custom text format for textDocument/hover responses⭐
- [x] Support for textDocument/signatureHelp requests⭐(basic support)
- [x] Fuzzy search (initially completed, will be based on more popular fuzzy matching libraries later, but this also depends on SQLite implementation)  
- [x] Autocomplete⭐(now implemented frequency-based autocomplete, at the cost of an extra SQLite database. Using small libraries to trade space for time)
    - [ ] Add more intuitive autocomplete modes, including
      - [ ] Case matching
      - [ ] Root word matching
      - [ ] Fuzzy matching beyond frequency-based search
- [ ] Add unit tests⭐(particularly desired, but may require code refactoring, abstracting specific business logic)
- [x] Configuration file specifying dictionary location
- [x] Support for SQLite database⭐
- [ ] Phrase lookup
- [ ] Support for dictionary conversion from CSV and other formats
- [ ] More powerful fuzzy matching algorithms⭐(SIMD acceleration? Affine Levenshtein distance? Can try both, shouldn't be limited to SQLite's fuzzy matching)
- [ ] Implement Neovim compatibility layer to actively add new words during file editing, track query frequency, etc. (highly dependent on SQLite implementation)⭐⭐⭐(very desired! but workload is rather large)

## Background

As someone with an English vocabulary far from sufficient for my reading needs, I have always hoped to be able to quickly look up words when taking English notes in Neovim. This would help me quickly understand English definitions when writing notes (for example, during English class or when reading literature, I can directly search for words I hear) and reduce vocabulary barriers when reading documents (mostly in MD format, usually read using Neovim).

When writing code with Neovim, the built-in `textDocument/hover` request of LSP helps me quickly query function/variable names through LSP (by default using the `K` command in Neovim). This inspired me to implement dictionary functionality using LSP, using a unified method to look up word definitions as if looking up variable names. In Neovim's insert mode, the `textDocument/signatureHelp` feature can be used to query variable names (by default using `<C-S>`), which can similarly help me query word definitions during writing. These two features nicely simulate our mental model for word lookup and can easily be integrated into the workflow of LSP-compatible editors. At the same time, simple autocompletion can help me better handle word queries and spelling issues during writing, and implement simple dictionary query functions.

This functionality is relatively easy to implement, so I'm trying to use Rust to become familiar with its development process.
