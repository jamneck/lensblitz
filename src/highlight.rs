//! A small, allocation-light syntax highlighter.
//!
//! One hand-written lexer covers the C-like, script-like and data languages people put in
//! Markdown. It runs at hundreds of MB/s, needs no grammar files, and costs nothing at
//! startup — which is why it is used instead of a TextMate/tree-sitter engine.

/// Token classes. `Theme::syn` is indexed by these.
pub mod tok {
    pub const PLAIN: u16 = 0;
    pub const COMMENT: u16 = 1;
    pub const STRING: u16 = 2;
    pub const NUMBER: u16 = 3;
    pub const KEYWORD: u16 = 4;
    pub const TYPE: u16 = 5;
    pub const FUNC: u16 = 6;
    pub const CONST: u16 = 7;
    pub const PUNCT: u16 = 8;
    pub const ATTR: u16 = 9;
    pub const PROP: u16 = 10;
    pub const TAG: u16 = 11;
    pub const VAR: u16 = 12;
    pub const INS: u16 = 13;
    pub const DEL: u16 = 14;
    pub const META: u16 = 15;
}
use tok::*;

/// `(end_byte, token)` runs covering the whole input, in order.
pub type Tokens = Vec<(u32, u16)>;

struct Lang {
    line: &'static [&'static str],
    block: Option<(&'static str, &'static str)>,
    /// Quote characters that start a string.
    quotes: &'static str,
    keywords: &'static [&'static str],
    types: &'static [&'static str],
    consts: &'static [&'static str],
    flags: u32,
}

const NOCASE: u32 = 1; // keywords are case-insensitive
const PREPROC: u32 = 1 << 1; // `#include` style lines
const DOLLAR: u32 = 1 << 2; // `$var`
const AT: u32 = 1 << 3; // `@decorator`
const TICK_LIFETIME: u32 = 1 << 4; // `'a` is not a string
const TRIPLE: u32 = 1 << 5; // `"""` strings
const RUST_ATTR: u32 = 1 << 6; // `#[...]`
const BANG_MACRO: u32 = 1 << 7; // `name!(`
const DASH_IDENT: u32 = 1 << 8; // identifiers may contain `-`
const NO_CAPS_TYPES: u32 = 1 << 9; // do not treat Capitalised words as types
const MULTILINE_STR: u32 = 1 << 10; // strings may span lines

const C_CONSTS: &[&str] = &["NULL", "false", "nil", "null", "nullptr", "true", "undefined"];

const RUST: Lang = Lang {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: "\"'",
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn", "for",
        "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static",
        "struct", "super", "trait", "type", "union", "unsafe", "use", "where", "while", "yield",
    ],
    types: &[
        "Self", "bool", "char", "f32", "f64", "i128", "i16", "i32", "i64", "i8", "isize", "str", "u128", "u16", "u32",
        "u64", "u8", "usize",
    ],
    consts: &["Err", "None", "Ok", "Some", "false", "true"],
    flags: TICK_LIFETIME | RUST_ATTR | BANG_MACRO | MULTILINE_STR,
};

const C: Lang = Lang {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: "\"'",
    keywords: &[
        "alignas",
        "alignof",
        "auto",
        "break",
        "case",
        "catch",
        "class",
        "co_await",
        "co_return",
        "co_yield",
        "concept",
        "const",
        "const_cast",
        "constexpr",
        "continue",
        "decltype",
        "default",
        "delete",
        "do",
        "dynamic_cast",
        "else",
        "enum",
        "explicit",
        "export",
        "extern",
        "final",
        "for",
        "friend",
        "goto",
        "if",
        "inline",
        "mutable",
        "namespace",
        "new",
        "noexcept",
        "operator",
        "override",
        "private",
        "protected",
        "public",
        "register",
        "reinterpret_cast",
        "requires",
        "restrict",
        "return",
        "sizeof",
        "static",
        "static_assert",
        "static_cast",
        "struct",
        "switch",
        "template",
        "this",
        "throw",
        "try",
        "typedef",
        "typeid",
        "typename",
        "union",
        "using",
        "virtual",
        "volatile",
        "while",
    ],
    types: &[
        "bool", "char", "char16_t", "char32_t", "char8_t", "double", "float", "int", "int16_t", "int32_t", "int64_t",
        "int8_t", "long", "short", "signed", "size_t", "ssize_t", "uint16_t", "uint32_t", "uint64_t", "uint8_t",
        "unsigned", "void", "wchar_t",
    ],
    consts: C_CONSTS,
    flags: PREPROC,
};

const CSHARP: Lang = Lang {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: "\"'",
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "base",
        "break",
        "case",
        "catch",
        "checked",
        "class",
        "const",
        "continue",
        "default",
        "delegate",
        "do",
        "else",
        "enum",
        "event",
        "explicit",
        "extern",
        "finally",
        "fixed",
        "for",
        "foreach",
        "get",
        "goto",
        "if",
        "implicit",
        "in",
        "init",
        "interface",
        "internal",
        "is",
        "lock",
        "namespace",
        "new",
        "operator",
        "out",
        "override",
        "params",
        "partial",
        "private",
        "protected",
        "public",
        "readonly",
        "record",
        "ref",
        "return",
        "sealed",
        "set",
        "sizeof",
        "static",
        "struct",
        "switch",
        "this",
        "throw",
        "try",
        "typeof",
        "unchecked",
        "unsafe",
        "using",
        "var",
        "virtual",
        "volatile",
        "when",
        "where",
        "while",
        "yield",
    ],
    types: &[
        "bool", "byte", "char", "decimal", "double", "dynamic", "float", "int", "long", "object", "sbyte", "short",
        "string", "uint", "ulong", "ushort", "void",
    ],
    consts: C_CONSTS,
    flags: AT | PREPROC,
};

