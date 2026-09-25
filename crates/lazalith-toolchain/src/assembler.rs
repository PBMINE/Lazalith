use crate::{
    CodeMapping, DebugSource, ObjectBuilder, ObjectFile, Relocation, RelocationKind, Section,
    Symbol, SymbolBinding, ToolchainError,
};
use alloc::{borrow::ToOwned, boxed::Box, collections::BTreeMap, string::String, vec::Vec};
use core::{error::Error, fmt};
use lazalith_diagnostics::{Diagnostic, DiagnosticCode, Label, Severity};
use lazalith_isa::{
    Condition as TypeCondition, ControlRegister, DataSize, Instruction, Opcode, Operand, encode,
};
use lazalith_os::LzxArchitecture;
use lazalith_types::{ByteOffset, RegisterIndex, SourceError, SourceId, SourceManager, SourceSpan};

#[derive(Debug)]
pub enum AssemblyError {
    Source(SourceError),
    InvalidSpan(lazalith_types::InvalidSpan),
    Diagnostic {
        diagnostic: Diagnostic,
        sources: SourceManager,
    },
}

impl fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(source) => write!(f, "source registration failed: {source}"),
            Self::InvalidSpan(source) => write!(f, "source span construction failed: {source}"),
            Self::Diagnostic { diagnostic, .. } => write!(f, "{diagnostic}"),
        }
    }
}

impl Error for AssemblyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Source(source) => Some(source),
            Self::InvalidSpan(source) => Some(source),
            Self::Diagnostic { diagnostic, .. } => Some(diagnostic),
        }
    }
}

impl AssemblyError {
    pub fn diagnostic(&self) -> Option<&Diagnostic> {
        match self {
            Self::Diagnostic { diagnostic, .. } => Some(diagnostic),
            _ => None,
        }
    }

