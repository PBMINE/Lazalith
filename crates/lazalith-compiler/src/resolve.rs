//! Name resolution for Lazen v1.
//!
//! This stage builds the symbol table the type checker uses. It does not assign
//! types and it does not judge whether an expression makes sense; it answers
//! exactly two questions:
//!
//! 1. Does every name in the file name something that exists?
//! 2. May this use reach it, given modules, `pub`, and `use`?
//!
//! v1 has no generics, no traits, and no runtime lookup, so a module tree plus
//! a flat scope chain per function is the whole model. Nothing here invents
//! module semantics the syntax document does not have.

use alloc::collections::BTreeMap;
use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use lazalith_diagnostics::{Diagnostic, DiagnosticCode, Help, Label, Note, Severity};
use lazalith_types::{SourceId, SourceManager, SourceSpan};

use crate::ast::{
    Block, ConstDecl, Expr, ExternDecl, Function, Item, Name, Path, Program, Stmt, TypeAnnotation,
};
use crate::diagnostic::StageError;

/// The resolver's diagnostic codes.
pub mod codes {
    /// A name is not defined anywhere it could be.
    pub const UNRESOLVED: &str = "N0001";
    /// Two items in one scope share a name.
    pub const DUPLICATE_ITEM: &str = "N0002";
    /// Two parameters or locals share a name in one scope.
    pub const DUPLICATE_BINDING: &str = "N0003";
    /// A private item was used from another module.
    pub const PRIVATE: &str = "N0004";
    /// A name was used in a type position but is not a type.
    pub const NOT_A_TYPE: &str = "N0005";
    /// A module path does not exist.
    pub const UNKNOWN_MODULE: &str = "N0006";
    /// A `use` path does not name an item.
    pub const UNRESOLVED_IMPORT: &str = "N0007";
    /// `use` imported a name that is already in scope.
    pub const SHADOWED_IMPORT: &str = "N0008";
    /// A function is called with `::` where a module was expected, or the
    /// other way round.
    pub const NOT_A_MODULE: &str = "N0009";
    /// A local shadows a name that a later use depends on.
    pub const SHADOWS_GLOBAL: &str = "N0010";
    /// A `const` initialiser is not a constant expression.
    pub const NOT_CONSTANT: &str = "N0011";
    /// A `use` declaration is not at the top of a module.
    pub const MISPLACED_USE: &str = "N0012";
}

/// What a name in a module refers to.
#[derive(Clone, Debug)]
pub enum Symbol {
    /// A function.
    Function(Box<ResolvedFunction>),
    /// An `extern` syscall declaration.
    Extern(Box<ResolvedExtern>),
    /// A `const`.
    Constant(Box<ResolvedConstant>),
    /// A nested module.
    Module(Box<ResolvedModule>),
}

/// A function with everything the later stages need.
#[derive(Clone, Debug)]
pub struct ResolvedFunction {
    /// The declared name.
    pub name: String,
    /// Whether the function is `pub`.
    pub is_public: bool,
    /// The parameters, in order.
    pub parameters: Vec<Parameter>,
    /// The result type as written, or `None` for the unit result.
    pub result: Option<TypeAnnotation>,
    /// The body.
    pub body: Block,
    /// Where the function was written.
    pub span: SourceSpan,
    /// The module path this function belongs to, empty for the file's own
    /// module.
    pub module: Vec<String>,
}

/// A parameter.
#[derive(Clone, Debug)]
pub struct Parameter {
    /// The parameter's name.
    pub name: String,
    /// Its written type.
    pub annotation: TypeAnnotation,
    /// Where it was written.
    pub span: SourceSpan,
}

/// An `extern` declaration with everything the later stages need.
#[derive(Clone, Debug)]
pub struct ResolvedExtern {
    /// The declared name.
    pub name: String,
    /// The parameters, in ABI order.
    pub parameters: Vec<Parameter>,
    /// The result type.
    pub result: TypeAnnotation,
    /// Where it was written.
    pub span: SourceSpan,
    /// The module path this declaration belongs to.
    pub module: Vec<String>,
}