const JAVA: Lang = Lang {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: "\"'",
    keywords: &[
        "abstract",
        "actual",
        "annotation",
        "as",
        "assert",
        "break",
        "by",
        "case",
        "catch",
        "class",
        "companion",
        "const",
        "constructor",
        "continue",
        "data",
        "default",
        "do",
        "else",
        "enum",
        "expect",
        "extends",
        "final",
        "finally",
        "for",
        "fun",
        "if",
        "implements",
        "import",
        "in",
        "init",
        "inline",
        "instanceof",
        "interface",
        "internal",
        "is",
        "lateinit",
        "native",
        "new",
        "object",
        "open",
        "operator",
        "override",
        "package",
        "permits",
        "private",
        "protected",
        "public",
        "record",
        "return",
        "sealed",
        "static",
        "strictfp",
        "super",
        "suspend",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "transient",
        "try",
        "typealias",
        "val",
        "var",
        "volatile",
        "when",
        "while",
        "yield",
    ],
    types: &["boolean", "byte", "char", "double", "float", "int", "long", "short", "void"],
    consts: C_CONSTS,
    flags: AT | TRIPLE,
};

const SWIFT: Lang = Lang {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: "\"",
    keywords: &[
        "actor",
        "as",
        "associatedtype",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "continue",
        "convenience",
        "default",
        "defer",
        "deinit",
        "do",
        "else",
        "enum",
        "extension",
        "fallthrough",
        "fileprivate",
        "final",
        "for",
        "func",
        "guard",
        "if",
        "import",
        "in",
        "indirect",
        "init",
        "inout",
        "internal",
        "is",
        "lazy",
        "let",
        "mutating",
        "nonisolated",
        "open",
        "operator",
        "override",
        "private",
        "protocol",
        "public",
        "repeat",
        "required",
        "rethrows",
        "return",
        "self",
        "some",
        "static",
        "struct",
        "subscript",
        "super",
        "switch",
        "throw",
        "throws",
        "try",
        "typealias",
        "unowned",
        "var",
        "weak",
        "where",
        "while",
    ],
    types: &["Any", "Bool", "Character", "Double", "Float", "Int", "Self", "String", "Void"],
    consts: C_CONSTS,
    flags: AT | TRIPLE | PREPROC,
};

const GO: Lang = Lang {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: "\"'`",
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "type",
        "var",
    ],
    types: &[
        "any",
        "bool",
        "byte",
        "complex128",
        "complex64",
        "error",
        "float32",
        "float64",
        "int",
        "int16",
        "int32",
        "int64",
        "int8",
        "rune",
        "string",
        "uint",
        "uint16",
        "uint32",
        "uint64",
        "uint8",
        "uintptr",
    ],
    consts: &["false", "iota", "nil", "true"],
    flags: MULTILINE_STR,
};

const JS: Lang = Lang {
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: "\"'`",
    keywords: &[
        "abstract",
        "as",
        "asserts",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "declare",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "finally",
        "for",
        "from",
        "function",
        "get",
        "if",
        "implements",
        "import",
        "in",
        "infer",
        "instanceof",
        "interface",
        "is",
        "keyof",
        "let",
        "namespace",
        "new",
        "of",
        "override",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "satisfies",
        "set",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "try",
        "type",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    types: &["any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "unknown"],
    consts: &["Infinity", "NaN", "false", "null", "true", "undefined"],
    flags: AT | MULTILINE_STR,
};

const PYTHON: Lang = Lang {
    line: &["#"],
    block: None,
    quotes: "\"'",
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def", "del", "elif", "else",
        "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "match", "nonlocal", "not",
        "or", "pass", "raise", "return", "try", "while", "with", "yield",
    ],
    types: &["bool", "bytes", "dict", "float", "frozenset", "int", "list", "object", "set", "str", "tuple"],
    consts: &["False", "None", "True", "cls", "self"],
    flags: AT | TRIPLE,
};

const RUBY: Lang = Lang {
    line: &["#"],
    block: None,
    quotes: "\"'`",
    keywords: &[
        "alias",
        "and",
        "begin",
        "break",
        "case",
        "class",
        "def",
        "defined?",
        "do",
        "else",
        "elsif",
        "end",
        "ensure",
        "for",
        "if",
        "in",
        "include",
        "module",
        "next",
        "not",
        "or",
        "private",
        "protected",
        "public",
        "raise",
        "redo",
        "require",
        "require_relative",
        "rescue",
        "retry",
        "return",
        "self",
        "super",
        "then",
        "undef",
        "unless",
        "until",
        "when",
        "while",
        "yield",
    ],
    types: &[],
    consts: &["false", "nil", "true"],
    flags: AT | DOLLAR,
};

const PHP: Lang = Lang {
    line: &["//", "#"],
    block: Some(("/*", "*/")),
    quotes: "\"'",
    keywords: &[
        "abstract",
        "and",
        "as",
        "break",
        "case",
        "catch",
        "class",
        "clone",
        "const",
        "continue",
        "declare",
        "default",
        "do",
        "echo",
        "else",
        "elseif",
        "enum",
        "extends",
        "final",
        "finally",
        "fn",
        "for",
        "foreach",
        "function",
        "global",
        "if",
        "implements",
        "include",
        "instanceof",
        "interface",
        "match",
        "namespace",
        "new",
        "or",
        "private",
        "protected",
        "public",
        "readonly",
        "require",
        "return",
        "static",
        "switch",
        "throw",
        "trait",
        "try",
        "use",
        "var",
        "while",
        "yield",
    ],
    types: &["array", "bool", "float", "int", "mixed", "string", "void"],
    consts: C_CONSTS,
    flags: DOLLAR,
};

const SHELL: Lang = Lang {
    line: &["#"],
    block: None,
    quotes: "\"'`",
    keywords: &[
        "break", "case", "continue", "do", "done", "elif", "else", "esac", "exit", "export", "fi", "for", "function",
        "if", "in", "local", "readonly", "return", "select", "set", "shift", "source", "then", "unset", "until",
        "while",
    ],
    types: &[],
    consts: &["false", "true"],
    flags: DOLLAR | DASH_IDENT | NO_CAPS_TYPES | MULTILINE_STR,
};

const SQL: Lang = Lang {
    line: &["--"],
    block: Some(("/*", "*/")),
    quotes: "'\"`",
    keywords: &[
        "add",
        "all",
        "alter",
        "and",
        "any",
        "as",
        "asc",
        "begin",
        "between",
        "by",
        "cascade",
        "case",
        "check",
        "column",
        "commit",
        "constraint",
        "create",
        "cross",
        "database",
        "default",
        "delete",
        "desc",
        "distinct",
        "drop",
        "else",
        "end",
        "except",
        "exists",
        "explain",
        "foreign",
        "from",
        "full",
        "grant",
        "group",
        "having",
        "if",
        "ilike",
        "in",
        "index",
        "inner",
        "insert",
        "intersect",
        "into",
        "is",
        "join",
        "key",
        "left",
        "like",
        "limit",
        "materialized",
        "not",
        "offset",
        "on",
        "or",
        "order",
        "outer",
        "over",
        "partition",
        "primary",
        "references",
        "returning",
        "revoke",
        "right",
        "rollback",
        "schema",
        "select",
        "set",
        "table",
        "then",
        "transaction",
        "trigger",
        "truncate",
        "union",
        "unique",
        "update",
        "using",
        "values",
        "view",
        "when",
        "where",
        "window",
        "with",
    ],
    types: &[
        "bigint",
        "bigserial",
        "blob",
        "bool",
        "boolean",
        "bytea",
        "char",
        "date",
        "decimal",
        "double",
        "float",
        "int",
        "integer",
        "interval",
        "json",
        "jsonb",
        "numeric",
        "real",
        "serial",
        "smallint",
        "text",
        "time",
        "timestamp",
        "timestamptz",
        "uuid",
        "varchar",
    ],
    consts: &["false", "null", "true"],
    flags: NOCASE | NO_CAPS_TYPES,
};

const LUA: Lang = Lang {
    line: &["--"],
    block: Some(("--[[", "]]")),
    quotes: "\"'",
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in", "local", "not", "or",
        "repeat", "return", "then", "until", "while",
    ],
    types: &[],
    consts: &["false", "nil", "true"],
    flags: 0,
};

