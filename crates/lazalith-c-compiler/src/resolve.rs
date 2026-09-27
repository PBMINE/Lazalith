//! Name resolution: what a name means.
//!
//! # What this stage is for
//!
//! C's scopes are small and its rules are mostly about *where a name was
//! written*, so this stage does three things and nothing else:
//!
//! - It records every `typedef` name, because the parser needed the same list
//!   to tell a declaration from an expression and the two must not disagree.
//! - It records every `struct`, `union` and `enum` tag in its own namespace. C
//!   has a separate tag namespace, so a `struct` named `state` and a variable
//!   named `state` coexist, and a compiler with one table for both refuses
//!   correct programs.
//! - It records every ordinary name, so the type checker can say what a name is
//!   rather than only that it exists.
//!
//! # Types are not this stage's job
//!
//! Nothing here stores a [`CType`]. A type needs the whole declaration to build
//! — an array's length is an expression, a `struct`'s layout is its members —
//! and a stage that stored half-built types would be a second type system. So
//! [`Binding`] records what a name *is* and where it was written, and the type
//! checker builds the type when it needs it.
//!
//! # What it refuses
//!
//! A name that cannot mean anything: an undeclared name, a name declared twice
//! in one scope, a tag used before it is defined, a `typedef` used where a
//! value belongs, and a `goto` whose label is not in the function.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use lazalith_types::{SourceId, SourceManager, SourceSpan};

use crate::ast::*;
use crate::diagnostic::StageError;

/// C's diagnostic codes for this stage.
pub mod codes {
    /// A name that is not declared anywhere it is used.
    pub const UNDECLARED: &str = "C0301";
    /// A name or a tag declared twice in one scope.
    pub const REDECLARED: &str = "C0302";
    /// A tag that is not declared.
    pub const UNKNOWN_TAG: &str = "C0303";
    /// A `goto` whose label is not in the function.
    pub const UNKNOWN_LABEL: &str = "C0304";
    /// A name used where a value is expected, but it names a type.
    pub const TYPE_AS_VALUE: &str = "C0305";
}

/// What a name is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingKind {
    /// A variable, a parameter, or a block-scope declaration.
    Variable,
    /// A function, whether a definition or a prototype.
    Function,
    /// An `enum` constant, which is a value even though its type is an enum.
    Enumerator,
    /// A `typedef` name, which is a type and not a value.
    TypeName,
}

/// One ordinary name.
#[derive(Clone, Debug)]
pub struct Binding {
    /// The name.
    pub name: String,
    /// What it is.
    pub kind: BindingKind,
    /// Where it was declared.
    pub span: SourceSpan,
    /// Whether it is a file-scope name, which outlives every block.
    pub file_scope: bool,
    /// Whether it was declared `extern`, or a prototype.
    pub external: bool,
}

/// One function's worth of collected names.
#[derive(Clone, Debug)]
pub struct FunctionScope {
    /// The function's name.
    pub name: String,
    /// Where it is.
    pub span: SourceSpan,
    /// Every label in it.
    pub labels: Vec<(String, SourceSpan)>,
    /// Every label a `goto` in it names.
    pub targets: Vec<(String, SourceSpan)>,
    /// Whether it returns `void`, which decides whether a `return` needs a
    /// value and whether falling off the end is right.
    pub returns_void: bool,
}

/// Everything resolution found.
#[derive(Debug)]
pub struct Resolved {
    /// The tree, unchanged.
    ///
    /// This stage annotates rather than rewrites. A rewritten tree would mean
    /// the type checker walking two representations, and two representations
    /// of one program is one more thing to keep in step.
    pub unit: TranslationUnit,
    /// The file-scope names.
    pub globals: BTreeMap<String, Binding>,
    /// The tags declared at file scope.
    pub tags: BTreeMap<String, SourceSpan>,
    /// The `typedef` names and the specifier each one names.
    ///
    /// The specifier is kept rather than a type because building a type is the
    /// type checker's job, and a `typedef` can name a `struct` that is not
    /// complete until later in the file.
    pub typedefs: BTreeMap<String, (TypeSpecifier, SourceSpan)>,
    /// One entry per function definition.
    pub functions: Vec<FunctionScope>,
    /// Every failure.
    pub diagnostics: Vec<StageError>,
}