    pub fn sources(&self) -> Option<&SourceManager> {
        match self {
            Self::Diagnostic { sources, .. } => Some(sources),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TokenKind {
    Word(String),
    Integer(i128),
    Text(Vec<u8>),
    Comma,
    Colon,
    LBracket,
    RBracket,
    Plus,
    Minus,
}

#[derive(Clone)]
struct Token {
    kind: TokenKind,
    span: SourceSpan,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SectionKey {
    Text,
    ReadOnlyData,
    Data,
    Bss,
}

impl SectionKey {
    const ORDER: [Self; 4] = [Self::Text, Self::ReadOnlyData, Self::Data, Self::Bss];
}

#[derive(Clone)]
struct SectionAccumulator {
    key: SectionKey,
    alignment: u64,
    bytes: Vec<u8>,
    bss_size: u64,
    used: bool,
}

impl SectionAccumulator {
    fn new(key: SectionKey) -> Self {
        Self {
            key,
            alignment: match key {
                SectionKey::Text => 4,
                SectionKey::ReadOnlyData | SectionKey::Data => 4,
                SectionKey::Bss => 4,
            },
            bytes: Vec::new(),
            bss_size: 0,
            used: false,
        }
    }

    fn offset(&self) -> u64 {
        if self.key == SectionKey::Bss {
            self.bss_size
        } else {
            self.bytes.len() as u64
        }
    }

    fn append(&mut self, bytes: &[u8]) -> Result<(), ToolchainError> {
        self.bytes
            .try_reserve(bytes.len())
            .map_err(ToolchainError::Allocation)?;
        self.bytes.extend_from_slice(bytes);
        self.used = true;
        Ok(())
    }

    fn zero(&mut self, count: u64) -> Result<(), ToolchainError> {
        let count = usize::try_from(count).map_err(|_| {
            ToolchainError::Assembly(Box::new(AssemblyError::Diagnostic {
                diagnostic: Diagnostic::new(
                    Severity::Error,
                    DiagnosticCode::new("E200").expect("assembler diagnostic code is valid"),
                    "zero count does not fit the host index domain",
                ),
                sources: SourceManager::new(),
            }))
        })?;
        self.bytes
            .try_reserve(count)
            .map_err(ToolchainError::Allocation)?;
        self.bytes.resize(self.bytes.len() + count, 0);
        self.used = true;
        Ok(())
    }

    fn align(&mut self, alignment: u64) -> Result<u64, ToolchainError> {
        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(ToolchainError::Assembly(Box::new(
                AssemblyError::Diagnostic {
                    diagnostic: Diagnostic::new(
                        Severity::Error,
                        DiagnosticCode::new("E201").expect("assembler diagnostic code is valid"),
                        "alignment must be a nonzero power of two",
                    ),
                    sources: SourceManager::new(),
                },
            )));
        }
        self.alignment = self.alignment.max(alignment);
        let current = if self.key == SectionKey::Bss {
            self.bss_size
        } else {
            self.bytes.len() as u64
        };
        let mask = alignment - 1;
        let next = current
            .checked_add(mask)
            .ok_or(ToolchainError::Assembly(Box::new(
                AssemblyError::Diagnostic {
                    diagnostic: Diagnostic::new(
                        Severity::Error,
                        DiagnosticCode::new("E202").expect("assembler diagnostic code is valid"),
                        "alignment calculation overflows",
                    ),
                    sources: SourceManager::new(),
                },
            )))?
            & !mask;
        let padding = next - current;
        if self.key == SectionKey::Bss {
            self.bss_size = next;
        } else {
            self.bytes.resize(self.bytes.len() + padding as usize, 0);
        }
        self.used = true;
        Ok(next)
    }
}

#[derive(Clone)]
enum DefKind {
    Undefined,
    Absolute,
    Section(SectionKey),
}

#[derive(Clone)]
struct SymbolDef {
    name: String,
    kind: DefKind,
    global: bool,
    value: u64,
    size: u64,
    span: SourceSpan,
}

#[derive(Clone)]
struct PendingRelocation {
    symbol: String,
    section: SectionKey,
    kind: RelocationKind,
    offset: u64,
    addend: i64,
    span: SourceSpan,
}

#[derive(Clone)]
struct PendingMapping {
    offset: u64,
    start: usize,
    length: usize,
}

#[derive(Clone)]
enum Expr {
    Integer(i128),
    Symbol { name: String, addend: i64 },
}

#[derive(Clone)]
enum ParsedOperand {
    Concrete(Operand),
    Immediate(Expr, SourceSpan),
    Memory {
        base: RegisterIndex,
        displacement: Expr,
        span: SourceSpan,
    },
}

#[derive(Clone)]
struct Assembler<'a> {
    source: &'a str,
    manager: SourceManager,
    source_id: SourceId,
    source_name: String,
    source_length: u32,
    architecture: Option<LzxArchitecture>,
    current: SectionKey,
    entry: Option<String>,
    sections: BTreeMap<SectionKey, SectionAccumulator>,
    symbols: Vec<SymbolDef>,
    globals: BTreeMap<String, bool>,
    dword_span: Option<SourceSpan>,
    relocations: Vec<PendingRelocation>,
    mappings: Vec<PendingMapping>,
}

pub fn assemble(source: &str) -> Result<ObjectFile, ToolchainError> {
    assemble_named("input.lzs", source)
}

pub fn assemble_named(name: &str, source: &str) -> Result<ObjectFile, ToolchainError> {
    let mut manager = SourceManager::new();
    let source_id = manager
        .add_file(name, source)
        .map_err(AssemblyError::Source)?;
    let source_length = u32::try_from(source.len()).map_err(|_| {
        ToolchainError::Assembly(Box::new(AssemblyError::Source(
            lazalith_types::SourceError::TextTooLarge {
                length: source.len(),
                max: u32::MAX,
            },
        )))
    })?;
    let mut assembler = Assembler {
        source,
        manager,
        source_id,
        source_name: String::from(name),
        source_length,
        architecture: None,
        current: SectionKey::Text,
        entry: None,
        sections: SectionKey::ORDER
            .into_iter()
            .map(|key| (key, SectionAccumulator::new(key)))
            .collect(),
        symbols: Vec::new(),
        globals: BTreeMap::new(),
        dword_span: None,
        relocations: Vec::new(),
        mappings: Vec::new(),
    };
    assembler.parse()
}

impl<'a> Assembler<'a> {
    fn parse(&mut self) -> Result<ObjectFile, ToolchainError> {
        let mut line_start = 0;
        while line_start <= self.source.len() {
            let relative_end = self.source[line_start..]
                .find('\n')
                .unwrap_or(self.source.len() - line_start);
            let line_end = line_start + relative_end;
            let tokens = self.lex_line(line_start, line_end)?;
            if !tokens.is_empty() {
                self.parse_line(line_start, line_end, tokens)?;
            }
            if line_end == self.source.len() {
                break;
            }
            line_start = line_end + 1;
        }
        self.finish()
    }

    fn lex_line(&self, start: usize, end: usize) -> Result<Vec<Token>, ToolchainError> {
        let line = &self.source[start..end];
        let bytes = line.as_bytes();
        let mut tokens = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index].is_ascii_whitespace() {
                index += 1;
                continue;
            }
            if bytes[index] == b'#' || bytes[index] == b';' {
                break;
            }
            let token_start = index;
            let kind = match bytes[index] {
                b',' => {
                    index += 1;
                    TokenKind::Comma
                }
                b':' => {
                    index += 1;
                    TokenKind::Colon
                }
                b'[' => {
                    index += 1;
                    TokenKind::LBracket
                }
                b']' => {
                    index += 1;
                    TokenKind::RBracket
                }
                b'+' => {
                    index += 1;
                    TokenKind::Plus
                }
                b'-' => {
                    index += 1;
                    TokenKind::Minus
                }
                b'"' => {
                    index += 1;
                    let mut value = Vec::new();
                    let mut closed = false;
                    while index < bytes.len() {
                        let byte = bytes[index];
                        index += 1;
                        if byte == b'"' {
                            closed = true;
                            break;
                        }
                        if byte == b'\\' {
                            if index >= bytes.len() {
                                break;
                            }
                            let escaped = bytes[index];
                            index += 1;
                            value.push(match escaped {
                                b'n' => b'\n',
                                b'r' => b'\r',
                                b't' => b'\t',
                                b'0' => 0,
                                b'\\' => b'\\',
                                b'"' => b'"',
                                b'x' => {
                                    if index + 1 >= bytes.len()
                                        || !bytes[index].is_ascii_hexdigit()
                                        || !bytes[index + 1].is_ascii_hexdigit()
                                    {
                                        return Err(self.fail(
                                            start + token_start,
                                            start + index,
                                            "E210",
                                            "invalid hexadecimal string escape",
                                        ));
                                    }
                                    let high = hex_value(bytes[index]);
                                    let low = hex_value(bytes[index + 1]);
                                    index += 2;
                                    (high << 4) | low
                                }
                                _ => {
                                    return Err(self.fail(
                                        start + token_start,
                                        start + index,
                                        "E211",
                                        "invalid string escape",
                                    ));
                                }
                            });
                        } else {
                            value.push(byte);
                        }
                    }
                    if !closed {
                        return Err(self.fail(
                            start + token_start,
                            start + index,
                            "E212",
                            "unterminated string literal",
                        ));
                    }
                    TokenKind::Text(value)
                }
                byte if byte.is_ascii_digit() => {
                    let number_start = index;
                    if byte == b'0'
                        && index + 1 < bytes.len()
                        && (bytes[index + 1] == b'x' || bytes[index + 1] == b'X')
                    {
                        index += 2;
                        let digits_start = index;
                        while index < bytes.len() && bytes[index].is_ascii_hexdigit() {
                            index += 1;
                        }
                        if digits_start == index {
                            return Err(self.fail(
                                start + token_start,
                                start + index,
                                "E213",
                                "hexadecimal literal has no digits",
                            ));
                        }
                        let raw =
                            core::str::from_utf8(&bytes[number_start + 2..index]).unwrap_or("");
                        let value = i128::from_str_radix(raw, 16).map_err(|_| {
                            self.fail(
                                start + token_start,
                                start + index,
                                "E214",
                                "hexadecimal literal is out of range",
                            )
                        })?;
                        TokenKind::Integer(value)
                    } else {
                        while index < bytes.len() && bytes[index].is_ascii_digit() {
                            index += 1;
                        }
                        let raw = core::str::from_utf8(&bytes[number_start..index]).unwrap_or("");
                        let value = raw.parse::<i128>().map_err(|_| {
                            self.fail(
                                start + token_start,
                                start + index,
                                "E215",
                                "integer literal is out of range",
                            )
                        })?;
                        TokenKind::Integer(value)
                    }
                }
                byte if is_identifier_start(byte) => {
                    index += 1;
                    while index < bytes.len() && is_identifier_continue(bytes[index]) {
                        index += 1;
                    }
                    let raw = core::str::from_utf8(&bytes[token_start..index]).unwrap_or("");
                    TokenKind::Word(String::from(raw))
                }
                _ => {
                    let length = line[index..]
                        .chars()
                        .next()
                        .map(char::len_utf8)
                        .unwrap_or(1);
                    return Err(self.fail(
                        start + index,
                        start + index + length,
                        "E216",
                        "invalid character in assembly source",
                    ));
                }
            };
            let span = self.span(start + token_start, start + index)?;
            tokens.push(Token { kind, span });
        }
        Ok(tokens)
    }