const HASKELL: Lang = Lang {
    line: &["--"],
    block: Some(("{-", "-}")),
    quotes: "\"",
    keywords: &[
        "case",
        "class",
        "data",
        "deriving",
        "do",
        "else",
        "forall",
        "if",
        "import",
        "in",
        "infix",
        "infixl",
        "infixr",
        "instance",
        "let",
        "module",
        "newtype",
        "of",
        "qualified",
        "then",
        "type",
        "where",
    ],
    types: &[],
    consts: &["False", "Nothing", "True"],
    flags: 0,
};

const ELIXIR: Lang = Lang {
    line: &["#"],
    block: None,
    quotes: "\"'",
    keywords: &[
        "after",
        "alias",
        "and",
        "case",
        "catch",
        "cond",
        "def",
        "defimpl",
        "defmacro",
        "defmodule",
        "defp",
        "defprotocol",
        "defstruct",
        "do",
        "else",
        "end",
        "fn",
        "for",
        "if",
        "import",
        "in",
        "not",
        "or",
        "quote",
        "raise",
        "receive",
        "require",
        "rescue",
        "try",
        "unless",
        "unquote",
        "use",
        "when",
        "with",
    ],
    types: &[],
    consts: &["false", "nil", "true"],
    flags: AT | TRIPLE,
};

const LISP: Lang = Lang {
    line: &[";"],
    block: None,
    quotes: "\"",
    keywords: &[
        "cond", "def", "define", "defmacro", "defn", "defun", "do", "fn", "if", "lambda", "let", "loop", "ns", "quote",
        "require", "setq", "when",
    ],
    types: &[],
    consts: &["false", "nil", "t", "true"],
    flags: DASH_IDENT | NO_CAPS_TYPES,
};

const HASHLANG: Lang = Lang {
    // Dockerfile, Makefile, Nix, HCL, R, Perl, PowerShell, CMake and friends.
    line: &["#"],
    block: None,
    quotes: "\"'",
    keywords: &[
        "ADD",
        "ARG",
        "CMD",
        "COPY",
        "ENTRYPOINT",
        "ENV",
        "EXPOSE",
        "FROM",
        "HEALTHCHECK",
        "LABEL",
        "RUN",
        "SHELL",
        "USER",
        "VOLUME",
        "WORKDIR",
        "data",
        "else",
        "for",
        "foreach",
        "function",
        "if",
        "import",
        "in",
        "include",
        "inherit",
        "let",
        "locals",
        "module",
        "output",
        "provider",
        "rec",
        "resource",
        "return",
        "terraform",
        "variable",
        "while",
        "with",
    ],
    types: &[],
    consts: &["FALSE", "NULL", "TRUE", "false", "null", "true"],
    flags: DOLLAR | NO_CAPS_TYPES | DASH_IDENT,
};

const CLIKE: Lang = Lang {
    // Reasonable defaults for brace languages without their own table (Scala, Dart, Zig, ...).
    line: &["//"],
    block: Some(("/*", "*/")),
    quotes: "\"'",
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "comptime",
        "const",
        "continue",
        "def",
        "default",
        "defer",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "extern",
        "final",
        "finally",
        "fn",
        "for",
        "fun",
        "func",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "interface",
        "is",
        "let",
        "match",
        "message",
        "mixin",
        "mut",
        "new",
        "object",
        "override",
        "package",
        "private",
        "protected",
        "pub",
        "public",
        "query",
        "return",
        "service",
        "static",
        "struct",
        "super",
        "switch",
        "this",
        "throw",
        "trait",
        "try",
        "type",
        "union",
        "val",
        "var",
        "while",
        "with",
        "yield",
    ],
    types: &[
        "bool", "double", "f32", "f64", "float", "i32", "i64", "int", "string", "u32", "u64", "u8", "usize", "void",
    ],
    consts: C_CONSTS,
    flags: AT,
};