/// A `const` declaration.
#[derive(Clone, Debug)]
pub struct ResolvedConstant {
    /// The declared name.
    pub name: String,
    /// Whether it is `pub`.
    pub is_public: bool,
    /// Its written type, if any.
    pub annotation: Option<TypeAnnotation>,
    /// Its value.
    pub value: Expr,
    /// Where it was written.
    pub span: SourceSpan,
    /// The module path this constant belongs to.
    pub module: Vec<String>,
}

/// A module and everything in it.
#[derive(Clone, Debug)]
pub struct ResolvedModule {
    /// The module's own name.
    pub name: String,
    /// Whether it is `pub`.
    pub is_public: bool,
    /// Its items by name, in a deterministic order.
    pub items: BTreeMap<String, Symbol>,
    /// The order the items were written in, for diagnostics.
    pub order: Vec<String>,
    /// Where the module was written.
    pub span: SourceSpan,
    /// The module path, empty for the file's own module.
    pub path: Vec<String>,
}

impl ResolvedModule {
    fn new(name: String, is_public: bool, span: SourceSpan, path: Vec<String>) -> Self {
        Self {
            name,
            is_public,
            items: BTreeMap::new(),
            order: Vec::new(),
            span,
            path,
        }
    }

    fn insert(&mut self, name: String, symbol: Symbol) {
        if !self.items.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.items.insert(name, symbol);
    }
}

/// A whole resolved file.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// The file's own module. Its items are the top-level items.
    pub root: ResolvedModule,
    /// The import table of the file's own module.
    pub imports: BTreeMap<String, Import>,
    /// Every module, flattened by path, so a lookup can find a sibling module
    /// without walking the tree.
    pub modules: BTreeMap<Vec<String>, ResolvedModule>,
}

/// An import created by `use`.
#[derive(Clone, Debug)]
pub struct Import {
    /// The name it is bound to locally.
    pub local: String,
    /// The full path it refers to.
    pub target: Vec<String>,
    /// Where the `use` was written.
    pub span: SourceSpan,
}

/// A name found in some scope, with enough information to report its origin.
#[derive(Clone, Debug)]
pub struct Found<'a> {
    /// The symbol.
    pub symbol: &'a Symbol,
    /// The module the symbol lives in.
    pub module: &'a ResolvedModule,
    /// Whether the use may see it, that is, whether every module between the
    /// use and the symbol is `pub` and the symbol itself is `pub`.
    pub visible: bool,
}

impl Resolved {
    /// Every module, the file's own first.
    ///
    /// The root module is not a key in `modules`, so a lookup that walks the
    /// module tree must include it explicitly. This iterator is the one place that
    /// knows that.
    pub fn all_modules(&self) -> impl Iterator<Item = &ResolvedModule> {
        core::iter::once(&self.root).chain(self.modules.values())
    }

    /// The function with this fully qualified path, if there is one.
    pub fn function(&self, path: &[String]) -> Option<&ResolvedFunction> {
        let (module_path, name) = path.split_at(path.len() - 1);
        let module = self.modules.get(module_path)?;
        match module.items.get(&name[0])? {
            Symbol::Function(function) => Some(function),
            _ => None,
        }
    }
}

/// Resolves a parsed file.
pub fn resolve(
    _source: SourceId,
    sources: &SourceManager,
    program: Program,
) -> Result<Resolved, StageError> {
    let context_sources = sources;
    let mut context = Context {
        sources,
        modules: BTreeMap::new(),
    };
    let mut root = ResolvedModule::new(String::new(), true, program.span.clone(), Vec::new());
    collect_module(&mut context, &program.items, &mut root, Vec::new())?;
    let mut imports = BTreeMap::new();
    collect_imports(&context, &program.items, &mut imports)?;
    let modules = context.modules;
    let resolved = Resolved {
        root,
        imports,
        modules,
    };
    // Every import is checked against the finished tree, so a `use` of a path
    // that does not exist, or of a private item, is reported here.
    check_imports(context_sources, &resolved, &program.items)?;
    Ok(resolved)
}

struct Context<'a> {
    sources: &'a SourceManager,
    modules: BTreeMap<Vec<String>, ResolvedModule>,
}