/// Resolves every name in a translation unit.
pub fn resolve(source: SourceId, sources: &SourceManager, unit: TranslationUnit) -> Resolved {
    let mut resolver = Resolver {
        source,
        sources,
        scopes: vec![Scope::new()],
        globals: BTreeMap::new(),
        tags: BTreeMap::new(),
        typedefs: BTreeMap::new(),
        functions: Vec::new(),
        errors: Vec::new(),
        function: None,
    };
    resolver.translation_unit(&unit);
    Resolved {
        unit,
        globals: resolver.globals,
        tags: resolver.tags,
        typedefs: resolver.typedefs,
        functions: resolver.functions,
        diagnostics: resolver.errors,
    }
}

/// One lexical scope.
#[derive(Debug, Default)]
struct Scope {
    names: BTreeMap<String, Binding>,
    tags: BTreeMap<String, SourceSpan>,
}

impl Scope {
    fn new() -> Self {
        Self::default()
    }
}

struct Resolver<'a> {
    source: SourceId,
    sources: &'a SourceManager,
    scopes: Vec<Scope>,
    globals: BTreeMap<String, Binding>,
    tags: BTreeMap<String, SourceSpan>,
    typedefs: BTreeMap<String, (TypeSpecifier, SourceSpan)>,
    functions: Vec<FunctionScope>,
    errors: Vec<StageError>,
    function: Option<FunctionScope>,
}

impl<'a> Resolver<'a> {
    fn translation_unit(&mut self, unit: &TranslationUnit) {
        for declaration in &unit.declarations {
            match declaration {
                Declaration::Function(definition) => self.function_definition(definition),
                Declaration::Declaration(declaration) => self.declaration(declaration, true),
                Declaration::Record(definition) => self.record_definition(definition),
                Declaration::Enum(definition) => self.enum_definition(definition),
                Declaration::StaticAssert(_) => {}
            }
        }
        self.function = None;
    }

    /// Records a refusal at a span.
    ///
    /// The help is required rather than optional: every refusal here has a reason,
    /// and a refusal without one is not something a reader can act on.
    fn error(
        &mut self,
        span: SourceSpan,
        code: &str,
        message: impl Into<String>,
        help: impl Into<String>,
    ) {
        let help = help.into();
        self.errors.push(crate::diagnostic::at(
            self.source,
            self.sources,
            span,
            code,
            message,
            &[],
            Some(&help),
        ));
    }

    // -- declaring --

    /// Records a name in the innermost scope, refusing a second one.
    ///
    /// A second declaration in the *same* scope is refused, and a second in an
    /// outer one is allowed, because that is C: an inner declaration shadows an
    /// outer one, and a program that does so is legal and sometimes intended.
    fn declare(&mut self, binding: Binding) {
        if let Some(previous) = self
            .scopes
            .last()
            .and_then(|scope| scope.names.get(&binding.name))
        {
            let previous_line = line_of(self.sources, previous.span.clone());
            self.error(
                binding.span,
                codes::REDECLARED,
                alloc::format!("`{}` is already declared in this scope", binding.name),
                alloc::format!(
                    "the earlier one is at line {previous_line}; two names in one scope \
                     make the program's meaning depend on which one is meant"
                ),
            );
            return;
        }
        if binding.file_scope {
            self.globals.insert(binding.name.clone(), binding.clone());
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.names.insert(binding.name.clone(), binding);
        }
    }