enum Mode {
    Plain,
    /// Strings and numbers only, for languages we do not know.
    Minimal,
    Generic(&'static Lang),
    Shell,
    Json,
    Yaml,
    Toml,
    Markup,
    Css,
    Diff,
}

fn mode_for(lang: &str) -> Mode {
    let lang = lang.to_ascii_lowercase();
    match lang.as_str() {
        "" | "text" | "txt" | "plain" | "plaintext" | "output" | "log" | "ascii" | "mermaid" => Mode::Plain,
        "rust" | "rs" => Mode::Generic(&RUST),
        "c" | "h" | "cpp" | "c++" | "cc" | "cxx" | "hpp" | "objc" | "objective-c" | "arduino" | "glsl" | "hlsl"
        | "wgsl" | "cuda" => Mode::Generic(&C),
        "csharp" | "cs" | "c#" => Mode::Generic(&CSHARP),
        "java" | "kotlin" | "kt" | "kts" | "groovy" | "gradle" => Mode::Generic(&JAVA),
        "swift" => Mode::Generic(&SWIFT),
        "go" | "golang" => Mode::Generic(&GO),
        "javascript" | "js" | "jsx" | "mjs" | "cjs" | "node" | "typescript" | "ts" | "tsx" | "mts" | "cts" => {
            Mode::Generic(&JS)
        }
        "python" | "py" | "python3" | "py3" | "ipython" | "starlark" | "bazel" | "gdscript" => Mode::Generic(&PYTHON),
        "ruby" | "rb" | "crystal" => Mode::Generic(&RUBY),
        "php" => Mode::Generic(&PHP),
        "bash" | "sh" | "shell" | "zsh" | "fish" | "console" | "shell-session" | "shellsession" | "terminal"
        | "ksh" => Mode::Shell,
        "sql" | "mysql" | "postgres" | "postgresql" | "pgsql" | "sqlite" | "plsql" | "tsql" => Mode::Generic(&SQL),
        "lua" => Mode::Generic(&LUA),
        "haskell" | "hs" | "elm" | "purescript" => Mode::Generic(&HASKELL),
        "elixir" | "ex" | "exs" => Mode::Generic(&ELIXIR),
        "clojure" | "clj" | "cljs" | "edn" | "lisp" | "scheme" | "elisp" | "racket" | "commonlisp" => {
            Mode::Generic(&LISP)
        }
        "dockerfile" | "docker" | "makefile" | "make" | "mk" | "nix" | "hcl" | "terraform" | "tf" | "r" | "perl"
        | "pl" | "powershell" | "ps1" | "pwsh" | "cmake" | "julia" | "jl" | "nginx" | "awk" | "tcl" => {
            Mode::Generic(&HASHLANG)
        }
        "scala" | "dart" | "zig" | "graphql" | "gql" | "protobuf" | "proto" | "solidity" | "sol" | "v" | "verilog"
        | "d" | "nim" | "odin" | "vala" | "haxe" | "actionscript" | "thrift" | "prisma" => Mode::Generic(&CLIKE),
        "json" | "json5" | "jsonc" | "jsonl" | "ndjson" | "geojson" => Mode::Json,
        "yaml" | "yml" => Mode::Yaml,
        "toml" | "ini" | "cfg" | "conf" | "properties" | "env" | "dotenv" | "editorconfig" | "gitconfig" => Mode::Toml,
        "xml" | "html" | "htm" | "xhtml" | "svg" | "vue" | "svelte" | "plist" | "rss" | "xsl" | "astro" => Mode::Markup,
        "css" | "scss" | "sass" | "less" | "stylus" => Mode::Css,
        "diff" | "patch" => Mode::Diff,
        _ => Mode::Minimal,
    }
}

/// Tokenise `src` as `lang` (a fenced code block info string, e.g. `rust`).
pub fn highlight(lang: &str, src: &str) -> Tokens {
    let mut out = Out { toks: Vec::with_capacity(src.len() / 6 + 4) };
    match mode_for(lang) {
        Mode::Plain => {}
        Mode::Minimal => minimal(src, &mut out),
        Mode::Generic(l) => generic(l, src, &mut out),
        Mode::Shell => shell(src, &mut out),
        Mode::Json => json(src, &mut out),
        Mode::Yaml => yaml(src, &mut out),
        Mode::Toml => toml(src, &mut out),
        Mode::Markup => markup(src, &mut out),
        Mode::Css => css(src, &mut out),
        Mode::Diff => diff(src, &mut out),
    }
    out.push(src.len(), PLAIN);
    out.toks
}

struct Out {
    toks: Tokens,
}

impl Out {
    /// Mark everything up to `end` (exclusive) that is not yet covered as `tok`.
    fn push(&mut self, end: usize, tok: u16) {
        let end = end as u32;
        match self.toks.last_mut() {
            Some(last) if last.0 >= end => {}
            Some(last) if last.1 == tok => last.0 = end,
            _ => self.toks.push((end, tok)),
        }
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

fn line_end(b: &[u8], from: usize) -> usize {
    b[from..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| from + p)
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= b.len() {
        return None;
    }
    b[from..].windows(needle.len()).position(|w| w == needle).map(|p| from + p)
}

/// End of a quoted string starting at `i` (which holds the quote). Honours backslash escapes.
fn string_end(b: &[u8], i: usize, multiline: bool) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 1,
            b'\n' if !multiline => return j,
            c if c == q => return j + 1,
            _ => {}
        }
        j += 1;
    }
    b.len()
}