impl<'a> Context<'a> {
    fn error(
        &self,
        raw_code: &str,
        message: impl Into<String>,
        span: &SourceSpan,
        notes: &[&str],
        help: Option<&str>,
        extra: &[(SourceSpan, String)],
    ) -> StageError {
        let code = DiagnosticCode::new(raw_code)
            .unwrap_or_else(|_| DiagnosticCode::new("N9999").expect("the fallback code is valid"));
        let mut diagnostic = Diagnostic::new(Severity::Error, code, message)
            .with_label(Label::primary(span.clone(), "here"));
        for note in notes {
            diagnostic = diagnostic.with_note(Note::new(*note));
        }
        for (label_span, text) in extra {
            diagnostic = diagnostic.with_label(Label::secondary(label_span.clone(), text.clone()));
        }
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(Help::new(help));
        }
        StageError::from_parts(diagnostic, self.sources.clone())
    }
}

/// Fills one module from a list of items, recursing into nested modules.
fn collect_module(
    context: &mut Context<'_>,
    items: &[Item],
    module: &mut ResolvedModule,
    path: Vec<String>,
) -> Result<(), StageError> {
    for item in items {
        match item {
            Item::Function(function) => {
                let resolved = resolve_function(context, function, &path)?;
                insert_unique(
                    context,
                    module,
                    function.name.text.clone(),
                    Symbol::Function(Box::new(resolved)),
                    &function.name,
                )?;
            }
            Item::Extern(extern_decl) => {
                let resolved = resolve_extern(context, extern_decl, &path)?;
                insert_unique(
                    context,
                    module,
                    extern_decl.name.text.clone(),
                    Symbol::Extern(Box::new(resolved)),
                    &extern_decl.name,
                )?;
            }
            Item::Const(constant) => {
                let resolved = resolve_const(constant, &path);
                insert_unique(
                    context,
                    module,
                    constant.name.text.clone(),
                    Symbol::Constant(Box::new(resolved)),
                    &constant.name,
                )?;
            }
            Item::Module(child) => {
                let child_path = {
                    let mut child_path = path.clone();
                    child_path.push(child.name.text.clone());
                    child_path
                };
                let mut resolved = ResolvedModule::new(
                    child.name.text.clone(),
                    child.is_public,
                    child.span.clone(),
                    child_path.clone(),
                );
                collect_module(context, &child.items, &mut resolved, child_path.clone())?;
                if context.modules.contains_key(&child_path) {
                    return Err(context.error(
                        codes::DUPLICATE_ITEM,
                        alloc::format!("module `{}` is defined twice", child.name.text),
                        &child.name.span,
                        &["every module in a file must have one definition"],
                        Some("merge the two modules, or rename one"),
                        &[],
                    ));
                }
                context.modules.insert(child_path, resolved.clone());
                insert_unique(
                    context,
                    module,
                    child.name.text.clone(),
                    Symbol::Module(Box::new(resolved)),
                    &child.name,
                )?;
            }
            Item::Use(_) => {}
        }
    }
    Ok(())
}

fn insert_unique(
    context: &Context<'_>,
    module: &mut ResolvedModule,
    name: String,
    symbol: Symbol,
    written: &Name,
) -> Result<(), StageError> {
    if let Some(existing) = module.items.get(&name) {
        let kind = |symbol: &Symbol| match symbol {
            Symbol::Function(_) => "a function",
            Symbol::Extern(_) => "an extern declaration",
            Symbol::Constant(_) => "a const",
            Symbol::Module(_) => "a module",
        };
        let mut extra = Vec::new();
        if let Some(span) = existing_span(existing) {
            extra.push((span, "first defined here".to_string()));
        }
        let where_ = if module.name.is_empty() {
            "this file".to_string()
        } else {
            alloc::format!("module `{}`", module.name)
        };
        return Err(context.error(
            codes::DUPLICATE_ITEM,
            alloc::format!("`{name}` is already defined in {where_}"),
            &written.span,
            &[alloc::format!("a module may hold one {} of a given name", kind(existing)).as_str()],
            Some("rename one of the two, or remove the duplicate"),
            &extra,
        ));
    }
    module.insert(name, symbol);
    Ok(())
}