    fn parse_line(
        &mut self,
        start: usize,
        end: usize,
        tokens: Vec<Token>,
    ) -> Result<(), ToolchainError> {
        if tokens.len() >= 2 && tokens[1].kind == TokenKind::Colon {
            self.define_label(start, end, &tokens)?;
            return Ok(());
        }
        let first = match tokens.first() {
            Some(token) => token,
            None => return Ok(()),
        };
        if let TokenKind::Word(word) = &first.kind
            && word.starts_with('.')
        {
            return self.parse_directive(start, end, tokens);
        }
        self.parse_instruction(start, end, tokens)
    }

    fn define_label(
        &mut self,
        start: usize,
        end: usize,
        tokens: &[Token],
    ) -> Result<(), ToolchainError> {
        if tokens.len() != 2 {
            return Err(self.fail(start, end, "E220", "label must be a standalone definition"));
        }
        let name = match &tokens[0].kind {
            TokenKind::Word(name) if valid_identifier(name) => name.clone(),
            _ => {
                return Err(self.fail(
                    tokens[0].span.start().as_u32() as usize,
                    tokens[0].span.end().as_u32() as usize,
                    "E221",
                    "label name is invalid",
                ));
            }
        };
        if self.symbols.iter().any(|symbol| symbol.name == name) {
            return Err(self.fail(start, end, "E222", "symbol is defined more than once"));
        }
        let value = self
            .sections
            .get(&self.current)
            .ok_or_else(|| self.fail(start, end, "E223", "current section is unavailable"))?
            .offset();
        let global = self.globals.get(&name).copied().unwrap_or(false);
        self.symbols.push(SymbolDef {
            name,
            kind: DefKind::Section(self.current),
            global,
            value,
            size: 0,
            span: tokens[0].span.clone(),
        });
        self.sections
            .get_mut(&self.current)
            .expect("current section exists")
            .used = true;
        Ok(())
    }

    fn parse_directive(
        &mut self,
        start: usize,
        end: usize,
        tokens: Vec<Token>,
    ) -> Result<(), ToolchainError> {
        let directive = match &tokens[0].kind {
            TokenKind::Word(word) => word.clone(),
            _ => return Err(self.fail(start, end, "E230", "directive must be a word")),
        };
        if directive.eq_ignore_ascii_case(".arch") {
            return self.parse_arch(start, end, &tokens);
        }
        if directive.eq_ignore_ascii_case(".entry") {
            return self.parse_entry(start, end, &tokens);
        }
        if directive.eq_ignore_ascii_case(".section") {
            return self.parse_section(start, end, &tokens);
        }
        if directive.eq_ignore_ascii_case(".text")
            || directive.eq_ignore_ascii_case(".rodata")
            || directive.eq_ignore_ascii_case(".data")
            || directive.eq_ignore_ascii_case(".bss")
        {
            if tokens.len() != 1 {
                return Err(self.fail(start, end, "E311", "section directive accepts no operands"));
            }
            self.current = if directive.eq_ignore_ascii_case(".text") {
                SectionKey::Text
            } else if directive.eq_ignore_ascii_case(".rodata") {
                SectionKey::ReadOnlyData
            } else if directive.eq_ignore_ascii_case(".data") {
                SectionKey::Data
            } else {
                SectionKey::Bss
            };
            return Ok(());
        }
        if directive.eq_ignore_ascii_case(".global") {
            return self.parse_global(start, end, &tokens);
        }
        if directive.eq_ignore_ascii_case(".extern") {
            return self.parse_extern(start, end, &tokens);
        }
        if directive.eq_ignore_ascii_case(".equ") {
            return self.parse_equ(start, end, &tokens);
        }
        if directive.eq_ignore_ascii_case(".align") {
            return self.parse_align(start, end, &tokens);
        }
        if directive.eq_ignore_ascii_case(".zero") {
            return self.parse_zero(start, end, &tokens);
        }
        if directive.eq_ignore_ascii_case(".ascii") || directive.eq_ignore_ascii_case(".asciz") {
            return self.parse_ascii(
                start,
                end,
                &tokens,
                directive.eq_ignore_ascii_case(".asciz"),
            );
        }
        if directive.eq_ignore_ascii_case(".byte")
            || directive.eq_ignore_ascii_case(".half")
            || directive.eq_ignore_ascii_case(".word")
            || directive.eq_ignore_ascii_case(".dword")
            || directive.eq_ignore_ascii_case(".pcrelword")
        {
            return self.parse_data(start, end, &tokens, &directive);
        }
        Err(self.fail(start, end, "E231", "unknown assembler directive"))
    }

    fn parse_arch(
        &mut self,
        start: usize,
        end: usize,
        tokens: &[Token],
    ) -> Result<(), ToolchainError> {
        if self.architecture.is_some() {
            return Err(self.fail(start, end, "E232", ".arch may be specified only once"));
        }
        if tokens.len() != 2 {
            return Err(self.fail(start, end, "E233", ".arch requires one value"));
        }
        let value = self.word(tokens, 1, "E234")?;
        self.architecture = Some(if value.eq_ignore_ascii_case("lz32") {
            LzxArchitecture::Lz32
        } else if value.eq_ignore_ascii_case("lz64") {
            LzxArchitecture::Lz64
        } else {
            return Err(self.fail(
                tokens[1].span.start().as_u32() as usize,
                tokens[1].span.end().as_u32() as usize,
                "E235",
                ".arch requires lz32 or lz64",
            ));
        });
        Ok(())
    }

    fn parse_entry(
        &mut self,
        start: usize,
        end: usize,
        tokens: &[Token],
    ) -> Result<(), ToolchainError> {
        if self.entry.is_some() {
            return Err(self.fail(start, end, "E236", ".entry may be specified only once"));
        }
        if tokens.len() != 2 {
            return Err(self.fail(start, end, "E237", ".entry requires one label"));
        }
        self.entry = Some(self.word(tokens, 1, "E238")?.to_owned());
        Ok(())
    }

    fn parse_section(
        &mut self,
        start: usize,
        end: usize,
        tokens: &[Token],
    ) -> Result<(), ToolchainError> {
        if tokens.len() != 2 {
            return Err(self.fail(start, end, "E239", ".section requires one section name"));
        }
        let value = self.word(tokens, 1, "E240")?;
        self.current = if value.eq_ignore_ascii_case(".text") || value.eq_ignore_ascii_case("text")
        {
            SectionKey::Text
        } else if value.eq_ignore_ascii_case(".rodata") || value.eq_ignore_ascii_case("rodata") {
            SectionKey::ReadOnlyData
        } else if value.eq_ignore_ascii_case(".data") || value.eq_ignore_ascii_case("data") {
            SectionKey::Data
        } else if value.eq_ignore_ascii_case(".bss") || value.eq_ignore_ascii_case("bss") {
            SectionKey::Bss
        } else {
            return Err(self.fail(
                tokens[1].span.start().as_u32() as usize,
                tokens[1].span.end().as_u32() as usize,
                "E241",
                "unknown section",
            ));
        };
        self.sections
            .entry(self.current)
            .or_insert_with(|| SectionAccumulator::new(self.current));
        Ok(())
    }