fn number_end(b: &[u8], i: usize) -> usize {
    let mut j = i;
    while j < b.len() {
        let c = b[j];
        let exp_sign =
            (c == b'+' || c == b'-') && matches!(b[j - 1], b'e' | b'E') && j > i && !b[i..j].starts_with(b"0x");
        let dot = c == b'.' && b.get(j + 1).is_some_and(|d| d.is_ascii_digit());
        if c.is_ascii_alphanumeric() || c == b'_' || dot || exp_sign {
            j += 1;
        } else {
            break;
        }
    }
    j
}

fn in_list(list: &[&str], word: &str) -> bool {
    list.binary_search(&word).is_ok()
}

fn generic(l: &Lang, src: &str, out: &mut Out) {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut line_start = true; // only whitespace seen on this line so far
    while i < n {
        let c = b[i];
        if c == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if c == b' ' || c == b'\t' || c == b'\r' {
            i += 1;
            continue;
        }
        let at_line_start = std::mem::replace(&mut line_start, false);

        if let Some((open, close)) = l.block
            && b[i..].starts_with(open.as_bytes())
        {
            out.push(i, PLAIN);
            i = find(b, i + open.len(), close.as_bytes()).map_or(n, |p| p + close.len());
            out.push(i, COMMENT);
            continue;
        }
        if l.line.iter().any(|p| b[i..].starts_with(p.as_bytes())) {
            out.push(i, PLAIN);
            i = line_end(b, i);
            out.push(i, COMMENT);
            continue;
        }
        if c == b'#' {
            if l.flags & RUST_ATTR != 0 && matches!(b.get(i + 1), Some(b'[') | Some(b'!')) {
                out.push(i, PLAIN);
                i = find(b, i, b"]").map_or(line_end(b, i), |p| p + 1).min(line_end(b, i));
                out.push(i, ATTR);
                continue;
            }
            if l.flags & PREPROC != 0 && at_line_start {
                out.push(i, PLAIN);
                i = line_end(b, i);
                out.push(i, ATTR);
                continue;
            }
        }
        if l.quotes.as_bytes().contains(&c) {
            out.push(i, PLAIN);
            if c == b'\'' && l.flags & TICK_LIFETIME != 0 {
                // `'a'`/`'\n'` are chars; `'a` is a lifetime.
                let close = if b.get(i + 1) == Some(&b'\\') {
                    find(b, i + 2, b"'").filter(|p| p - i <= 10)
                } else {
                    let ch_len = src[i + 1..].chars().next().map_or(1, char::len_utf8);
                    (b.get(i + 1 + ch_len) == Some(&b'\'')).then_some(i + 1 + ch_len)
                };
                match close {
                    Some(p) => {
                        i = p + 1;
                        out.push(i, STRING);
                    }
                    None => {
                        i += 1;
                        while i < n && is_ident(b[i]) {
                            i += 1;
                        }
                        out.push(i, ATTR);
                    }
                }
                continue;
            }
            let triple = [c, c, c];
            if l.flags & TRIPLE != 0 && b[i..].starts_with(&triple) {
                i = find(b, i + 3, &triple).map_or(n, |p| p + 3);
            } else {
                i = string_end(b, i, c == b'`' || l.flags & MULTILINE_STR != 0);
            }
            out.push(i, STRING);
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            out.push(i, PLAIN);
            i = number_end(b, i);
            out.push(i, NUMBER);
            continue;
        }
        if (c == b'@' && l.flags & AT != 0) || (c == b'$' && l.flags & DOLLAR != 0) {
            let tok = if c == b'@' { ATTR } else { VAR };
            let mut j = i + 1;
            if c == b'$' && b.get(j) == Some(&b'{') {
                j = find(b, j, b"}").map_or(j, |p| p + 1).min(line_end(b, j));
            } else {
                while j < n && (is_ident(b[j]) || (c == b'@' && b[j] == b'.')) {
                    j += 1;
                }
            }
            if j > i + 1 {
                out.push(i, PLAIN);
                i = j;
                out.push(i, tok);
                continue;
            }
        }
        if is_ident_start(c) {
            let start = i;
            i += 1;
            while i < n
                && (is_ident(b[i]) || (l.flags & DASH_IDENT != 0 && b[i] == b'-' && i + 1 < n && is_ident(b[i + 1])))
            {
                i += 1;
            }
            let word = &src[start..i];
            let lower;
            let key = if l.flags & NOCASE != 0 {
                lower = word.to_ascii_lowercase();
                lower.as_str()
            } else {
                word
            };
            let mut j = i;
            while j < n && b[j] == b' ' {
                j += 1;
            }
            let next = b.get(j).copied();
            let after_dot = start > 0 && b[start - 1] == b'.';
            let tok = if !after_dot && in_list(l.keywords, key) {
                KEYWORD
            } else if in_list(l.consts, key) {
                CONST
            } else if in_list(l.types, key) {
                TYPE
            } else if next == Some(b'(')
                || (l.flags & BANG_MACRO != 0 && b.get(i) == Some(&b'!') && b.get(i + 1) != Some(&b'='))
            {
                FUNC
            } else if l.flags & NO_CAPS_TYPES == 0 && word.as_bytes()[0].is_ascii_uppercase() {
                let shouty =
                    word.len() > 1 && word.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_');
                if shouty { CONST } else { TYPE }
            } else {
                PLAIN
            };
            out.push(start, PLAIN);
            out.push(i, tok);
            continue;
        }
        // Operators and punctuation.
        out.push(i, PLAIN);
        i += 1;
        out.push(i, PUNCT);
    }
}

fn minimal(src: &str, out: &mut Out) {
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            out.push(i, PLAIN);
            i = string_end(b, i, false);
            out.push(i, STRING);
        } else if c.is_ascii_digit() && (i == 0 || !is_ident(b[i - 1])) {
            out.push(i, PLAIN);
            i = number_end(b, i);
            out.push(i, NUMBER);
        } else {
            i += 1;
        }
    }
}