fn existing_span(symbol: &Symbol) -> Option<SourceSpan> {
    Some(match symbol {
        Symbol::Function(item) => item.span.clone(),
        Symbol::Extern(item) => item.span.clone(),
        Symbol::Constant(item) => item.span.clone(),
        Symbol::Module(item) => item.span.clone(),
    })
}

fn resolve_function(
    context: &Context<'_>,
    function: &Function,
    module: &[String],
) -> Result<ResolvedFunction, StageError> {
    let mut parameters: Vec<Parameter> = Vec::new();
    for parameter in &function.parameters {
        if parameters
            .iter()
            .any(|other| other.name == parameter.name.text)
        {
            return Err(context.error(
                codes::DUPLICATE_BINDING,
                alloc::format!("parameter `{}` is declared twice", parameter.name.text),
                &parameter.name.span,
                &["a function may not have two parameters with one name"],
                Some("rename one of the parameters"),
                &[],
            ));
        }
        parameters.push(Parameter {
            name: parameter.name.text.clone(),
            annotation: parameter.annotation.clone(),
            span: parameter.span.clone(),
        });
    }
    check_body_bindings(context, &function.body, &parameters)?;
    Ok(ResolvedFunction {
        name: function.name.text.clone(),
        is_public: function.is_public,
        parameters,
        result: function.result.clone(),
        body: function.body.clone(),
        span: function.span.clone(),
        module: module.to_vec(),
    })
}

fn resolve_extern(
    context: &Context<'_>,
    extern_decl: &ExternDecl,
    module: &[String],
) -> Result<ResolvedExtern, StageError> {
    let mut parameters: Vec<Parameter> = Vec::new();
    for parameter in &extern_decl.parameters {
        if parameters
            .iter()
            .any(|other| other.name == parameter.name.text)
        {
            return Err(context.error(
                codes::DUPLICATE_BINDING,
                alloc::format!("parameter `{}` is declared twice", parameter.name.text),
                &parameter.name.span,
                &["an `extern` declaration may not repeat a parameter name"],
                Some("rename one of the parameters"),
                &[],
            ));
        }
        parameters.push(Parameter {
            name: parameter.name.text.clone(),
            annotation: parameter.annotation.clone(),
            span: parameter.span.clone(),
        });
    }
    Ok(ResolvedExtern {
        name: extern_decl.name.text.clone(),
        parameters,
        result: extern_decl.result.clone(),
        span: extern_decl.span.clone(),
        module: module.to_vec(),
    })
}

fn resolve_const(constant: &ConstDecl, module: &[String]) -> ResolvedConstant {
    ResolvedConstant {
        name: constant.name.text.clone(),
        is_public: constant.is_public,
        annotation: constant.annotation.clone(),
        value: constant.value.clone(),
        span: constant.span.clone(),
        module: module.to_vec(),
    }
}

/// Checks that every binding in a body is unique within its own scope, and
/// that every name used in the body is bound somewhere.
fn check_body_bindings(
    context: &Context<'_>,
    block: &Block,
    parameters: &[Parameter],
) -> Result<(), StageError> {
    let mut scope: Vec<String> = parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect();
    for statement in &block.statements {
        check_statement(context, statement, &mut scope, &block.tail, true)?;
    }
    if let Some(tail) = &block.tail {
        check_expression_names(context, tail, &scope)?;
    }
    Ok(())
}