    fn parse_global(
        &mut self,
        start: usize,
        end: usize,
        tokens: &[Token],
    ) -> Result<(), ToolchainError> {
        if tokens.len() != 2 {
            return Err(self.fail(start, end, "E242", ".global requires one symbol"));
        }
        let name = self.word(tokens, 1, "E243")?.to_owned();
        if let Some(symbol) = self.symbols.iter_mut().find(|symbol| symbol.name == name) {
            symbol.global = true;
        } else {
            self.globals.insert(name, true);
        }
        let _ = (start, end);
        Ok(())
    }

    fn parse_extern(
        &mut self,
        start: usize,
        end: usize,
        tokens: &[Token],
    ) -> Result<(), ToolchainError> {
        if tokens.len() != 2 {
            return Err(self.fail(start, end, "E244", ".extern requires one symbol"));
        }
        let name = self.word(tokens, 1, "E245")?.to_owned();
        if self.symbols.iter().any(|symbol| symbol.name == name) {
            return Err(self.fail(start, end, "E246", "extern symbol is already defined"));
        }
        self.symbols.push(SymbolDef {
            name: name.clone(),
            kind: DefKind::Undefined,
            global: true,
            value: 0,
            size: 0,
            span: tokens[1].span.clone(),
        });
        self.globals.insert(name, true);
        Ok(())
    }

    fn parse_equ(
        &mut self,
        start: usize,
        end: usize,
        tokens: &[Token],
    ) -> Result<(), ToolchainError> {
        if tokens.len() != 4 || tokens[2].kind != TokenKind::Comma {
            return Err(self.fail(start, end, "E247", ".equ requires name, expression"));
        }
        let name = self.word(tokens, 1, "E248")?.to_owned();
        if self.symbols.iter().any(|symbol| symbol.name == name) {
            return Err(self.fail(start, end, "E249", "constant is already defined"));
        }
        let mut index = 3;
        let expression = self.parse_expression(tokens, &mut index)?;
        let value = match expression {
            Expr::Integer(value) if value >= 0 => u64::try_from(value).map_err(|_| {
                self.fail(
                    tokens[3].span.start().as_u32() as usize,
                    tokens[3].span.end().as_u32() as usize,
                    "E250",
                    "constant is out of range",
                )
            })?,
            _ => {
                return Err(self.fail(
                    tokens[3].span.start().as_u32() as usize,
                    tokens[3].span.end().as_u32() as usize,
                    "E251",
                    ".equ requires an unsigned integer",
                ));
            }
        };
        let global = self.globals.get(&name).copied().unwrap_or(false);
        self.symbols.push(SymbolDef {
            name,
            kind: DefKind::Absolute,
            global,
            value,
            size: 0,
            span: tokens[0].span.clone(),
        });
        Ok(())
    }