    /// The innermost binding for a name.
    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.names.get(name))
    }

    /// The innermost tag with a name, and where it was declared.
    fn lookup_tag(&self, name: &str) -> Option<SourceSpan> {
        for scope in self.scopes.iter().rev() {
            if let Some(span) = scope.tags.get(name) {
                return Some(span.clone());
            }
        }
        self.tags.get(name).cloned()
    }

    /// Records a tag, refusing a redefinition.
    ///
    /// A tag may be *referred* to as often as it is liked. Only *defining* one
    /// twice is a mistake, and saying so is the difference between a compiler
    /// that accepts `struct S *next;` inside `struct S` and one that does not.
    fn declare_tag(&mut self, name: String, span: &SourceSpan) {
        if let Some(previous) = self.lookup_tag(&name) {
            let line = line_of(self.sources, previous.clone());
            self.error(
                span.clone(),
                codes::REDECLARED,
                alloc::format!("the tag `{name}` is already defined"),
                alloc::format!(
                    "the earlier definition is at line {line}; a tag may be referred to \
                     any number of times, but only defined once"
                ),
            );
            return;
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.tags.insert(name.clone(), span.clone());
        }
        self.tags.insert(name, span.clone());
    }

    fn record_definition(&mut self, definition: &RecordDefinition) {
        if let Some(tag) = &definition.tag {
            self.declare_tag(tag.clone(), &definition.span);
        }
    }

    fn enum_definition(&mut self, definition: &EnumDefinition) {
        if let Some(tag) = &definition.tag {
            self.declare_tag(tag.clone(), &definition.span);
        }
        let file_scope = self.scopes.len() == 1;
        if let Some(members) = &definition.members {
            for member in members {
                self.declare(Binding {
                    name: member.name.clone(),
                    kind: BindingKind::Enumerator,
                    span: member.span.clone(),
                    file_scope,
                    external: false,
                });
            }
        }
    }

    fn declaration(&mut self, declaration: &VarDecl, file_scope: bool) {
        // A tag mentioned by a declaration becomes visible for the rest of the
        // scope, which is what makes `struct S { ... } s;` usable afterwards.
        self.collect_tags(&declaration.base);
        for declarator in &declaration.declarators {
            let Some(name) = &declarator.name else {
                continue;
            };
            if declaration.typedef {
                if let Some((_, previous)) = self.typedefs.get(name) {
                    let line = line_of(self.sources, previous.clone());
                    self.error(
                        declarator.span.clone(),
                        codes::REDECLARED,
                        alloc::format!("`{name}` is already a type name"),
                        alloc::format!(
                            "declared at line {line}; a typedef name may not be redefined, \
                             even to the same type"
                        ),
                    );
                    continue;
                }
                self.typedefs.insert(
                    name.clone(),
                    (declaration.base.clone(), declarator.span.clone()),
                );
                // A typedef name lives in the *type* namespace and is also
                // visible as a name, so the parser's declaration-versus-
                // expression question is answered by this same table.
                self.declare(Binding {
                    name: name.clone(),
                    kind: BindingKind::TypeName,
                    span: declarator.span.clone(),
                    file_scope,
                    external: false,
                });
                continue;
            }
            let is_function = declarator
                .derivation
                .last()
                .is_some_and(|derivation| matches!(derivation, Derivation::Function(_, _)));
            self.declare(Binding {
                name: name.clone(),
                kind: if is_function {
                    BindingKind::Function
                } else {
                    BindingKind::Variable
                },
                span: declarator.span.clone(),
                file_scope,
                external: declaration.extern_ || is_function,
            });
        }
    }

    /// Records every tag a base type mentions.
    ///
    /// A *mention* with no definition is a reference, and it must already
    /// exist — that is how a forward declaration earns its keep. A mention with
    /// a definition is a definition, and it is recorded instead.
    fn collect_tags(&mut self, base: &TypeSpecifier) {
        match &base.base {
            BaseType::Record(reference) => {
                if let Some(tag) = &reference.tag {
                    match &reference.definition {
                        Some(definition) => self.record_definition(definition),
                        None => {
                            if self.lookup_tag(tag).is_none() {
                                self.error(
                                    reference.span.clone(),
                                    codes::UNKNOWN_TAG,
                                    alloc::format!("no type named `{tag}` is declared"),
                                    alloc::format!(
                                        "declare it with `struct {tag} {{ ... }};` before it \
                                         is used, or forward-declare it with \
                                         `struct {tag};`"
                                    ),
                                );
                            }
                        }
                    }
                }
            }
            BaseType::Enum(reference) => {
                if let Some(tag) = &reference.tag {
                    match &reference.definition {
                        Some(definition) => self.enum_definition(definition),
                        None => {
                            if self.lookup_tag(tag).is_none() {
                                self.error(
                                    reference.span.clone(),
                                    codes::UNKNOWN_TAG,
                                    alloc::format!("no type named `{tag}` is declared"),
                                    alloc::format!(
                                        "declare it with `enum {tag} {{ ... }};` before it is used"
                                    ),
                                );
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn function_definition(&mut self, definition: &FunctionDefinition) {
        // A file-scope function does not inherit anything from the previous
        // function, so the scope stack goes back to the file's own.
        self.scopes.truncate(1);
        self.collect_tags(&definition.base);
        self.scopes.push(Scope::new());
        // A function's own name is in the *file's* scope, not only in its body.
        // A function is visible to every other function in the translation unit,
        // and C says so: `f` may call `g` when `g` is defined below it. Putting
        // the name in the file scope is what makes that true, and it is also
        // what makes a second definition of the same name a *redefinition* rather
        // than two unconnected names.
        if let Some(scope) = self.scopes.first_mut() {
            scope.names.insert(
                definition.name.clone(),
                Binding {
                    name: definition.name.clone(),
                    kind: BindingKind::Function,
                    span: definition.span.clone(),
                    file_scope: true,
                    external: false,
                },
            );
        }
        self.declare(Binding {
            name: definition.name.clone(),
            kind: BindingKind::Function,
            span: definition.span.clone(),
            file_scope: false,
            external: false,
        });
        if let Some(Derivation::Function(parameters, _)) = definition.declarator.derivation.last() {
            for parameter in parameters {
                let Some(declarator) = &parameter.declarator else {
                    continue;
                };
                let Some(name) = &declarator.name else {
                    continue;
                };
                self.declare(Binding {
                    name: name.clone(),
                    kind: BindingKind::Variable,
                    span: parameter.span.clone(),
                    file_scope: false,
                    external: false,
                });
            }
        }
        self.function = Some(FunctionScope {
            name: definition.name.clone(),
            span: definition.span.clone(),
            labels: Vec::new(),
            targets: Vec::new(),
            // Filled in by the type checker, which is the only stage that can
            // build the return type. A wrong value here would change which
            // `return`s are reported, so it defaults to the *permissive* one:
            // reporting nothing is better than reporting something false.
            returns_void: false,
        });
        self.block(&definition.body);
        if let Some(function) = self.function.take() {
            for (name, span) in &function.targets {
                if !function.labels.iter().any(|(label, _)| label == name) {
                    self.error(
                        span.clone(),
                        codes::UNKNOWN_LABEL,
                        alloc::format!(
                            "`{name}` is a label this `goto` names, and it is not in the function"
                        ),
                        "a `goto` can only reach a label in its own function",
                    );
                }
            }
            self.functions.push(function);
        }
        self.scopes.pop();
    }

    fn block(&mut self, block: &Block) {
        self.scopes.push(Scope::new());
        for item in &block.items {
            match item {
                BlockItem::Declaration(declaration) => self.declaration(declaration, false),
                BlockItem::Statement(statement) => self.statement(statement),
            }
        }
        self.scopes.pop();
    }

    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Block(block) => self.block(block),
            Statement::Expression(expression) => self.expression(expression),
            Statement::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.expression(condition);
                self.statement(then_branch);
                if let Some(else_branch) = else_branch {
                    self.statement(else_branch);
                }
            }
            Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
                self.expression(condition);
                self.statement(body);
            }
            Statement::For {
                initialiser,
                condition,
                step,
                body,
            } => {
                // A `for`'s declaration is scoped to the loop, which is the one
                // scope C has that is not a block.
                self.scopes.push(Scope::new());
                if let Some(init) = initialiser {
                    match &**init {
                        ForInit::Declaration(declaration) => self.declaration(declaration, false),
                        ForInit::Expression(expression) => self.expression(expression),
                    }
                }
                if let Some(condition) = condition {
                    self.expression(condition);
                }
                if let Some(step) = step {
                    self.expression(step);
                }
                self.statement(body);
                self.scopes.pop();
            }
            Statement::Switch { condition, body } => {
                self.expression(condition);
                self.statement(body);
            }
            Statement::Case { value, statement } => {
                self.expression(value);
                self.statement(statement);
            }
            Statement::Default { statement, .. } => self.statement(statement),
            Statement::Break(_) | Statement::Continue(_) | Statement::Empty => {}
            Statement::StaticAssert(_) => {}
            Statement::Return { value, .. } => {
                if let Some(value) = value {
                    self.expression(value);
                }
            }
            Statement::Goto { name, span } => {
                if let Some(function) = &mut self.function {
                    function.targets.push((name.clone(), span.clone()));
                }
            }
            Statement::Label {
                name,
                statement,
                span,
            } => {
                if let Some(function) = &mut self.function {
                    function.labels.push((name.clone(), span.clone()));
                }
                self.statement(statement);
            }
            Statement::Declaration(declaration) => self.declaration(declaration, false),
        }
    }

    /// Walks an expression for its names.
    ///
    /// A name that resolves to nothing is reported here, because that is a
    /// question about names and not about types. Reporting it here also means
    /// the type checker never has to ask whether a name exists, and so can never
    /// forget to.
    fn expression(&mut self, expression: &Expression) {
        match expression {
            Expression::Name { name, span } => {
                if let Some(binding) = self.lookup(name) {
                    if binding.kind == BindingKind::TypeName {
                        self.error(
                            span.clone(),
                            codes::TYPE_AS_VALUE,
                            alloc::format!("`{name}` names a type, not a value"),
                            "a type name cannot be used where a value is expected",
                        );
                    }
                    return;
                }
                if self.lookup_tag(name).is_some() {
                    self.error(
                        span.clone(),
                        codes::TYPE_AS_VALUE,
                        alloc::format!("`{name}` names a type, not a value"),
                        "a tag needs `struct`, `union` or `enum` in front of it to name a \
                         value of that type",
                    );
                    return;
                }
                // A name the OS ABI has is declared by the ABI, not by the
                // program's own declarations. A C program calls `write` without
                // declaring it, and reporting that as undeclared would make the
                // one facility a C program most wants unavailable.
                if lazalith_os_abi::abi_syscall(name).is_some() {
                    return;
                }
                self.error(
                    span.clone(),
                    codes::UNDECLARED,
                    alloc::format!("`{name}` is not declared"),
                    "every name a program uses must be declared before it is used; a name \
                     the OS ABI has is the exception and needs no declaration",
                );
            }
            Expression::Integer { .. }
            | Expression::Character { .. }
            | Expression::String { .. } => {}
            Expression::SizeofType(name) => self.collect_tags(&name.base),
            Expression::SizeofExpression(inner) => self.expression(inner),
            Expression::Group(inner) => self.expression(inner),
            Expression::Address(inner)
            | Expression::Dereference(inner)
            | Expression::Plus(inner)
            | Expression::Minus(inner)
            | Expression::BitNot(inner)
            | Expression::Not(inner) => self.expression(inner),
            Expression::Increment { operand, .. } => self.expression(operand),
            Expression::Binary { left, right, .. }
            | Expression::Logical { left, right, .. }
            | Expression::Comma { left, right } => {
                self.expression(left);
                self.expression(right);
            }
            Expression::Conditional {
                condition,
                then_value,
                else_value,
            } => {
                self.expression(condition);
                self.expression(then_value);
                self.expression(else_value);
            }
            Expression::Call { callee, arguments } => {
                self.expression(callee);
                for argument in arguments {
                    self.expression(argument);
                }
            }
            Expression::Subscript { array, index } => {
                self.expression(array);
                self.expression(index);
            }
            Expression::Member { record, .. } => self.expression(record),
            Expression::Assign { target, value }
            | Expression::CompoundAssign { target, value, .. } => {
                self.expression(target);
                self.expression(value);
            }
            Expression::Cast { ty, operand } => {
                self.collect_tags(&ty.base);
                self.expression(operand);
            }
        }
    }
}

/// The line a span is on, for a diagnostic that points at another one.
fn line_of(sources: &SourceManager, span: SourceSpan) -> u32 {
    sources
        .line_column(span.id(), span.start())
        .map(|position| position.line)
        .unwrap_or(0)
}

/// The `typedef` names a resolution found, for a caller that needs the names and
/// not the types.
pub fn typedef_names(resolved: &Resolved) -> BTreeSet<String> {
    resolved.typedefs.keys().cloned().collect()
}