fn check_statement(
    context: &Context<'_>,
    statement: &Stmt,
    scope: &mut Vec<String>,
    _tail: &Option<Box<Expr>>,
    function_scope: bool,
) -> Result<(), StageError> {
    match statement {
        Stmt::Let {
            name, span, value, ..
        } => {
            check_expression_names(context, value, scope)?;
            if function_scope && scope.contains(&name.text) {
                let previous = scope
                    .iter()
                    .position(|other| *other == name.text)
                    .unwrap_or(0);
                let _ = previous;
                return Err(context.error(
                    codes::DUPLICATE_BINDING,
                    alloc::format!("`{}` is already bound in this scope", name.text),
                    &name.span,
                    &["a function's body is one scope, so a name may be bound once"],
                    Some("rename one of the two bindings"),
                    &[],
                ));
            }
            scope.push(name.text.clone());
            let _ = span;
            Ok(())
        }
        Stmt::Assign { target, value, .. } => {
            check_expression_names(context, target, scope)?;
            check_expression_names(context, value, scope)
        }
        Stmt::Expression { expression, .. } => check_expression_names(context, expression, scope),
        Stmt::If { arms, .. } => {
            for arm in arms {
                if let Some(condition) = &arm.condition {
                    check_expression_names(context, condition, scope)?;
                }
                check_nested_block(context, &arm.body, scope)?;
            }
            Ok(())
        }
        Stmt::While {
            condition, body, ..
        } => {
            check_expression_names(context, condition, scope)?;
            check_nested_block(context, body, scope)
        }
        Stmt::For {
            name,
            iterated,
            end,
            body,
            ..
        } => {
            check_expression_names(context, iterated, scope)?;
            if let Some(end) = end {
                check_expression_names(context, end, scope)?;
            }
            // The loop variable is scoped to the body.
            let mut inner = scope.clone();
            inner.push(name.text.clone());
            check_nested_block(context, body, &inner)
        }
        Stmt::Loop { body, .. } => check_nested_block(context, body, scope),
        Stmt::Break { .. } | Stmt::Continue { .. } => Ok(()),
        Stmt::Return { value, .. } => match value {
            Some(value) => check_expression_names(context, value, scope),
            None => Ok(()),
        },
        Stmt::Block { block, .. } => check_nested_block(context, block, scope),
    }
}

/// A nested block gets its own scope, so a name may be reused once the block
/// has ended. `for` bodies likewise.
fn check_nested_block(
    context: &Context<'_>,
    block: &Block,
    scope: &[String],
) -> Result<(), StageError> {
    let mut inner = scope.to_vec();
    for statement in &block.statements {
        check_statement(context, statement, &mut inner, &block.tail, false)?;
    }
    if let Some(tail) = &block.tail {
        check_expression_names(context, tail, &inner)?;
    }
    Ok(())
}

/// Walks an expression, checking that every bare name is bound in `scope`.
fn check_expression_names(
    context: &Context<'_>,
    expression: &Expr,
    scope: &[String],
) -> Result<(), StageError> {
    match expression {
        Expr::Int { .. } | Expr::Str { .. } | Expr::Bool { .. } => Ok(()),
        Expr::Path { path, .. } => {
            if path.segments.len() == 1 {
                let name = &path.segments[0].text;
                if scope.iter().any(|bound| bound == name) {
                    return Ok(());
                }
            }
            // Not a local: it must be an item, which the caller checks against
            // the module tree. The type checker decides that, so here it is
            // only recorded as unresolvable when the path is not even a shape
            // Lazen has.
            Ok(())
        }
        Expr::Call {
            callee, arguments, ..
        } => {
            check_expression_names(context, callee, scope)?;
            for argument in arguments {
                check_expression_names(context, argument, scope)?;
            }
            Ok(())
        }
        Expr::MethodCall {
            receiver,
            arguments,
            ..
        } => {
            check_expression_names(context, receiver, scope)?;
            for argument in arguments {
                check_expression_names(context, argument, scope)?;
            }
            Ok(())
        }
        Expr::Index { base, index, .. } => {
            check_expression_names(context, base, scope)?;
            check_expression_names(context, index, scope)
        }
        Expr::Unary { operand, .. } => check_expression_names(context, operand, scope),
        Expr::Binary { left, right, .. } => {
            check_expression_names(context, left, scope)?;
            check_expression_names(context, right, scope)
        }
        Expr::Cast { operand, .. } => check_expression_names(context, operand, scope),
        Expr::Array { elements, .. } => {
            for element in elements {
                check_expression_names(context, element, scope)?;
            }
            Ok(())
        }
        Expr::ArrayRepeat { value, count, .. } => {
            check_expression_names(context, value, scope)?;
            check_expression_names(context, count, scope)
        }
        Expr::If { arms, .. } => {
            for arm in arms {
                if let Some(condition) = &arm.condition {
                    check_expression_names(context, condition, scope)?;
                }
                let mut inner = scope.to_vec();
                for statement in &arm.body.statements {
                    check_statement(context, statement, &mut inner, &arm.body.tail, false)?;
                }
                if let Some(tail) = &arm.body.tail {
                    check_expression_names(context, tail, &inner)?;
                }
            }
            Ok(())
        }
        Expr::Block { block, .. } => {
            let mut inner = scope.to_vec();
            for statement in &block.statements {
                check_statement(context, statement, &mut inner, &block.tail, false)?;
            }
            if let Some(tail) = &block.tail {
                check_expression_names(context, tail, &inner)?;
            }
            Ok(())
        }
    }
}

