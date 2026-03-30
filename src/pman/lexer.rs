use anyhow::{Result, bail};

use crate::pman::token::{Span, Token, TokenKind};

struct Lexer<'a> {
    input: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
    path: &'a str,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str, start_line: usize, start_col: usize, path: &'a str) -> Self {
        Lexer {
            input: input.as_bytes(),
            pos: 0,
            line: start_line,
            col: start_col,
            path,
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn peek(&self) -> u8 {
        self.input[self.pos]
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.input.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> u8 {
        let ch = self.input[self.pos];
        self.pos += 1;
        if ch == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        ch
    }

    fn skip_whitespace_and_comments(&mut self) {
        while !self.at_end() {
            let ch = self.peek();
            if ch == b' ' || ch == b'\t' || ch == b'\n' || ch == b'\r' {
                self.advance();
            } else if ch == b'#' {
                while !self.at_end() && self.peek() != b'\n' {
                    self.advance();
                }
            } else {
                break;
            }
        }
    }

    fn starts_with(&self, s: &[u8]) -> bool {
        self.input[self.pos..].starts_with(s)
    }

    fn lex_fenced_string(&mut self) -> Result<Token> {
        let start_line = self.line;
        let start_col = self.col;
        let start_pos = self.pos;

        // consume opening """
        self.advance();
        self.advance();
        self.advance();

        let content_start = self.pos;
        loop {
            if self.at_end() {
                bail!(
                    "{}:{}:{}: error: unterminated fenced string",
                    self.path,
                    start_line,
                    start_col
                );
            }
            if self.starts_with(b"\"\"\"") {
                let content =
                    std::str::from_utf8(&self.input[content_start..self.pos]).expect("valid utf-8");
                let content = content.to_string();
                // consume closing """
                self.advance();
                self.advance();
                self.advance();
                return Ok(Token {
                    kind: TokenKind::FencedString(content),
                    span: Span {
                        start: start_pos,
                        end: self.pos,
                        line: start_line,
                        col: start_col,
                    },
                });
            }
            self.advance();
        }
    }

    fn lex_string(&mut self) -> Result<Token> {
        let start_line = self.line;
        let start_col = self.col;
        let start_pos = self.pos;

        // consume opening quote
        self.advance();

        let mut value = String::new();
        loop {
            if self.at_end() {
                bail!(
                    "{}:{}:{}: error: unterminated string",
                    self.path,
                    start_line,
                    start_col
                );
            }
            let ch = self.advance();
            match ch {
                b'"' => {
                    return Ok(Token {
                        kind: TokenKind::String(value),
                        span: Span {
                            start: start_pos,
                            end: self.pos,
                            line: start_line,
                            col: start_col,
                        },
                    });
                }
                b'\\' => {
                    if self.at_end() {
                        bail!(
                            "{}:{}:{}: error: unterminated string",
                            self.path,
                            start_line,
                            start_col
                        );
                    }
                    let esc = self.advance();
                    match esc {
                        b'"' => value.push('"'),
                        b'\\' => value.push('\\'),
                        b'n' => value.push('\n'),
                        b't' => value.push('\t'),
                        _ => {
                            value.push('\\');
                            value.push(esc as char);
                        }
                    }
                }
                _ => value.push(ch as char),
            }
        }
    }

    fn lex_number(&mut self) -> Token {
        let start_line = self.line;
        let start_col = self.col;
        let start_pos = self.pos;

        // scan integer part
        while !self.at_end() && self.peek().is_ascii_digit() {
            self.advance();
        }

        // scan optional fractional part
        if !self.at_end() && self.peek() == b'.' {
            // check it's not `..` or `..=`
            if self.peek_at(1) != Some(b'.') {
                self.advance(); // consume '.'
                while !self.at_end() && self.peek().is_ascii_digit() {
                    self.advance();
                }
            }
        }

        let num_str =
            std::str::from_utf8(&self.input[start_pos..self.pos]).expect("valid utf-8 digits");
        let num: f64 = num_str.parse().expect("valid number");

        // check for duration suffix
        let suffix_start = self.pos;
        if !self.at_end() {
            // try "ms" first (longest match)
            if self.starts_with(b"ms") {
                let after = self.peek_at(2);
                if after.is_none() || !is_ident_char(after.unwrap()) {
                    self.advance();
                    self.advance();
                    return Token {
                        kind: TokenKind::Duration(num / 1000.0),
                        span: Span {
                            start: start_pos,
                            end: self.pos,
                            line: start_line,
                            col: start_col,
                        },
                    };
                }
            }
            // try "m" (minutes)
            if self.peek() == b'm' {
                let after = self.peek_at(1);
                if after.is_none() || !is_ident_char(after.unwrap()) {
                    self.advance();
                    return Token {
                        kind: TokenKind::Duration(num * 60.0),
                        span: Span {
                            start: start_pos,
                            end: self.pos,
                            line: start_line,
                            col: start_col,
                        },
                    };
                }
            }
            // try "s" (seconds)
            if self.peek() == b's' {
                let after = self.peek_at(1);
                if after.is_none() || !is_ident_char(after.unwrap()) {
                    self.advance();
                    return Token {
                        kind: TokenKind::Duration(num),
                        span: Span {
                            start: start_pos,
                            end: self.pos,
                            line: start_line,
                            col: start_col,
                        },
                    };
                }
            }
        }

        // no suffix consumed, restore pos (it hasn't moved past suffix_start)
        debug_assert_eq!(self.pos, suffix_start);

        Token {
            kind: TokenKind::Number(num),
            span: Span {
                start: start_pos,
                end: self.pos,
                line: start_line,
                col: start_col,
            },
        }
    }

    fn lex_ident_or_keyword(&mut self) -> Token {
        let start_line = self.line;
        let start_col = self.col;
        let start_pos = self.pos;

        while !self.at_end() && is_ident_char(self.peek()) {
            self.advance();
        }

        let word =
            std::str::from_utf8(&self.input[start_pos..self.pos]).expect("valid utf-8 ident");
        let kind = match word {
            "config" => TokenKind::Config,
            "job" => TokenKind::Job,
            "service" => TokenKind::Service,
            "event" => TokenKind::Event,
            "if" => TokenKind::If,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "env" => TokenKind::Env,
            "run" => TokenKind::Run,
            "wait" => TokenKind::Wait,
            "watch" => TokenKind::Watch,
            "after" => TokenKind::After,
            "on_fail" => TokenKind::OnFail,
            "spawn" => TokenKind::Spawn,
            "http" => TokenKind::Http,
            "connect" => TokenKind::Connect,
            "exists" => TokenKind::Exists,
            "contains" => TokenKind::Contains,
            "running" => TokenKind::Running,
            "glob" => TokenKind::Glob,
            "arg" => TokenKind::Arg,
            "import" => TokenKind::Import,
            "as" => TokenKind::As,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "none" => TokenKind::None,
            "args" => TokenKind::Args,
            _ => TokenKind::Ident(word.to_string()),
        };

        Token {
            kind,
            span: Span {
                start: start_pos,
                end: self.pos,
                line: start_line,
                col: start_col,
            },
        }
    }

    fn make_token(
        &self,
        kind: TokenKind,
        start_pos: usize,
        start_line: usize,
        start_col: usize,
    ) -> Token {
        Token {
            kind,
            span: Span {
                start: start_pos,
                end: self.pos,
                line: start_line,
                col: start_col,
            },
        }
    }

    fn next_token(&mut self) -> Result<Option<Token>> {
        self.skip_whitespace_and_comments();

        if self.at_end() {
            return Ok(None);
        }

        let start_line = self.line;
        let start_col = self.col;
        let start_pos = self.pos;
        let ch = self.peek();

        // fenced string
        if self.starts_with(b"\"\"\"") {
            return self.lex_fenced_string().map(Some);
        }

        // double-quoted string
        if ch == b'"' {
            return self.lex_string().map(Some);
        }

        // number
        if ch.is_ascii_digit() {
            return Ok(Some(self.lex_number()));
        }

        // identifier or keyword
        if ch.is_ascii_alphabetic() || ch == b'_' {
            return Ok(Some(self.lex_ident_or_keyword()));
        }

        // multi-char operators (longest first)
        if self.starts_with(b"..=") {
            self.advance();
            self.advance();
            self.advance();
            return Ok(Some(self.make_token(
                TokenKind::DotDotEq,
                start_pos,
                start_line,
                start_col,
            )));
        }
        if self.starts_with(b"..") {
            self.advance();
            self.advance();
            return Ok(Some(self.make_token(
                TokenKind::DotDot,
                start_pos,
                start_line,
                start_col,
            )));
        }
        if self.starts_with(b"==") {
            self.advance();
            self.advance();
            return Ok(Some(self.make_token(
                TokenKind::Eq,
                start_pos,
                start_line,
                start_col,
            )));
        }
        if self.starts_with(b"!=") {
            self.advance();
            self.advance();
            return Ok(Some(self.make_token(
                TokenKind::Ne,
                start_pos,
                start_line,
                start_col,
            )));
        }
        if self.starts_with(b"<=") {
            self.advance();
            self.advance();
            return Ok(Some(self.make_token(
                TokenKind::Le,
                start_pos,
                start_line,
                start_col,
            )));
        }
        if self.starts_with(b">=") {
            self.advance();
            self.advance();
            return Ok(Some(self.make_token(
                TokenKind::Ge,
                start_pos,
                start_line,
                start_col,
            )));
        }
        if self.starts_with(b"&&") {
            self.advance();
            self.advance();
            return Ok(Some(self.make_token(
                TokenKind::And,
                start_pos,
                start_line,
                start_col,
            )));
        }
        if self.starts_with(b"||") {
            self.advance();
            self.advance();
            return Ok(Some(self.make_token(
                TokenKind::Or,
                start_pos,
                start_line,
                start_col,
            )));
        }

        if self.starts_with(b"::") {
            self.advance();
            self.advance();
            return Ok(Some(self.make_token(
                TokenKind::ColonColon,
                start_pos,
                start_line,
                start_col,
            )));
        }

        // single-char tokens
        self.advance();
        let kind = match ch {
            b'=' => TokenKind::Assign,
            b'<' => TokenKind::Lt,
            b'>' => TokenKind::Gt,
            b'!' => TokenKind::Not,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b',' => TokenKind::Comma,
            b'.' => TokenKind::Dot,
            b'@' => TokenKind::At,
            _ => bail!(
                "{}:{}:{}: error: unexpected character '{}'",
                self.path,
                start_line,
                start_col,
                ch as char
            ),
        };

        Ok(Some(
            self.make_token(kind, start_pos, start_line, start_col),
        ))
    }
}

fn is_ident_char(ch: u8) -> bool {
    ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-'
}

/// Lex `.pman` source into tokens.
///
/// `start_line` and `start_col` are 1-based offsets applied to the first
/// character — pass `(1, 1)` when lexing from the beginning of a file.
pub fn lex(input: &str, start_line: usize, start_col: usize, path: &str) -> Result<Vec<Token>> {
    let mut lexer = Lexer::new(input, start_line, start_col, path);
    let mut tokens = Vec::new();
    while let Some(tok) = lexer.next_token()? {
        tokens.push(tok);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(input: &str) -> Vec<TokenKind> {
        lex(input, 1, 1, "test.pman")
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn lex_keywords() {
        assert_eq!(
            kinds("job event service config"),
            vec![
                TokenKind::Job,
                TokenKind::Event,
                TokenKind::Service,
                TokenKind::Config,
            ]
        );
    }

    #[test]
    fn lex_string() {
        assert_eq!(kinds(r#""hello""#), vec![TokenKind::String("hello".into())]);
    }

    #[test]
    fn lex_string_escapes() {
        assert_eq!(
            kinds(r#""a\"b\\c\n\t""#),
            vec![TokenKind::String("a\"b\\c\n\t".into())]
        );
    }

    #[test]
    fn lex_number() {
        assert_eq!(
            kinds("42 3.25"),
            vec![TokenKind::Number(42.0), TokenKind::Number(3.25)]
        );
    }

    #[test]
    fn lex_duration() {
        assert_eq!(
            kinds("5s 500ms 2m 1.5s"),
            vec![
                TokenKind::Duration(5.0),
                TokenKind::Duration(0.5),
                TokenKind::Duration(120.0),
                TokenKind::Duration(1.5),
            ]
        );
    }

    #[test]
    fn lex_operators() {
        assert_eq!(
            kinds("== != < > <= >="),
            vec![
                TokenKind::Eq,
                TokenKind::Ne,
                TokenKind::Lt,
                TokenKind::Gt,
                TokenKind::Le,
                TokenKind::Ge,
            ]
        );
    }

    #[test]
    fn lex_logical() {
        assert_eq!(
            kinds("&& || !"),
            vec![TokenKind::And, TokenKind::Or, TokenKind::Not]
        );
    }

    #[test]
    fn lex_punctuation() {
        assert_eq!(
            kinds("= { } [ ] ( ) , .. ..="),
            vec![
                TokenKind::Assign,
                TokenKind::LBrace,
                TokenKind::RBrace,
                TokenKind::LBracket,
                TokenKind::RBracket,
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::Comma,
                TokenKind::DotDot,
                TokenKind::DotDotEq,
            ]
        );
    }

    #[test]
    fn lex_at_and_dot() {
        assert_eq!(
            kinds("@migrate.KEY"),
            vec![
                TokenKind::At,
                TokenKind::Ident("migrate".into()),
                TokenKind::Dot,
                TokenKind::Ident("KEY".into()),
            ]
        );
    }

    #[test]
    fn lex_fenced_string() {
        let input = "run \"\"\"\n  echo hello\n\"\"\"";
        assert_eq!(
            kinds(input),
            vec![
                TokenKind::Run,
                TokenKind::FencedString("\n  echo hello\n".into()),
            ]
        );
    }

    #[test]
    fn lex_comment_skipped() {
        assert_eq!(
            kinds("job # comment\nweb"),
            vec![TokenKind::Job, TokenKind::Ident("web".into())]
        );
    }

    #[test]
    fn lex_not_vs_ne() {
        assert_eq!(kinds("!connect"), vec![TokenKind::Not, TokenKind::Connect]);
        assert_eq!(kinds("!="), vec![TokenKind::Ne]);
    }

    #[test]
    fn lex_args_keyword() {
        assert_eq!(
            kinds("args.port"),
            vec![
                TokenKind::Args,
                TokenKind::Dot,
                TokenKind::Ident("port".into()),
            ]
        );
    }

    #[test]
    fn lex_identifier_with_hyphens() {
        assert_eq!(
            kinds("web-server"),
            vec![TokenKind::Ident("web-server".into())]
        );
    }

    #[test]
    fn lex_import_as_keywords() {
        assert_eq!(
            kinds(r#"import "foo.pman" as bar"#),
            vec![
                TokenKind::Import,
                TokenKind::String("foo.pman".into()),
                TokenKind::As,
                TokenKind::Ident("bar".into()),
            ]
        );
    }

    #[test]
    fn lex_colon_colon() {
        assert_eq!(
            kinds("@ns::migrate"),
            vec![
                TokenKind::At,
                TokenKind::Ident("ns".into()),
                TokenKind::ColonColon,
                TokenKind::Ident("migrate".into()),
            ]
        );
    }

    #[test]
    fn lex_span_tracks_line_and_col() {
        let tokens = lex("job\n  web", 1, 1, "test.pman").unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].span.line, 1);
        assert_eq!(tokens[0].span.col, 1);
        assert_eq!(tokens[1].span.line, 2);
        assert_eq!(tokens[1].span.col, 3);
    }

    #[test]
    fn lex_error_includes_line_col() {
        let err = lex("job\n\"hello", 1, 1, "test.pman").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("test.pman:2:1: error:"),
            "expected full location in error: {msg}"
        );
    }

    #[test]
    fn lex_unterminated_string_errors() {
        assert!(lex(r#""hello"#, 1, 1, "test.pman").is_err());
    }

    #[test]
    fn lex_unterminated_fenced_errors() {
        assert!(lex("\"\"\"\nhello", 1, 1, "test.pman").is_err());
    }

    #[test]
    fn lex_start_offset() {
        let tokens = lex("foo", 5, 10, "test.pman").unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].span.line, 5);
        assert_eq!(tokens[0].span.col, 10);
    }
}
