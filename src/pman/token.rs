/// Source location for error reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize, // byte offset
    pub end: usize,   // byte offset
    pub line: usize,  // 1-based line number
    pub col: usize,   // 1-based column (byte offset within line)
}

impl Span {
    /// Format an error message with file location.
    pub fn fmt_error(&self, path: &str, msg: &str) -> String {
        format!("{path}:{}:{}: {msg}", self.line, self.col)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    // Keywords
    Config,   // config
    Job,      // job
    Event,    // event
    If,       // if
    For,      // for
    In,       // in
    Env,      // env
    Run,      // run
    Wait,     // wait
    Watch,    // watch
    After,    // after
    Once,     // once
    OnFail,   // on_fail
    Spawn,    // spawn
    Http,     // http
    Connect,  // connect
    Exists,   // exists
    Contains, // contains
    Running,  // running
    Glob,     // glob
    Arg,      // arg
    True,     // true
    False,    // false
    None,     // none

    // Literals
    String(String),       // "..." (contents, escapes resolved)
    Number(f64),          // 42, 3.14
    Duration(f64),        // 5.0 (seconds) — suffix parsed into seconds
    FencedString(String), // ``` ... ``` (raw contents)

    // Identifiers and references
    Ident(String), // bare identifier (e.g., job name, var name)
    At,            // @
    Dot,           // .
    Args,          // args (keyword for args namespace)

    // Operators
    Eq,  // ==
    Ne,  // !=
    Lt,  // <
    Gt,  // >
    Le,  // <=
    Ge,  // >=
    And, // &&
    Or,  // ||
    Not, // !

    // Punctuation
    Assign,   // =
    LBrace,   // {
    RBrace,   // }
    LBracket, // [
    RBracket, // ]
    LParen,   // (
    RParen,   // )
    Comma,    // ,
    DotDot,   // ..
    DotDotEq, // ..=
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