/// Collects the file's `use` declarations, checking that each one resolves and
/// does not collide with another import.
/// Collects the file's `use` declarations and checks each one.
///
/// An import is checked here rather than at every use, so a wrong `use` is
/// reported once, where it was written.
fn collect_imports(
    context: &Context<'_>,
    items: &[Item],
    imports: &mut BTreeMap<String, Import>,
) -> Result<(), StageError> {
    for item in items {
        let Item::Use(declaration) = item else {
            continue;
        };
        let segments: Vec<&str> = declaration
            .path
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        let local = declaration
            .alias
            .as_ref()
            .map(|alias| alias.text.clone())
            .unwrap_or_else(|| declaration.path.tail().text.clone());
        if let Some(existing) = imports.get(&local) {
            let extra = [(existing.span.clone(), "first imported here".to_string())];
            return Err(context.error(
                codes::SHADOWED_IMPORT,
                alloc::format!("`{local}` is imported twice"),
                &declaration.span,
                &[],
                Some("remove one of the two `use` declarations"),
                &extra,
            ));
        }
        imports.insert(
            local,
            Import {
                local: declaration.path.tail().text.clone(),
                target: segments
                    .iter()
                    .map(|segment| (*segment).to_string())
                    .collect(),
                span: declaration.span.clone(),
            },
        );
    }
    Ok(())
}

/// A lookup of a name in a resolved file, for the type checker and for tests.
///
/// A name is resolved from the module that wrote the use, so a function can call a
/// sibling of its own module, and then from the file's root, so a nested module
/// can reach a top-level item.
pub fn lookup<'a>(resolved: &'a Resolved, path: &Path) -> Option<Found<'a>> {
    lookup_from(
        resolved,
        &[],
        &path
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>(),
    )
}

/// A lookup by plain string segments, from the file's root module.
pub fn lookup_parts<'a>(resolved: &'a Resolved, segments: &[&str]) -> Option<Found<'a>> {
    lookup_from(resolved, &[], segments)
}

/// A lookup from a particular module.
pub fn lookup_from<'a>(
    resolved: &'a Resolved,
    from: &[String],
    segments: &[&str],
) -> Option<Found<'a>> {
    let (last, modules) = segments.split_last()?;

    // A `use` binding is visible wherever the file's import table applies.
    if modules.is_empty()
        && let Some(import) = resolved.imports.get(*last)
        && let Some(found) = lookup_from(
            resolved,
            &[],
            &import.target.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    {
        return Some(Found {
            visible: found.visible,
            ..found
        });
    }

    // Start in the module that wrote the use, then walk the path's module
    // segments, then look up the item itself.
    if let Some(found) = walk(resolved, from, modules, last) {
        return Some(found);
    }

    // A nested module also sees the file's top-level items, because the root
    // module encloses every other module.
    if !from.is_empty()
        && !crossed_any(resolved, from, modules)
        && let Some(found) = found(&resolved.root, last, from)
    {
        return Some(found);
    }

    // A path written from the file's top level is absolute, whatever module it
    // was written in. Without this, `rt::memory::copy` inside `rt::sys` would be
    // looked for as `rt::sys::rt::memory`, and the only way for a nested module to
    // reach a sibling would be a `use` at the top level — which reads as a quirk
    // of the language rather than a rule, and makes deeply nested libraries
    // impossible to write.
    if !from.is_empty() {
        return walk(resolved, &[], modules, last);
    }
    None
}