fn shell(src: &str, out: &mut Out) {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut cmd_pos = true; // the next word is a command name
    while i < n {
        let c = b[i];
        match c {
            b'\n' | b'|' | b';' | b'&' | b'(' => {
                cmd_pos = true;
                if c != b'\n' {
                    out.push(i, PLAIN);
                    out.push(i + 1, PUNCT);
                }
                i += 1;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'#' if i == 0 || b[i - 1].is_ascii_whitespace() => {
                out.push(i, PLAIN);
                i = line_end(b, i);
                out.push(i, COMMENT);
            }
            b'$' if cmd_pos && b.get(i + 1) == Some(&b' ') => {
                // A leading `$ ` prompt.
                out.push(i, PLAIN);
                i += 1;
                out.push(i, META);
            }
            b'"' | b'\'' | b'`' => {
                out.push(i, PLAIN);
                i = string_end(b, i, true);
                out.push(i, STRING);
                cmd_pos = false;
            }
            b'$' => {
                out.push(i, PLAIN);
                let mut j = i + 1;
                match b.get(j) {
                    Some(b'{') => j = find(b, j, b"}").map_or(j, |p| p + 1).min(line_end(b, j)),
                    Some(b'(') => {
                        // `$(` starts a command substitution; colour just the sigil.
                    }
                    _ => {
                        while j < n && (is_ident(b[j]) || matches!(b[j], b'?' | b'@' | b'#' | b'*') && j == i + 1) {
                            j += 1;
                        }
                    }
                }
                i = j.max(i + 1);
                out.push(i, VAR);
                cmd_pos = false;
            }
            b'-' if (i == 0 || b[i - 1].is_ascii_whitespace())
                && b.get(i + 1).is_some_and(|d| d.is_ascii_alphanumeric() || *d == b'-') =>
            {
                out.push(i, PLAIN);
                while i < n && !b[i].is_ascii_whitespace() && b[i] != b'=' {
                    i += 1;
                }
                out.push(i, ATTR);
                cmd_pos = false;
            }
            _ if is_ident_start(c) || c == b'.' || c == b'/' || c == b'~' => {
                let start = i;
                while i < n
                    && !b[i].is_ascii_whitespace()
                    && !matches!(b[i], b'|' | b';' | b'&' | b'(' | b')' | b'"' | b'\'' | b'$' | b'`')
                {
                    i += 1;
                }
                let word = &src[start..i];
                let assign = word.contains('=');
                let tok = if in_list(SHELL.keywords, word) {
                    KEYWORD
                } else if cmd_pos && !assign {
                    FUNC
                } else {
                    PLAIN
                };
                out.push(start, PLAIN);
                if assign && cmd_pos {
                    let eq = start + word.find('=').unwrap_or(0);
                    out.push(eq, VAR);
                    out.push(i, PLAIN);
                } else {
                    out.push(i, tok);
                }
                // `sudo`, `env`, keywords and assignments are followed by another command.
                cmd_pos = matches!(
                    word,
                    "sudo" | "env" | "exec" | "time" | "xargs" | "then" | "do" | "else" | "if" | "elif" | "while"
                ) || (assign && cmd_pos);
            }
            _ if c.is_ascii_digit() => {
                out.push(i, PLAIN);
                i = number_end(b, i);
                out.push(i, if cmd_pos { PLAIN } else { NUMBER });
                cmd_pos = false;
            }
            _ => {
                out.push(i, PLAIN);
                i += 1;
                out.push(i, PUNCT);
            }
        }
    }
}

fn json(src: &str, out: &mut Out) {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        let c = b[i];
        match c {
            b'"' | b'\'' => {
                let end = string_end(b, i, false);
                let mut j = end;
                while j < n && matches!(b[j], b' ' | b'\t') {
                    j += 1;
                }
                out.push(i, PLAIN);
                out.push(end, if b.get(j) == Some(&b':') { PROP } else { STRING });
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                out.push(i, PLAIN);
                i = line_end(b, i);
                out.push(i, COMMENT);
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                out.push(i, PLAIN);
                i = find(b, i + 2, b"*/").map_or(n, |p| p + 2);
                out.push(i, COMMENT);
            }
            b'-' | b'0'..=b'9' => {
                out.push(i, PLAIN);
                i = number_end(b, i + 1);
                out.push(i, NUMBER);
            }
            _ if is_ident_start(c) => {
                let start = i;
                while i < n && is_ident(b[i]) {
                    i += 1;
                }
                out.push(start, PLAIN);
                let lit = matches!(&src[start..i], "true" | "false" | "null" | "NaN" | "Infinity");
                out.push(i, if lit { CONST } else { PROP });
            }
            b'{' | b'}' | b'[' | b']' | b',' | b':' => {
                out.push(i, PLAIN);
                i += 1;
                out.push(i, PUNCT);
            }
            _ => i += 1,
        }
    }
}

/// Colour a scalar value (the right-hand side of `key: value` / `key = value`).
fn scalar(src: &str, start: usize, end: usize, out: &mut Out, comment: &[u8]) {
    let b = src.as_bytes();
    let mut i = start;
    while i < end {
        let c = b[i];
        if comment.contains(&c) && (i == start || b[i - 1].is_ascii_whitespace()) {
            out.push(i, PLAIN);
            out.push(end, COMMENT);
            return;
        }
        match c {
            b'"' | b'\'' => {
                out.push(i, PLAIN);
                i = string_end(b, i, false).min(end);
                out.push(i, STRING);
            }
            b' ' | b'\t' => i += 1,
            b'[' | b']' | b'{' | b'}' | b',' => {
                out.push(i, PLAIN);
                i += 1;
                out.push(i, PUNCT);
            }
            _ => {
                let ws = i;
                while i < end && !matches!(b[i], b' ' | b'\t' | b',' | b']' | b'}') {
                    i += 1;
                }
                let word = &src[ws..i];
                let tok = match word {
                    "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~" | "True" | "False" => CONST,
                    _ if word.as_bytes()[0].is_ascii_digit()
                        || (word.len() > 1
                            && matches!(word.as_bytes()[0], b'-' | b'+' | b'.')
                            && word.as_bytes()[1].is_ascii_digit()) =>
                    {
                        NUMBER
                    }
                    _ if word.starts_with('&') || word.starts_with('*') || word.starts_with('!') => ATTR,
                    _ if word.starts_with('$') => VAR,
                    _ => STRING,
                };
                out.push(ws, PLAIN);
                out.push(i, tok);
            }
        }
    }
}