    fn parse_align(
        &mut self,
        start: usize,
        end: usize,
        tokens: &[Token],
    ) -> Result<(), ToolchainError> {
        if tokens.len() != 2 {
            return Err(self.fail(start, end, "E252", ".align requires one value"));
        }
        let value = self.integer(tokens, 1, "E253")?;
        let alignment = u64::try_from(value)
            .map_err(|_| self.fail(start, end, "E254", "alignment is out of range"))?;
        if self.current == SectionKey::Text {
            return Err(self.fail(start, end, "E255", ".align is not valid in text"));
        }
        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(self.fail(
                start,
                end,
                "E201",
                "alignment must be a nonzero power of two",
            ));
        }
        if alignment > 1 << 20 {
            return Err(self.fail(start, end, "E307", "alignment exceeds the assembler limit"));
        }
        self.sections
            .entry(self.current)
            .or_insert_with(|| SectionAccumulator::new(self.current))
            .align(alignment)
            .map_err(|error| self.map_error(error, start, end))?;
        Ok(())
    }

    fn parse_zero(
        &mut self,
        start: usize,
        end: usize,
        tokens: &[Token],
    ) -> Result<(), ToolchainError> {
        if self.current == SectionKey::Text {
            return Err(self.fail(start, end, "E312", ".zero is not valid in text"));
        }
        if tokens.len() != 2 {
            return Err(self.fail(start, end, "E256", ".zero requires one value"));
        }
        let value = self.integer(tokens, 1, "E257")?;
        if value < 0 {
            return Err(self.fail(
                tokens[1].span.start().as_u32() as usize,
                tokens[1].span.end().as_u32() as usize,
                "E258",
                ".zero count cannot be negative",
            ));
        }
        let count = u64::try_from(value)
            .map_err(|_| self.fail(start, end, "E259", ".zero count is out of range"))?;
        if self.current == SectionKey::Bss {
            let key = self.current;
            let current = self
                .sections
                .get(&key)
                .map_or(0, SectionAccumulator::offset);
            let next = current
                .checked_add(count)
                .ok_or_else(|| self.fail(start, end, "E260", ".zero count overflows"))?;
            let section = self
                .sections
                .entry(key)
                .or_insert_with(|| SectionAccumulator::new(key));
            section.bss_size = next;
            section.used = true;
        } else {
            self.sections
                .entry(self.current)
                .or_insert_with(|| SectionAccumulator::new(self.current))
                .zero(count)
                .map_err(|error| self.map_error(error, start, end))?;
        }
        Ok(())
    }

    fn parse_ascii(
        &mut self,
        start: usize,
        end: usize,
        tokens: &[Token],
        terminated: bool,
    ) -> Result<(), ToolchainError> {
        if tokens.len() != 2 {
            return Err(self.fail(start, end, "E261", ".ascii requires one string"));
        }
        if self.current == SectionKey::Text || self.current == SectionKey::Bss {
            return Err(self.fail(
                start,
                end,
                "E262",
                "string data is not valid in this section",
            ));
        }
        let TokenKind::Text(mut value) = tokens[1].kind.clone() else {
            return Err(self.fail(start, end, "E263", ".ascii requires a string"));
        };
        if terminated {
            value.push(0);
        }
        self.sections
            .entry(self.current)
            .or_insert_with(|| SectionAccumulator::new(self.current))
            .append(&value)
            .map_err(|error| self.map_error(error, start, end))
    }

    fn parse_data(
        &mut self,
        start: usize,
        end: usize,
        tokens: &[Token],
        directive: &str,
    ) -> Result<(), ToolchainError> {
        if self.current == SectionKey::Text || self.current == SectionKey::Bss {
            return Err(self.fail(
                start,
                end,
                "E264",
                "data directives are not valid in this section",
            ));
        }
        let (width, kind) = match directive.to_ascii_lowercase().as_str() {
            ".byte" => (1usize, None),
            ".half" => (2, None),
            ".word" => (4, Some(RelocationKind::AbsoluteWord32)),
            ".dword" => (8, Some(RelocationKind::AbsoluteWord64)),
            ".pcrelword" => (4, Some(RelocationKind::PcRelativeWord32)),
            _ => return Err(self.fail(start, end, "E265", "unknown data directive")),
        };
        if directive.eq_ignore_ascii_case(".dword") {
            if let Some(architecture) = self.architecture {
                if architecture == LzxArchitecture::Lz32 {
                    return Err(self.fail(start, end, "E266", ".dword requires LZ64"));
                }
            } else {
                self.dword_span = Some(tokens[0].span.clone());
            }
        }
        if tokens.len() == 1 {
            return Err(self.fail(
                start,
                end,
                "E310",
                "data directive requires at least one value",
            ));
        }
        let mut index = 1;
        let section_key = self.current;
        self.sections
            .entry(section_key)
            .or_insert_with(|| SectionAccumulator::new(section_key));
        while index < tokens.len() {
            if index > 1 {
                if tokens[index].kind != TokenKind::Comma {
                    return Err(self.fail(
                        start,
                        end,
                        "E267",
                        "data values must be comma-separated",
                    ));
                }
                index += 1;
            }
            let offset = self
                .sections
                .get(&section_key)
                .expect("data section exists")
                .offset();
            if kind.is_some() && !offset.is_multiple_of(width as u64) {
                return Err(self.fail(
                    start,
                    end,
                    "E313",
                    "data relocation or value is not naturally aligned",
                ));
            }
            let expression = self.parse_expression(tokens, &mut index)?;
            let mut encoded = [0u8; 8];
            if let Expr::Integer(value) = expression {
                if value < 0 || value >= 1i128 << (width * 8) {
                    return Err(self.fail(start, end, "E268", "data value is out of range"));
                }
                encoded[..width].copy_from_slice(&(value as u128).to_le_bytes()[..width]);
            } else if let (Expr::Symbol { name, addend }, Some(kind)) = (expression, kind) {
                self.relocations.push(PendingRelocation {
                    symbol: name,
                    section: self.current,
                    kind,
                    offset,
                    addend,
                    span: tokens
                        .get(index.saturating_sub(1))
                        .map_or_else(|| tokens[0].span.clone(), |token| token.span.clone()),
                });
            } else {
                return Err(self.fail(
                    start,
                    end,
                    "E269",
                    "symbol requires a relocation-capable data directive",
                ));
            }
            self.sections
                .get_mut(&section_key)
                .expect("data section exists")
                .append(&encoded[..width])
                .map_err(|error| self.map_error(error, start, end))?;
        }
        Ok(())
    }

    fn parse_instruction(
        &mut self,
        start: usize,
        end: usize,
        tokens: Vec<Token>,
    ) -> Result<(), ToolchainError> {
        let architecture = self
            .architecture
            .ok_or_else(|| self.fail(start, end, "E130", "instruction appears before .arch"))?;
        if self.current != SectionKey::Text {
            return Err(self.fail(
                start,
                end,
                "E305",
                "instructions must be in the text section",
            ));
        }
        let mnemonic = self.word(&tokens, 0, "E270")?;
        let opcode = Opcode::ALL
            .iter()
            .copied()
            .find(|opcode| opcode.definition().mnemonic.eq_ignore_ascii_case(mnemonic))
            .ok_or_else(|| self.fail(start, end, "E138", "unsupported assembler instruction"))?;
        let definition = opcode.definition();
        let mut index = 1;
        let mut parsed = Vec::new();
        for (operand_index, operand_definition) in definition.operands().iter().enumerate() {
            if operand_index > 0 {
                if tokens.get(index).map(|token| &token.kind) != Some(&TokenKind::Comma) {
                    return Err(self.fail(
                        start,
                        end,
                        "E271",
                        "instruction operands must be comma-separated",
                    ));
                }
                index += 1;
            }
            let operand = self.parse_operand(operand_definition.kind, &tokens, &mut index)?;
            parsed.push(operand);
        }
        if index != tokens.len() {
            return Err(self.fail(start, end, "E272", "instruction has too many operands"));
        }
        let instruction_offset = self
            .sections
            .get(&SectionKey::Text)
            .map_or(0, SectionAccumulator::offset);
        let mut concrete = Vec::new();
        for (operand_index, operand) in parsed.iter().enumerate() {
            match operand {
                ParsedOperand::Concrete(value) => concrete.push(*value),
                ParsedOperand::Immediate(Expr::Integer(value), span) => {
                    concrete.push(Operand::Immediate(i32::try_from(*value).map_err(|_| {
                        self.fail(
                            span.start().as_u32() as usize,
                            span.end().as_u32() as usize,
                            "E273",
                            "immediate is outside signed 32-bit range",
                        )
                    })?))
                }
                ParsedOperand::Immediate(Expr::Symbol { name, addend }, span) => {
                    concrete.push(Operand::Immediate(0));
                    let kind = match (opcode, operand_index) {
                        (Opcode::Li, 1) => RelocationKind::LiImmediate,
                        (Opcode::Br, 1) | (Opcode::Call, 0) => RelocationKind::PcRelativeBranch,
                        _ => {
                            return Err(self.fail(
                                span.start().as_u32() as usize,
                                span.end().as_u32() as usize,
                                "E274",
                                "operand does not support symbol relocations",
                            ));
                        }
                    };
                    self.relocations.push(PendingRelocation {
                        symbol: name.clone(),
                        section: SectionKey::Text,
                        kind,
                        offset: instruction_offset,
                        addend: *addend,
                        span: span.clone(),
                    });
                }
                ParsedOperand::Memory {
                    base,
                    displacement,
                    span,
                } => {
                    let value = match displacement {
                        Expr::Integer(value) => i32::try_from(*value).map_err(|_| {
                            self.fail(
                                span.start().as_u32() as usize,
                                span.end().as_u32() as usize,
                                "E275",
                                "memory displacement is outside signed 32-bit range",
                            )
                        })?,
                        Expr::Symbol { name, addend } => {
                            self.relocations.push(PendingRelocation {
                                symbol: name.clone(),
                                section: SectionKey::Text,
                                kind: RelocationKind::MemoryDisplacement32,
                                offset: instruction_offset,
                                addend: *addend,
                                span: span.clone(),
                            });
                            0
                        }
                    };
                    concrete.push(Operand::Memory {
                        base: *base,
                        displacement: value,
                    });
                }
            }
        }
        let instruction = Instruction::new(architecture.config(), opcode, &concrete)
            .map_err(|_| self.fail(start, end, "E276", "instruction operands are invalid"))?;
        let encoded = encode(architecture.config(), &instruction)
            .map_err(|_| self.fail(start, end, "E277", "instruction cannot be encoded"))?;
        let section = self
            .sections
            .entry(SectionKey::Text)
            .or_insert_with(|| SectionAccumulator::new(SectionKey::Text));
        section
            .append(&encoded)
            .map_err(|error| self.map_error(error, start, end))?;
        self.mappings.push(PendingMapping {
            offset: instruction_offset,
            start,
            length: end - start,
        });
        Ok(())
    }

    fn parse_operand(
        &self,
        kind: lazalith_isa::OperandKind,
        tokens: &[Token],
        index: &mut usize,
    ) -> Result<ParsedOperand, ToolchainError> {
        match kind {
            lazalith_isa::OperandKind::Register => {
                let token = tokens.get(*index).ok_or_else(|| {
                    self.fail(
                        tokens
                            .last()
                            .map_or(0, |token| token.span.start().as_u32() as usize),
                        tokens
                            .last()
                            .map_or(0, |token| token.span.end().as_u32() as usize),
                        "E278",
                        "register operand is missing",
                    )
                })?;
                let value = self.word_token(token, "E279")?;
                let register = parse_register(value).ok_or_else(|| {
                    self.fail(
                        token.span.start().as_u32() as usize,
                        token.span.end().as_u32() as usize,
                        "E139",
                        "LI destination is not a valid register",
                    )
                })?;
                *index += 1;
                Ok(ParsedOperand::Concrete(Operand::Register(register)))
            }
            lazalith_isa::OperandKind::Immediate => {
                let span = self.expression_span(tokens, index);
                let expression = self.parse_expression(tokens, index)?;
                Ok(ParsedOperand::Immediate(expression, span))
            }
            lazalith_isa::OperandKind::Memory => {
                let Some(open) = tokens.get(*index) else {
                    return Err(self.fail(
                        0,
                        self.source_length as usize,
                        "E280",
                        "memory operand must begin with '['",
                    ));
                };
                if open.kind != TokenKind::LBracket {
                    return Err(self.fail(
                        open.span.start().as_u32() as usize,
                        open.span.end().as_u32() as usize,
                        "E280",
                        "memory operand must begin with '['",
                    ));
                }
                *index += 1;
                let base_token = tokens.get(*index).ok_or_else(|| {
                    self.fail(
                        0,
                        self.source_length as usize,
                        "E281",
                        "memory base register is missing",
                    )
                })?;
                let base =
                    parse_register(self.word_token(base_token, "E282")?).ok_or_else(|| {
                        self.fail(
                            base_token.span.start().as_u32() as usize,
                            base_token.span.end().as_u32() as usize,
                            "E283",
                            "memory base is not a valid register",
                        )
                    })?;
                *index += 1;
                let displacement = if tokens.get(*index).map(|token| &token.kind)
                    == Some(&TokenKind::Plus)
                    || tokens.get(*index).map(|token| &token.kind) == Some(&TokenKind::Minus)
                {
                    self.parse_expression(tokens, index)?
                } else {
                    Expr::Integer(0)
                };
                if tokens.get(*index).map(|token| &token.kind) != Some(&TokenKind::RBracket) {
                    return Err(self.fail(
                        tokens
                            .get(*index)
                            .map_or(self.source_length as usize, |token| {
                                token.span.start().as_u32() as usize
                            }),
                        tokens
                            .get(*index)
                            .map_or(self.source_length as usize, |token| {
                                token.span.end().as_u32() as usize
                            }),
                        "E284",
                        "memory operand must end with ']'",
                    ));
                }
                let span = SourceSpan::clone(&tokens[*index - 1].span);
                *index += 1;
                Ok(ParsedOperand::Memory {
                    base,
                    displacement,
                    span,
                })
            }
            lazalith_isa::OperandKind::DataSize => {
                let token = tokens.get(*index).ok_or_else(|| {
                    self.fail(
                        0,
                        self.source_length as usize,
                        "E285",
                        "data size is missing",
                    )
                })?;
                let value = self.word_token(token, "E286")?;
                let size = match value.to_ascii_lowercase().as_str() {
                    "byte" => DataSize::Byte,
                    "half" => DataSize::Half,
                    "word" => DataSize::Word,
                    "double" => DataSize::Double,
                    _ => {
                        return Err(self.fail(
                            token.span.start().as_u32() as usize,
                            token.span.end().as_u32() as usize,
                            "E287",
                            "unknown data size selector",
                        ));
                    }
                };
                *index += 1;
                Ok(ParsedOperand::Concrete(Operand::DataSize(size)))
            }
            lazalith_isa::OperandKind::Condition => {
                let token = tokens.get(*index).ok_or_else(|| {
                    self.fail(
                        0,
                        self.source_length as usize,
                        "E288",
                        "condition is missing",
                    )
                })?;
                let value = self.word_token(token, "E289")?;
                let condition = match value.to_ascii_lowercase().as_str() {
                    "al" => TypeCondition::Al,
                    "eq" => TypeCondition::Eq,
                    "ne" => TypeCondition::Ne,
                    "ult" => TypeCondition::Ult,
                    "uge" => TypeCondition::Uge,
                    "ule" => TypeCondition::Ule,
                    "ugt" => TypeCondition::Ugt,
                    "slt" => TypeCondition::Slt,
                    "sge" => TypeCondition::Sge,
                    "sle" => TypeCondition::Sle,
                    "sgt" => TypeCondition::Sgt,
                    "vs" => TypeCondition::Vs,
                    "vc" => TypeCondition::Vc,
                    "mi" => TypeCondition::Mi,
                    "pl" => TypeCondition::Pl,
                    _ => {
                        return Err(self.fail(
                            token.span.start().as_u32() as usize,
                            token.span.end().as_u32() as usize,
                            "E290",
                            "unknown condition selector",
                        ));
                    }
                };
                *index += 1;
                Ok(ParsedOperand::Concrete(Operand::Condition(condition)))
            }
            lazalith_isa::OperandKind::Control => {
                let token = tokens.get(*index).ok_or_else(|| {
                    self.fail(
                        0,
                        self.source_length as usize,
                        "E291",
                        "control register is missing",
                    )
                })?;
                let value = self.word_token(token, "E292")?;
                let control = match value.to_ascii_lowercase().as_str() {
                    "tvec" => ControlRegister::Tvec,
                    "epc" => ControlRegister::Epc,
                    "esp" => ControlRegister::Esp,
                    "estatus" => ControlRegister::Estatus,
                    "tcause" => ControlRegister::Tcause,
                    "tpayload" => ControlRegister::Tpayload,
                    _ => {
                        return Err(self.fail(
                            token.span.start().as_u32() as usize,
                            token.span.end().as_u32() as usize,
                            "E293",
                            "unknown control register selector",
                        ));
                    }
                };
                *index += 1;
                Ok(ParsedOperand::Concrete(Operand::Control(control)))
            }
        }
    }

    fn parse_expression(
        &self,
        tokens: &[Token],
        index: &mut usize,
    ) -> Result<Expr, ToolchainError> {
        let mut sign = 1i128;
        if tokens.get(*index).map(|token| &token.kind) == Some(&TokenKind::Minus) {
            sign = -1;
            *index += 1;
        } else if tokens.get(*index).map(|token| &token.kind) == Some(&TokenKind::Plus) {
            *index += 1;
        }
        let token = tokens.get(*index).ok_or_else(|| {
            self.fail(
                0,
                self.source_length as usize,
                "E294",
                "expression is missing",
            )
        })?;
        let value = match &token.kind {
            TokenKind::Integer(value) => {
                *index += 1;
                Expr::Integer(sign.checked_mul(*value).ok_or_else(|| {
                    self.fail(
                        token.span.start().as_u32() as usize,
                        token.span.end().as_u32() as usize,
                        "E296",
                        "integer literal is out of range",
                    )
                })?)
            }
            TokenKind::Word(name) if valid_identifier(name) => {
                if sign < 0 {
                    return Err(self.fail(
                        token.span.start().as_u32() as usize,
                        token.span.end().as_u32() as usize,
                        "E306",
                        "negative symbol expressions are not representable",
                    ));
                }
                *index += 1;
                let mut addend = 0i64;
                loop {
                    let op = tokens.get(*index).map(|token| &token.kind);
                    if op == Some(&TokenKind::Plus) || op == Some(&TokenKind::Minus) {
                        let sign: i64 = if op == Some(&TokenKind::Minus) { -1 } else { 1 };
                        *index += 1;
                        let number = tokens
                            .get(*index)
                            .and_then(|token| {
                                if let TokenKind::Integer(value) = token.kind {
                                    Some(value)
                                } else {
                                    None
                                }
                            })
                            .ok_or_else(|| {
                                self.fail(
                                    token.span.start().as_u32() as usize,
                                    token.span.end().as_u32() as usize,
                                    "E295",
                                    "symbol expression addend must be an integer",
                                )
                            })?;
                        let magnitude = i64::try_from(number).map_err(|_| {
                            self.fail(
                                token.span.start().as_u32() as usize,
                                token.span.end().as_u32() as usize,
                                "E296",
                                "symbol addend is out of range",
                            )
                        })?;
                        let addend_value = sign.checked_mul(magnitude).ok_or_else(|| {
                            self.fail(
                                token.span.start().as_u32() as usize,
                                token.span.end().as_u32() as usize,
                                "E296",
                                "symbol addend is out of range",
                            )
                        })?;
                        addend = addend.checked_add(addend_value).ok_or_else(|| {
                            self.fail(
                                token.span.start().as_u32() as usize,
                                token.span.end().as_u32() as usize,
                                "E297",
                                "symbol addend overflows",
                            )
                        })?;
                        *index += 1;
                    } else {
                        break;
                    }
                }
                Expr::Symbol {
                    name: name.clone(),
                    addend,
                }
            }
            _ => {
                return Err(self.fail(
                    token.span.start().as_u32() as usize,
                    token.span.end().as_u32() as usize,
                    "E298",
                    "expression must be an integer or symbol",
                ));
            }
        };
        Ok(value)
    }

    fn expression_span(&self, tokens: &[Token], index: &usize) -> SourceSpan {
        tokens
            .get(*index)
            .map(|token| token.span.clone())
            .unwrap_or_else(|| self.span(0, 0).unwrap_or_else(|_| unreachable!()))
    }

    fn finish(&self) -> Result<ObjectFile, ToolchainError> {
        let architecture = self
            .architecture
            .ok_or_else(|| self.fail(0, 0, "E100", "assembly requires an .arch directive"))?;
        let entry_name = self
            .entry
            .clone()
            .ok_or_else(|| self.fail(0, 0, "E101", "assembly requires an .entry directive"))?;
        if architecture == LzxArchitecture::Lz32
            && let Some(span) = &self.dword_span
        {
            return Err(self.fail(
                span.start().as_u32() as usize,
                span.end().as_u32() as usize,
                "E266",
                ".dword requires LZ64",
            ));
        }
        let text_offset = self
            .sections
            .get(&SectionKey::Text)
            .map_or(0, SectionAccumulator::offset);
        if text_offset == 0 {
            return Err(self.fail(0, 0, "E104", "entry program contains no instructions"));
        }
        if self
            .sections
            .get(&SectionKey::Bss)
            .is_some_and(|section| section.used && section.bss_size == 0)
        {
            return Err(self.fail(
                0,
                0,
                "E309",
                "BSS section must contain a nonzero allocation",
            ));
        }
        let mut builder = ObjectBuilder::new(architecture.config());
        let mut section_indices = BTreeMap::new();
        for key in SectionKey::ORDER {
            let Some(section) = self.sections.get(&key) else {
                continue;
            };
            if !section.used && key != SectionKey::Text {
                continue;
            }
            let constructed = match key {
                SectionKey::Text => Section::text("text", architecture.config(), &section.bytes),
                SectionKey::ReadOnlyData => {
                    Section::read_only_data("rodata", section.alignment, &section.bytes)
                }
                SectionKey::Data => Section::data("data", section.alignment, &section.bytes),
                SectionKey::Bss => Section::bss("bss", section.alignment, section.bss_size),
            };
            let index = builder
                .add_section(constructed.map_err(ToolchainError::Object)?)
                .map_err(ToolchainError::Object)?;
            section_indices.insert(key, index);
        }
        let mut symbol_indices = BTreeMap::new();
        for definition in &self.symbols {
            let binding = if definition.global {
                SymbolBinding::Global
            } else {
                SymbolBinding::Local
            };
            let symbol = match &definition.kind {
                DefKind::Undefined => Symbol::undefined(definition.name.clone(), binding),
                DefKind::Absolute => {
                    if architecture == LzxArchitecture::Lz32
                        && definition.value > u64::from(u32::MAX)
                    {
                        return Err(self.fail(
                            definition.span.start().as_u32() as usize,
                            definition.span.end().as_u32() as usize,
                            "E308",
                            "absolute value does not fit LZ32",
                        ));
                    }
                    Symbol::absolute(definition.name.clone(), binding, definition.value)
                }
                DefKind::Section(key) => {
                    let section = section_indices.get(key).copied().ok_or_else(|| {
                        self.fail(
                            definition.span.start().as_u32() as usize,
                            definition.span.end().as_u32() as usize,
                            "E299",
                            "symbol section is unavailable",
                        )
                    })?;
                    Symbol::section_defined(
                        definition.name.clone(),
                        binding,
                        section,
                        definition.value,
                        definition.size,
                    )
                }
            };
            let index = builder.add_symbol(symbol).map_err(ToolchainError::Object)?;
            symbol_indices.insert(definition.name.clone(), index);
        }
        let entry = symbol_indices
            .get(&entry_name)
            .copied()
            .ok_or_else(|| self.fail(0, 0, "E102", "entry label is not defined"))?;
        builder.set_entry(entry).map_err(ToolchainError::Object)?;
        let mut relocations = self.relocations.clone();
        relocations.sort_by_key(|relocation| (relocation.section, relocation.offset));
        for pending in &relocations {
            let symbol = symbol_indices
                .get(&pending.symbol)
                .copied()
                .ok_or_else(|| {
                    self.fail(
                        pending.span.start().as_u32() as usize,
                        pending.span.end().as_u32() as usize,
                        "E300",
                        "symbol is not defined or external",
                    )
                })?;
            let section = section_indices
                .get(&pending.section)
                .copied()
                .ok_or_else(|| {
                    self.fail(
                        pending.span.start().as_u32() as usize,
                        pending.span.end().as_u32() as usize,
                        "E301",
                        "relocation section is unavailable",
                    )
                })?;
            builder
                .add_relocation(Relocation::new(
                    symbol,
                    section,
                    pending.kind,
                    pending.offset,
                    pending.addend,
                ))
                .map_err(ToolchainError::Object)?;
        }
        if !self.mappings.is_empty() {
            let source = builder
                .add_debug_source(DebugSource::new(
                    self.source_name.clone(),
                    self.source_length,
                ))
                .map_err(ToolchainError::Object)?;
            let text = section_indices
                .get(&SectionKey::Text)
                .copied()
                .ok_or_else(|| self.fail(0, 0, "E302", "text section is unavailable"))?;
            for mapping in &self.mappings {
                builder
                    .add_debug_mapping(CodeMapping::new(
                        text,
                        mapping.offset,
                        source,
                        u32::try_from(mapping.start).map_err(|_| {
                            self.fail(
                                mapping.start,
                                mapping.start,
                                "E303",
                                "debug offset is out of range",
                            )
                        })?,
                        u32::try_from(mapping.length).map_err(|_| {
                            self.fail(
                                mapping.start,
                                mapping.start + mapping.length,
                                "E304",
                                "debug length is out of range",
                            )
                        })?,
                    ))
                    .map_err(ToolchainError::Object)?;
            }
        }
        builder.build().map_err(ToolchainError::Object)
    }

    fn word<'b>(
        &self,
        tokens: &'b [Token],
        index: usize,
        code: &'static str,
    ) -> Result<&'b str, ToolchainError> {
        match tokens.get(index).map(|token| &token.kind) {
            Some(TokenKind::Word(value)) if valid_identifier(value) => Ok(value),
            _ => Err(self.fail(
                tokens
                    .get(index)
                    .map_or(0, |token| token.span.start().as_u32() as usize),
                tokens
                    .get(index)
                    .map_or(0, |token| token.span.end().as_u32() as usize),
                code,
                "expected an identifier",
            )),
        }
    }
    fn word_token<'b>(
        &self,
        token: &'b Token,
        code: &'static str,
    ) -> Result<&'b str, ToolchainError> {
        match &token.kind {
            TokenKind::Word(value) if valid_identifier(value) => Ok(value),
            _ => Err(self.fail(
                token.span.start().as_u32() as usize,
                token.span.end().as_u32() as usize,
                code,
                "expected an identifier",
            )),
        }
    }
    fn integer(
        &self,
        tokens: &[Token],
        index: usize,
        code: &'static str,
    ) -> Result<i128, ToolchainError> {
        match tokens.get(index).map(|token| &token.kind) {
            Some(TokenKind::Integer(value)) => Ok(*value),
            _ => Err(self.fail(
                tokens
                    .get(index)
                    .map_or(0, |token| token.span.start().as_u32() as usize),
                tokens
                    .get(index)
                    .map_or(0, |token| token.span.end().as_u32() as usize),
                code,
                "expected an integer",
            )),
        }
    }
    fn span(&self, start: usize, end: usize) -> Result<SourceSpan, ToolchainError> {
        self.manager
            .source_span(
                self.source_id,
                ByteOffset::new(start as u32),
                ByteOffset::new(end as u32),
            )
            .map_err(|source| {
                ToolchainError::Assembly(Box::new(AssemblyError::InvalidSpan(source)))
            })
    }
    fn fail(&self, start: usize, end: usize, code: &str, message: &str) -> ToolchainError {
        let span = match self.manager.source_span(
            self.source_id,
            ByteOffset::new(start as u32),
            ByteOffset::new(end as u32),
        ) {
            Ok(span) => span,
            Err(source) => {
                return ToolchainError::Assembly(Box::new(AssemblyError::InvalidSpan(source)));
            }
        };
        let code = DiagnosticCode::new(code).expect("assembler diagnostic code is valid");
        ToolchainError::Assembly(Box::new(AssemblyError::Diagnostic {
            diagnostic: Diagnostic::new(Severity::Error, code, message)
                .with_label(Label::primary(span, message)),
            sources: self.manager.clone(),
        }))
    }
    fn map_error(&self, _error: ToolchainError, start: usize, end: usize) -> ToolchainError {
        self.fail(start, end, "E303", "assembler operation failed")
    }
}

fn parse_register(value: &str) -> Option<RegisterIndex> {
    let digits = value
        .strip_prefix('r')
        .or_else(|| value.strip_prefix('R'))?;
    RegisterIndex::try_from(digits.parse::<u8>().ok()?).ok()
}
fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first == '.' || first.is_ascii_alphabetic())
        && chars.all(|character| {
            character == '_'
                || character == '.'
                || character == '$'
                || character.is_ascii_alphanumeric()
        })
}
fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte == b'.' || byte.is_ascii_alphabetic()
}
fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit() || byte == b'$'
}
fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => 0,
    }
}