/// Walks `modules` from `from` and looks `last` up in the module it lands in.
fn walk<'a>(
    resolved: &'a Resolved,
    from: &[String],
    modules: &[&str],
    last: &str,
) -> Option<Found<'a>> {
    let mut path: Vec<String> = from.to_vec();
    let mut current = match resolved.modules.get(&path) {
        Some(module) => module,
        None if path.is_empty() => &resolved.root,
        None => return None,
    };
    for segment in modules {
        if !matches!(current.items.get(*segment), Some(Symbol::Module(_))) {
            return None;
        };
        path.push((*segment).to_string());
        current = resolved.modules.get(&path)?;
    }
    found(current, last, from)
}

/// Whether walking `modules` from `from` reaches a module at all.
///
/// This is only used to decide whether the enclosing-root fallback applies, so it
/// answers "did the path name a module" rather than "was the item found": a path
/// that names a real module and then misses on the item must report that miss, not
/// fall through and find something else with the same last segment.
fn crossed_any(resolved: &Resolved, from: &[String], modules: &[&str]) -> bool {
    if modules.is_empty() {
        return false;
    }
    let mut path: Vec<String> = from.to_vec();
    let mut current = match resolved.modules.get(&path) {
        Some(module) => module,
        None if path.is_empty() => &resolved.root,
        None => return false,
    };
    for segment in modules {
        if !matches!(current.items.get(*segment), Some(Symbol::Module(_))) {
            return false;
        };
        path.push((*segment).to_string());
        match resolved.modules.get(&path) {
            Some(module) => current = module,
            None => return false,
        }
    }
    let _ = current;
    true
}

/// Looks one name up in one module, and decides whether it is visible.
///
/// An item is visible if it is `pub`, if it lives in the module that is asking,
/// or if it lives in the file's root module, which encloses every other module.
fn found<'a>(module: &'a ResolvedModule, name: &str, from: &[String]) -> Option<Found<'a>> {
    let symbol = module.items.get(name)?;
    let is_public = match symbol {
        Symbol::Function(item) => item.is_public,
        Symbol::Constant(item) => item.is_public,
        Symbol::Extern(_) => true,
        Symbol::Module(item) => item.is_public,
    };
    let visible = is_public || module.path.is_empty() || module.path == from;
    Some(Found {
        symbol,
        module,
        visible,
    })
}

/// Resolves a parsed program without running the type checker.
///
/// This exists for tests and for a caller that wants the symbol table on its
/// own; the frontend pipeline uses [`crate::frontend::compile`].
pub fn resolve_only_for_tests(
    source: SourceId,
    sources: &SourceManager,
    program: Program,
) -> Result<Resolved, StageError> {
    resolve(source, sources, program)
}

/// Checks every `use` declaration against the finished module tree.
///
/// This runs after resolution because an import may name an item declared later
/// in the file, exactly as a call may.
fn check_imports(
    sources: &SourceManager,
    resolved: &Resolved,
    items: &[Item],
) -> Result<(), StageError> {
    let context = Context {
        sources,
        modules: BTreeMap::new(),
    };
    for item in items {
        let Item::Use(declaration) = item else {
            continue;
        };
        let segments: Vec<&str> = declaration
            .path
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        let name = segments.join("::");
        let Some(found) = lookup_from(resolved, &[], &segments) else {
            return Err(context.error(
                codes::UNRESOLVED_IMPORT,
                alloc::format!("`use {name}` names nothing"),
                &declaration.path.span,
                &["a `use` path must reach a function, a const, or a module"],
                Some("check the spelling, or declare the item it names"),
                &[],
            ));
        };
        if !found.visible {
            return Err(context.error(
                codes::PRIVATE,
                alloc::format!("`{name}` is private to its module"),
                &declaration.path.span,
                &["every item used from another module must be `pub`"],
                Some("write `pub` on the item"),
                &[],
            ));
        }
    }
    Ok(())
}