fn yaml(src: &str, out: &mut Out) {
    let b = src.as_bytes();
    let mut pos = 0;
    for line in src.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let end = start + line.trim_end_matches(['\n', '\r']).len();
        let mut i = start;
        while i < end && matches!(b[i], b' ' | b'\t') {
            i += 1;
        }
        if i >= end {
            continue;
        }
        if b[i] == b'#' {
            out.push(i, PLAIN);
            out.push(end, COMMENT);
            continue;
        }
        if src[i..end].starts_with("---") || src[i..end].starts_with("...") {
            out.push(i, PLAIN);
            out.push(end, META);
            continue;
        }
        while i < end && b[i] == b'-' && b.get(i + 1).is_none_or(|c| matches!(c, b' ' | b'\n' | b'\r')) {
            out.push(i, PLAIN);
            out.push(i + 1, PUNCT);
            i += 1;
            while i < end && b[i] == b' ' {
                i += 1;
            }
        }
        // `key:` at the start of the line (not inside a quoted string).
        let rest = &src[i..end];
        let colon = if rest.starts_with(['"', '\'']) {
            let e = string_end(b, i, false).min(end);
            (b.get(e) == Some(&b':')).then_some(e)
        } else {
            rest.find(": ").or_else(|| rest.ends_with(':').then(|| rest.len() - 1)).map(|p| i + p)
        };
        match colon {
            Some(colon) if !rest.starts_with(['[', '{']) => {
                out.push(i, PLAIN);
                out.push(colon, PROP);
                out.push(colon + 1, PUNCT);
                scalar(src, colon + 1, end, out, b"#");
            }
            _ => scalar(src, i, end, out, b"#"),
        }
    }
}

fn toml(src: &str, out: &mut Out) {
    let b = src.as_bytes();
    let mut pos = 0;
    for line in src.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let end = start + line.trim_end_matches(['\n', '\r']).len();
        let mut i = start;
        while i < end && matches!(b[i], b' ' | b'\t') {
            i += 1;
        }
        if i >= end {
            continue;
        }
        match b[i] {
            b'#' | b';' => {
                out.push(i, PLAIN);
                out.push(end, COMMENT);
            }
            b'[' => {
                out.push(i, PLAIN);
                let close = find(b, i, b"]").map_or(end, |p| (p + 1).min(end));
                let close = if b.get(close) == Some(&b']') { close + 1 } else { close };
                out.push(close, TYPE);
                scalar(src, close, end, out, b"#;");
            }
            _ => match src[i..end].find('=') {
                Some(eq) => {
                    out.push(i, PLAIN);
                    out.push(i + src[i..i + eq].trim_end().len(), PROP);
                    out.push(i + eq, PLAIN);
                    out.push(i + eq + 1, PUNCT);
                    scalar(src, i + eq + 1, end, out, b"#");
                }
                None => scalar(src, i, end, out, b"#"),
            },
        }
    }
}

fn markup(src: &str, out: &mut Out) {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        if b[i] != b'<' {
            if b[i] == b'&' {
                let e = (i + 1..n.min(i + 12)).find(|&j| b[j] == b';');
                if let Some(e) = e {
                    out.push(i, PLAIN);
                    i = e + 1;
                    out.push(i, CONST);
                    continue;
                }
            }
            i += 1;
            continue;
        }
        out.push(i, PLAIN);
        if b[i..].starts_with(b"<!--") {
            i = find(b, i + 4, b"-->").map_or(n, |p| p + 3);
            out.push(i, COMMENT);
            continue;
        }
        if b[i..].starts_with(b"<!") || b[i..].starts_with(b"<?") {
            i = find(b, i, b">").map_or(n, |p| p + 1);
            out.push(i, META);
            continue;
        }
        // `<`, optional `/`, then the tag name.
        let mut j = i + 1;
        if b.get(j) == Some(&b'/') {
            j += 1;
        }
        if !b.get(j).is_some_and(|c| c.is_ascii_alphabetic()) {
            i += 1;
            continue;
        }
        out.push(j, PUNCT);
        while j < n && (is_ident(b[j]) || matches!(b[j], b'-' | b':' | b'.')) {
            j += 1;
        }
        out.push(j, TAG);
        // Attributes up to `>`.
        while j < n && b[j] != b'>' {
            let c = b[j];
            if c == b'"' || c == b'\'' {
                out.push(j, PLAIN);
                j = string_end(b, j, true);
                out.push(j, STRING);
            } else if is_ident_start(c) || c == b':' || c == b'@' {
                out.push(j, PLAIN);
                while j < n && (is_ident(b[j]) || matches!(b[j], b'-' | b':' | b'.' | b'@')) {
                    j += 1;
                }
                out.push(j, ATTR);
            } else if c == b'=' || c == b'/' {
                out.push(j, PLAIN);
                j += 1;
                out.push(j, PUNCT);
            } else {
                j += 1;
            }
        }
        out.push(j, PLAIN);
        i = (j + 1).min(n);
        out.push(i, PUNCT);
    }
}

