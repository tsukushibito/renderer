//! Tokenize one PBRT file. Quoted strings, comments and arrays can all cross
//! directive boundaries in a line-oriented reader, so retain token kinds.
use crate::Error;
use std::{borrow::Cow, collections::BTreeMap, path::Path};

#[derive(PartialEq, Eq)]
enum Kind {
    Atom,
    String,
    Open,
    Close,
}

struct Token<'a> {
    kind: Kind,
    value: Cow<'a, str>,
    line: usize,
}

pub(super) struct Command<'a> {
    pub name: &'a str,
    pub line: usize,
    pub args: Vec<Cow<'a, str>>,
    pub params: BTreeMap<Cow<'a, str>, Vec<Cow<'a, str>>>,
}

fn tokens(input: &str) -> Result<Vec<Token<'_>>, Error> {
    let mut result = Vec::new();
    let mut chars = input.char_indices().peekable();
    let mut line = 1;
    while let Some((start, c)) = chars.next() {
        let at = line;
        let (kind, value) = match c {
            '\n' => {
                line += 1;
                continue;
            }
            c if c.is_whitespace() => continue,
            '#' => {
                for (_, ch) in chars.by_ref() {
                    if ch == '\n' {
                        line += 1;
                        break;
                    }
                }
                continue;
            }
            '[' => (Kind::Open, Cow::Borrowed("[")),
            ']' => (Kind::Close, Cow::Borrowed("]")),
            '"' => {
                let mut value = String::new();
                loop {
                    let (_, ch) = chars
                        .next()
                        .ok_or_else(|| format!("line {at}: unterminated string"))?;
                    match ch {
                        '"' => break,
                        '\\' => {
                            let (_, escaped) = chars.next().ok_or("unfinished escape")?;
                            value.push(match escaped {
                                'n' => '\n',
                                'r' => '\r',
                                't' => '\t',
                                '"' => '"',
                                '\\' => '\\',
                                _ => {
                                    return Err(
                                        format!("line {line}: unknown escape {escaped}").into()
                                    );
                                }
                            });
                        }
                        '\n' => return Err(format!("line {line}: newline inside string").into()),
                        _ => value.push(ch),
                    }
                }
                (Kind::String, Cow::Owned(value))
            }
            _ => {
                while chars
                    .peek()
                    .is_some_and(|(_, ch)| !ch.is_whitespace() && !"[]\"#".contains(*ch))
                {
                    chars.next();
                }
                let end = chars.peek().map_or(input.len(), |(n, _)| *n);
                (Kind::Atom, Cow::Borrowed(&input[start..end]))
            }
        };
        result.push(Token {
            kind,
            value,
            line: at,
        });
    }
    Ok(result)
}

fn directive(token: &Token<'_>) -> bool {
    token.kind == Kind::Atom
        && token
            .value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_uppercase)
}

pub(super) fn visit(
    input: &str,
    file: &Path,
    mut emit: impl FnMut(Command<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut stream = tokens(input)
        .map_err(|e| format!("{}: {e}", file.display()))?
        .into_iter()
        .peekable();
    while let Some(head) = stream.next() {
        if !directive(&head) {
            return Err(format!("{}:{}: expected a directive", file.display(), head.line).into());
        }
        let mut command = Command {
            name: &head.value,
            line: head.line,
            args: Vec::new(),
            params: BTreeMap::new(),
        };
        let positional = match command.name {
            "Texture" => 3,
            "Camera" | "Film" | "Sampler" | "Integrator" | "PixelFilter" | "LightSource"
            | "Shape" | "Material" | "NamedMaterial" | "MakeNamedMaterial" | "ObjectBegin"
            | "ObjectInstance" | "Include" => 1,
            _ => 0,
        };
        while stream.peek().is_some_and(|t| !directive(t)) {
            let mut token = stream.next().unwrap();
            let parameter = command.args.len() >= positional
                && token.kind == Kind::String
                && token.value.split_once(' ').is_some();
            let dst = if parameter {
                let name = token.value;
                token = stream
                    .next()
                    .filter(|t| !directive(t))
                    .ok_or_else(|| format!("{}:{}: missing {name}", file.display(), head.line))?;
                match command.params.entry(name) {
                    std::collections::btree_map::Entry::Vacant(e) => e.insert(Vec::new()),
                    std::collections::btree_map::Entry::Occupied(e) => {
                        return Err(format!(
                            "{}:{}: duplicate parameter {}",
                            file.display(),
                            head.line,
                            e.key()
                        )
                        .into());
                    }
                }
            } else {
                &mut command.args
            };
            if token.kind == Kind::Open {
                loop {
                    let item = stream.next().ok_or_else(|| {
                        format!("{}:{}: unclosed array", file.display(), token.line)
                    })?;
                    if item.kind == Kind::Close {
                        break;
                    }
                    if item.kind == Kind::Open || directive(&item) {
                        return Err(
                            format!("{}:{}: malformed array", file.display(), item.line).into()
                        );
                    }
                    dst.push(item.value);
                }
            } else if token.kind == Kind::Close {
                return Err(format!("{}:{}: unexpected ]", file.display(), token.line).into());
            } else {
                dst.push(token.value);
            }
        }
        emit(command)
            .map_err(|e| format!("{}:{} {}: {e}", file.display(), head.line, head.value))?;
    }
    Ok(())
}