fn css(src: &str, out: &mut Out) {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut depth = 0i32;
    let mut in_value = false;
    while i < n {
        let c = b[i];
        if b[i..].starts_with(b"/*") {
            out.push(i, PLAIN);
            i = find(b, i + 2, b"*/").map_or(n, |p| p + 2);
            out.push(i, COMMENT);
            continue;
        }
        if b[i..].starts_with(b"//") && (i == 0 || b[i - 1] != b':') {
            out.push(i, PLAIN);
            i = line_end(b, i);
            out.push(i, COMMENT);
            continue;
        }
        match c {
            b'"' | b'\'' => {
                out.push(i, PLAIN);
                i = string_end(b, i, false);
                out.push(i, STRING);
            }
            b'{' | b'}' | b';' | b':' | b',' | b'(' | b')' => {
                match c {
                    b'{' => {
                        depth += 1;
                        in_value = false;
                    }
                    b'}' => {
                        depth -= 1;
                        in_value = false;
                    }
                    b';' => in_value = false,
                    b':' if depth > 0 => in_value = true,
                    _ => {}
                }
                out.push(i, PLAIN);
                i += 1;
                out.push(i, PUNCT);
            }
            b'@' | b'$' | b'#' | b'.' | b'-' | b'!' if b.get(i + 1).is_some_and(|d| is_ident(*d) || *d == b'-') => {
                let start = i;
                i += 1;
                while i < n && (is_ident(b[i]) || b[i] == b'-') {
                    i += 1;
                }
                let tok = match c {
                    b'@' | b'!' => KEYWORD,
                    b'$' => VAR,
                    b'#' if in_value => CONST,
                    b'-' if b[start + 1].is_ascii_digit() => NUMBER,
                    b'-' if in_value => VAR,
                    b'-' => PROP,
                    _ if in_value => PLAIN,
                    _ => FUNC,
                };
                out.push(start, PLAIN);
                out.push(i, tok);
            }
            _ if c.is_ascii_digit() => {
                out.push(i, PLAIN);
                i = number_end(b, i);
                while i < n && (b[i].is_ascii_alphabetic() || b[i] == b'%') {
                    i += 1;
                }
                out.push(i, NUMBER);
            }
            _ if is_ident_start(c) => {
                let start = i;
                while i < n && (is_ident(b[i]) || b[i] == b'-') {
                    i += 1;
                }
                let mut j = i;
                while j < n && b[j] == b' ' {
                    j += 1;
                }
                let tok = if b.get(i) == Some(&b'(') {
                    FUNC
                } else if depth > 0 && !in_value && b.get(j) == Some(&b':') {
                    PROP
                } else if depth > 0 && in_value {
                    PLAIN
                } else {
                    TAG
                };
                out.push(start, PLAIN);
                out.push(i, tok);
            }
            _ => i += 1,
        }
    }
}

fn diff(src: &str, out: &mut Out) {
    let mut pos = 0;
    for line in src.split_inclusive('\n') {
        let end = pos + line.len();
        let tok = if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("diff ")
            || line.starts_with("index ")
        {
            META
        } else if line.starts_with("@@") {
            KEYWORD
        } else if line.starts_with('+') {
            INS
        } else if line.starts_with('-') {
            DEL
        } else {
            PLAIN
        };
        out.push(end, tok);
        pos = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(lang: &str, src: &str) -> Vec<(String, u16)> {
        let mut start = 0;
        highlight(lang, src)
            .into_iter()
            .map(|(end, t)| {
                let s = src[start..end as usize].to_owned();
                start = end as usize;
                (s, t)
            })
            .filter(|(s, _)| !s.trim().is_empty())
            .collect()
    }

    #[test]
    fn keyword_tables_are_sorted() {
        for l in [
            &RUST, &C, &CSHARP, &JAVA, &SWIFT, &GO, &JS, &PYTHON, &RUBY, &PHP, &SHELL, &SQL, &LUA, &HASKELL, &ELIXIR,
            &LISP, &HASHLANG, &CLIKE,
        ] {
            for list in [l.keywords, l.types, l.consts] {
                assert!(list.is_sorted(), "unsorted table: {list:?}");
            }
        }
    }

    #[test]
    fn covers_whole_input() {
        for (lang, src) in [
            ("rust", "fn main() { let x: u8 = 'a' as u8; /* hi */ }\n"),
            ("py", "def f(x):\n    '''doc'''\n    return x + 1  # ok\n"),
            ("bash", "$ sudo apt install -y foo && echo \"$HOME\" # done"),
            ("json", "{\"a\": [1, true, \"x\"]}"),
            ("yaml", "key: value\n- item: 3 # c\n"),
            ("toml", "[pkg]\nname = \"x\"\n"),
            ("html", "<a href=\"x\">t &amp; u</a><!-- c -->"),
            ("css", "a.b:hover { color: #fff; margin: 0 4px; }"),
            ("diff", "+a\n-b\n c\n"),
            ("whatever", "x = \"s\" 12"),
            ("", "plain"),
            ("rust", "héllo 'ünï"),
        ] {
            let t = highlight(lang, src);
            assert_eq!(t.last().map(|t| t.0 as usize), Some(src.len()), "{lang}");
            assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "{lang}: not increasing");
            assert!(t.iter().all(|(e, _)| src.is_char_boundary(*e as usize)), "{lang}: splits a char");
        }
    }

    #[test]
    fn rust_basics() {
        let t = toks("rust", "let s: &'a str = \"hi\"; // c");
        assert!(t.contains(&("let".into(), KEYWORD)));
        assert!(t.contains(&("'a".into(), ATTR)));
        assert!(t.contains(&("str".into(), TYPE)));
        assert!(t.contains(&("\"hi\"".into(), STRING)));
        assert!(t.contains(&("// c".into(), COMMENT)));
    }

    #[test]
    fn json_keys_and_values() {
        let t = toks("json", "{\"k\": \"v\", \"n\": 1.5}");
        assert!(t.contains(&("\"k\"".into(), PROP)));
        assert!(t.contains(&("\"v\"".into(), STRING)));
        assert!(t.contains(&("1.5".into(), NUMBER)));
    }

    #[test]
    fn shell_commands_and_flags() {
        let t = toks("sh", "brew install --cask foo | grep -i bar");
        assert!(t.contains(&("brew".into(), FUNC)));
        assert!(t.contains(&("--cask".into(), ATTR)));
        assert!(t.contains(&("grep".into(), FUNC)));
    }
}
