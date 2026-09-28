//! Type checking: what a program means, and whether it can mean it.
//!
//! # This is the stage that decides C's conversions
//!
//! C's arithmetic is written in a small number of rules, and every one of them
//! is here rather than in the IR emitter, because the emitter must not have an
//! opinion: it should be told `int + int -> int` and do that.
//!
//! - **Integer promotion.** Anything narrower than `int` — `char`, `short`,
//!   `_Bool` — becomes `int` before an arithmetic operator. This is why
//!   `c + 1` cannot overflow where `c` cannot hold `257`, and it is why
//!   `-c` on an unsigned `char` is a *negative* `int`.
//! - **Usual arithmetic conversions.** Two operands are brought to a common
//!   type: unsigned wins over signed at the same width, and the wider one wins
//!   otherwise, so a `long` and an `unsigned int` are both `long`. C's rule for
//!   equal-width signedness depends on the *rank*, not the width, and this
//!   machine's ranks are the sizes.
//! - **Decay.** An array used as a value becomes a pointer to its first element
//!   and a function becomes a pointer to itself. In exactly three places it
//!   does *not* happen — `sizeof`, `&`, and a string literal initialising a
//!   `char` array — and [`CType::decayed`] is written so those three call sites
//!   and no others use it.
//! - **Assignment conversion.** Assignment converts to the *target's* type, and
//!   the conversion happens at the assignment. A narrowing conversion to a
//!   signed type whose value does not fit is reported, because it is a value
//!   the program cannot represent, not a truncation it asked for.
//!
//! # What this stage refuses
//!
//! - **Floating point.** There is none, so there is nothing to check.
//! - **A `struct` or `union` larger than one word passed or returned by value.**
//!   The machine's ABI has no multiword return and no aggregate argument
//!   passing. The diagnostic names both halves: the C construct and the machine
//!   limit, so the reader knows to pass a pointer instead of guessing.
//! - **A variadic function *definition*.** A variadic *declaration* is accepted,
//!   because `printf` has to be callable, and a definition is refused because
//!   there is nowhere in the frame layout to put the extra arguments. A
//!   variadic function's body cannot know how many arguments it was given.
//! - **A `goto` into a block with a local that is not initialised.** Nothing
//!   enforces this in C, and this machine has no way to.

use alloc::collections::BTreeMap;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use lazalith_types::{SourceId, SourceManager, SourceSpan};

use crate::ast::*;
use crate::ctypes::{self, CType, EnumType, Enumerator, Field, FunctionType, RecordType, align_up};
use crate::diagnostic::StageError;
use crate::resolve::Resolved;

/// C's diagnostic codes for this stage.
pub mod codes {
    /// An expression's type is not one the operator accepts.
    pub const WRONG_OPERAND: &str = "C0401";
    /// An expression cannot have the type it has.
    pub const MISMATCH: &str = "C0402";
    /// A name that is not declared.
    pub const UNDECLARED: &str = "C0403";
    /// A value that does not fit its type.
    pub const OUT_OF_RANGE: &str = "C0404";
    /// An operand that must be assignable and is not.
    pub const NOT_ASSIGNABLE: &str = "C0405";
    /// An operand that must be an lvalue and is not.
    pub const NOT_LVALUE: &str = "C0406";
    /// A type this machine cannot represent.
    pub const UNSUPPORTED: &str = "C0407";
    /// A member that the record does not have.
    pub const NO_SUCH_MEMBER: &str = "C0408";
    /// A value where a type is required, or a type where a value is required.
    pub const NOT_A_TYPE: &str = "C0409";
    /// A `_Static_assert` whose condition is false.
    pub const STATIC_ASSERT: &str = "C0410";
    /// A `return` that does not match the function's return type.
    pub const BAD_RETURN: &str = "C0411";
    /// A call with the wrong number of arguments.
    pub const ARITY: &str = "C0412";
    /// A `break` or `continue` in the wrong place, or a `case` value of the
    /// wrong type.
    pub const BAD_CASE: &str = "C0413";
    /// A member of an incomplete type.
    pub const INCOMPLETE: &str = "C0414";
    /// An initialiser that does not fit its type.
    pub const BAD_INITIALISER: &str = "C0415";
    /// Two definitions of the same function.
    pub const REDEFINED: &str = "C0416";
    /// An integer constant that does not fit any type.
    pub const CONSTANT_TOO_LARGE: &str = "C0417";
}

/// A function after checking.
#[derive(Clone, Debug)]
pub struct CheckedFunction {
    /// The function's name.
    pub name: String,
    /// Its type, with the parameter list adjusted the way C adjusts it.
    pub ty: CType,
    /// Its parameters' names, in order, and whether each one has a name at all.
    pub parameters: Vec<(Option<String>, CType)>,
    /// The body.
    pub body: Block,
    /// Every block-scope local, in declaration order, with its type.
    ///
    /// The emitter needs a slot for each one and a slot needs a size, and the
    /// type checker is the only stage that has both the declared type and the
    /// declarator it came from. It is recorded here rather than rebuilt by the
    /// emitter, because a second copy of the declarator fold is a second thing
    /// to keep in step with the first.
    pub locals: Vec<(String, CType)>,
    /// Where the whole function is.
    pub span: SourceSpan,
}

/// A variable after checking.
#[derive(Clone, Debug)]
pub struct CheckedVariable {
    /// Its name.
    pub name: String,
    /// Its type.
    pub ty: CType,
    /// Its initialiser, when one was written.
    pub initial: Option<Initializer>,
    /// Where it is.
    pub span: SourceSpan,
    /// Whether it is a file-scope name, which has static storage.
    pub file_scope: bool,
}

/// A whole program after checking.
#[derive(Clone, Debug)]
pub struct CheckedCProgram {
    /// The functions, in declaration order.
    pub functions: Vec<CheckedFunction>,
    /// The file-scope variables, in declaration order.
    pub globals: Vec<CheckedVariable>,
    /// The `typedef` names and their types.
    pub typedefs: BTreeMap<String, CType>,
    /// The tags and their types, so a later stage can lay them out.
    pub tags: BTreeMap<String, CType>,
    /// Every function's type, keyed by C name.
    ///
    /// A body may call a function defined below it, and a prototype has no body at
    /// all, so the emitter cannot find a type by walking back to a definition. It
    /// is recorded here, where every front-end stage can put it.
    pub function_types: BTreeMap<String, CType>,
    /// Every cast's target type, keyed by where the cast starts.
    ///
    /// A cast is the one expression whose type the emitter cannot rebuild: `(T)` names
    /// a *type*, and building one needs the typedef table and the tag table, which
    /// are the checker's. A cast that is dropped is not a missed optimisation —
    /// `(unsigned char)` on a signed load is a different value — so the type is
    /// recorded where it is known and read back by byte offset.
    pub cast_types: BTreeMap<u32, CType>,

    /// Every `sizeof`'s answer in bytes, keyed by where the expression starts.
    ///
    /// A `sizeof` operand names a *type*, and the emitter cannot build one — it
    /// would need this stage's typedef and tag tables. The answer is also not
    /// recoverable from the operand the emitter lowers, because `sizeof` never
    /// decays: `sizeof buffer` is a `char[16]` and `sizeof (buffer + 0)` is a
    /// pointer, and the difference is only visible here.
    pub sizeofs: BTreeMap<u32, u32>,
    /// The function the runtime starts at, which is `main`.
    pub entry: String,
    /// Every failure.
    pub diagnostics: Vec<StageError>,
}

/// Checks a resolved translation unit.
pub fn check(source: SourceId, sources: &SourceManager, resolved: &Resolved) -> CheckedCProgram {
    let mut checker = Checker {
        source,
        sources,
        resolved,
        scopes: vec![BTreeMap::new()],
        typedefs: BTreeMap::new(),
        tags: BTreeMap::new(),
        function_types: BTreeMap::new(),
        cast_types: BTreeMap::new(),
        sizeofs: BTreeMap::new(),
        program_globals: Vec::new(),
        functions: Vec::new(),
        globals: Vec::new(),
        locals: Vec::new(),
        errors: Vec::new(),
        return_type: None,
        current_function: String::new(),
        loop_depth: 0,
        switch_depth: 0,
    };
    checker.program();
    let mut checker = checker;
    CheckedCProgram {
        functions: core::mem::take(&mut checker.functions),
        globals: core::mem::take(&mut checker.globals),
        typedefs: core::mem::take(&mut checker.typedefs),
        function_types: core::mem::take(&mut checker.function_types),
        cast_types: core::mem::take(&mut checker.cast_types),
        sizeofs: core::mem::take(&mut checker.sizeofs),
        tags: core::mem::take(&mut checker.tags),
        entry: String::from("main"),
        diagnostics: core::mem::take(&mut checker.errors),
    }
}

struct Checker<'a> {
    source: SourceId,
    sources: &'a SourceManager,
    resolved: &'a Resolved,
    scopes: Vec<BTreeMap<String, CType>>,
    typedefs: BTreeMap<String, CType>,
    tags: BTreeMap<String, CType>,
    /// Every function's type, gathered before any body is checked.
    ///
    /// A function may be called before the line that defines it, and a prototype
    /// has no line at all, so a body cannot look one up where it found it. This
    /// is the table both look it up in.
    function_types: BTreeMap<String, CType>,
    /// Every file-scope object's type, for the same reason.
    /// Every cast's target type, keyed by the cast's start offset.
    cast_types: BTreeMap<u32, CType>,
    sizeofs: BTreeMap<u32, u32>,
    program_globals: Vec<(String, CType)>,
    functions: Vec<CheckedFunction>,
    globals: Vec<CheckedVariable>,
    locals: Vec<(String, CType)>,
    errors: Vec<StageError>,
    return_type: Option<CType>,
    current_function: String,
    loop_depth: u32,
    switch_depth: u32,
}

impl<'a> Checker<'a> {
    fn program(&mut self) {
        // Every function's type is collected *before* any body is checked, and
        // for two reasons. A function may be called before the line that defines
        // it, which is legal C and which a single pass cannot see; and a
        // prototype with no body has to be callable too, because `printf` is
        // exactly that. Both need the type here rather than at the point of
        // definition, and neither has a body to find it in.
        for declaration in &self.resolved.unit.declarations {
            if let Declaration::Declaration(var) = declaration {
                self.collect_typedefs(var);
            }
        }
        for declaration in &self.resolved.unit.declarations {
            match declaration {
                Declaration::Declaration(var) => {
                    self.collect_function_types(var);
                    self.collect_global_types(var);
                }
                Declaration::Function(definition) => {
                    let base = self.specifier_type(&definition.base);
                    let ty = self.declarator_type(&base, &definition.declarator);
                    self.function_types
                        .insert(definition.name.clone(), ty.clone());
                    if self
                        .functions
                        .iter()
                        .any(|existing| existing.name == definition.name)
                    {
                        // Reported by `function`, which has the span; recording it
                        // here as well would report it twice.
                    }
                }
                _ => {}
            }
        }
        let declarations: Vec<Declaration> = self.resolved.unit.declarations.clone();
        for declaration in &declarations {
            match declaration {
                Declaration::Function(definition) => self.function(definition),
                Declaration::Declaration(var) => self.global(var),
                Declaration::Record(definition) => self.record(definition),
                Declaration::Enum(definition) => self.enumeration(definition),
                Declaration::StaticAssert(assertion) => self.static_assertion(assertion),
            }
        }
    }

    /// Records the type of every file-scope object a declaration names.
    ///
    /// A global's type is needed by every body that reads it, and a body is checked
    /// before the global is *laid out*, so the type is collected here rather
    /// than where the object is.
    fn collect_global_types(&mut self, var: &VarDecl) {
        if var.typedef {
            return;
        }
        let base = self.specifier_type(&var.base);
        for declarator in &var.declarators {
            let Some(name) = &declarator.name else {
                continue;
            };
            if declarator
                .derivation
                .last()
                .is_some_and(|derivation| matches!(derivation, Derivation::Function(_, _, _)))
            {
                continue;
            }
            let ty = self.declarator_type(&base, declarator);
            self.program_globals.push((name.clone(), ty));
        }
    }

    /// Records the type of every function a *declaration* names.
    ///
    /// A declaration whose declarator is a function type is a prototype, and a
    /// prototype is how a C program reaches a function defined in another
    /// translation unit. It has no body, so it is never lowered; recording its
    /// type here is what makes a call to it checkable.
    fn collect_function_types(&mut self, var: &VarDecl) {
        if var.typedef {
            return;
        }
        let base = self.specifier_type(&var.base);
        for declarator in &var.declarators {
            let Some(name) = &declarator.name else {
                continue;
            };
            if !declarator
                .derivation
                .last()
                .is_some_and(|derivation| matches!(derivation, Derivation::Function(_, _, _)))
            {
                continue;
            }
            let ty = self.declarator_type(&base, declarator);
            self.function_types.insert(name.clone(), ty);
        }
    }

    /// Records a refusal at a span.
    ///
    /// The help is required rather than optional: every refusal here names a C
    /// construct *and* the machine limit behind it, and a refusal that does not is
    /// not something a reader can act on.
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

    // -- types from specifiers --

    /// The type a specifier names.
    fn specifier_type(&mut self, specifier: &TypeSpecifier) -> CType {
        match &specifier.base {
            BaseType::Void => CType::Void,
            BaseType::Bool => CType::Bool,
            BaseType::Char { signed } => CType::Int {
                bits: 8,
                // The signedness the parser recorded, which is the written one.
                // A plain `char` is signed on this target, and `unsigned char` is
                // not — and it has to be honoured, because every `strcmp` in the C
                // standard library casts both operands to `unsigned char` before
                // comparing them, precisely so that a byte above 127 compares
                // *above* `'a'` and not below it.
                signed: *signed,
            },
            BaseType::Short { unsigned } => CType::Int {
                bits: 16,
                signed: !*unsigned,
            },
            BaseType::Int { unsigned } => CType::Int {
                bits: 32,
                signed: !*unsigned,
            },
            BaseType::Long {
                doubled: _,
                unsigned,
            } => CType::Int {
                bits: 64,
                signed: !*unsigned,
            },
            BaseType::Record(reference) => self.record_type(reference),
            BaseType::Enum(reference) => self.enum_type(reference),
            BaseType::Named(name) => self.typedefs.get(name).cloned().unwrap_or(CType::int()),
            BaseType::Parenthesised(inner) => self.specifier_type(inner),
        }
    }

    fn record_type(&mut self, reference: &RecordReference) -> CType {
        let key = reference.tag.clone().unwrap_or_default();
        if let Some(definition) = &reference.definition {
            let ty = self.build_record(definition, &key);
            self.tags.insert(key, ty.clone());
            return ty;
        }
        match self.tags.get(&key) {
            Some(ty) => ty.clone(),
            None => CType::int(),
        }
    }

    fn build_record(&mut self, definition: &RecordDefinition, key: &str) -> CType {
        // The record is inserted *before* its members are laid out, so a member
        // whose type is a pointer to this very record resolves. C requires
        // `struct S *next;` to work inside `struct S`, and this is how.
        let placeholder = CType::Struct(Box::new(RecordType {
            tag: definition.tag.clone(),
            union: definition.union,
            fields: Vec::new(),
            complete: false,
        }));
        let ty = if definition.union {
            CType::Union(Box::new(match &placeholder {
                CType::Union(record) => (**record).clone(),
                _ => unreachable!("the placeholder is the union it was built as"),
            }))
        } else {
            placeholder
        };
        if !key.is_empty() {
            self.tags.insert(key.to_string(), ty.clone());
        }
        let members = definition.members.clone().unwrap_or_default();
        let mut fields: Vec<Field> = Vec::new();
        let mut cursor = 0u32;
        let mut widest = 1u32;
        for member in &members {
            let base = self.specifier_type(&member.base);
            for declarator in &member.declarators {
                let member_ty = self.declarator_type(&base, declarator);
                let Some(size) = member_ty.size_in_bytes() else {
                    self.error(
                        declarator.span.clone(),
                        codes::INCOMPLETE,
                        alloc::format!(
                            "the member `{}` has no size",
                            declarator.name.clone().unwrap_or_default()
                        ),
                        "a member must have a complete type",
                    );
                    continue;
                };
                let alignment = member_ty.alignment_in_bytes();
                widest = widest.max(alignment);
                if definition.union {
                    fields.push(Field {
                        name: declarator.name.clone().unwrap_or_default(),
                        ty: member_ty,
                        offset: 0,
                        size,
                    });
                    cursor = cursor.max(size);
                } else {
                    let offset = align_up(cursor, alignment);
                    fields.push(Field {
                        name: declarator.name.clone().unwrap_or_default(),
                        ty: member_ty,
                        offset,
                        size,
                    });
                    cursor = offset.saturating_add(size);
                }
            }
            if member.declarators.is_empty() {
                // An anonymous member: its own size and alignment, laid out
                // where the next member would go.
                if let Some(size) = base.size_in_bytes() {
                    widest = widest.max(base.alignment_in_bytes());
                    let offset = align_up(cursor, base.alignment_in_bytes());
                    fields.push(Field {
                        name: String::new(),
                        ty: base,
                        offset,
                        size,
                    });
                    cursor = offset.saturating_add(size);
                }
            }
        }
        let record = if definition.union {
            CType::Union(Box::new(RecordType {
                tag: definition.tag.clone(),
                union: true,
                fields,
                complete: true,
            }))
        } else {
            CType::Struct(Box::new(RecordType {
                tag: definition.tag.clone(),
                union: false,
                fields,
                complete: true,
            }))
        };
        // The alignment is recomputed from the members, which is the only way a
        // record's own alignment is knowable.
        let _ = widest;
        if !key.is_empty() {
            self.tags.insert(key.to_string(), record.clone());
        }
        record
    }

    fn enum_type(&mut self, reference: &EnumReference) -> CType {
        let key = reference.tag.clone().unwrap_or_default();
        if let Some(definition) = &reference.definition {
            let mut members: Vec<Enumerator> = Vec::new();
            let mut next = 0i64;
            for member in definition.members.iter().flatten() {
                let value = match &member.value {
                    Some(expression) => self.constant_value(expression).unwrap_or(next),
                    None => next,
                };
                members.push(Enumerator {
                    name: member.name.clone(),
                    value,
                });
                next = value.saturating_add(1);
            }
            let ty = CType::Enum(Box::new(EnumType {
                tag: reference.tag.clone(),
                members,
                complete: true,
            }));
            if !key.is_empty() {
                self.tags.insert(key, ty.clone());
            }
            return ty;
        }
        self.tags.get(&key).cloned().unwrap_or(CType::int())
    }

    ///
    /// C's declarator rule is the opposite of what the list looks like, and this
    /// is where it is stated once. The derivation list is in *source* order, so
    /// `int *f(void)` is `[Pointer, Function]` and `int (*f)(void)` is
    /// `[Pointer, Function]` as well — the same list meaning opposite things. The
    /// difference is whether the function's parentheses were written, and that is
    /// why [`Derivation::Function`] records it.
    ///
    /// - **Written parentheses** (`int (*f)(void)`): the derivations to the left
    ///   are pointers *to* the function, so the function is built first and the
    ///   prefix wraps it.
    /// - **Unwritten** (`int *f(void)`): the derivations to the left belong to
    ///   the function's *return* type, so they are folded into the return type and
    ///   the function wraps that.
    ///
    /// Everything else is an array or another pointer, folded left to right.
    fn declarator_type(&mut self, base: &CType, declarator: &Declarator) -> CType {
        let derivation = &declarator.derivation;
        let function = derivation
            .iter()
            .position(|entry| matches!(entry, Derivation::Function(..)));
        let Some(at) = function else {
            return self.with_reversed_arrays(base.clone(), derivation);
        };
        let signature = self.signature(base, &derivation[at]);
        let Derivation::Function(_, _, parenthesised) = &derivation[at] else {
            unreachable!("the position came from this very match");
        };
        if *parenthesised {
            let mut ty = signature;
            for entry in &derivation[..at] {
                ty = self.derived(ty, entry);
            }
            return ty;
        }
        let mut result = base.clone();
        for entry in &derivation[..at] {
            result = self.derived(result, entry);
        }
        self.with_result(signature, result)
    }

    /// Applies a declarator's derivations with the array ones in reverse.
    ///
    /// C's postfix derivations associate right to left, so `T a[N][M]` is an array of
    /// N of an array of M — the dimensions are read outermost-first in the source and
    /// therefore have to be *applied* innermost-first. A `*` is a prefix operator and
    /// binds looser than every suffix, so the pointers are applied in source order at
    /// the front and only the arrays are reversed.
    fn with_reversed_arrays(&mut self, base: CType, derivation: &[Derivation]) -> CType {
        let mut ty = base;
        for entry in derivation {
            if !matches!(entry, Derivation::Array(_)) {
                ty = self.derived(ty, entry);
            }
        }
        for entry in derivation.iter().rev() {
            if matches!(entry, Derivation::Array(_)) {
                ty = self.derived(ty, entry);
            }
        }
        ty
    }

    /// One derivation applied to a type.
    fn derived(&mut self, ty: CType, derivation: &Derivation) -> CType {
        match derivation {
            Derivation::Pointer(_) => CType::Pointer(Box::new(ty)),
            Derivation::Array(length) => {
                let count = match length {
                    Some(expression) => self
                        .constant_value(expression)
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap_or(1),
                    None => 1,
                };
                CType::array_of(ty, count.max(1))
            }
            // A second function derivation in one declarator is a function
            // returning a function, which C does not have a type for. Reaching
            // here means the parser produced something the grammar forbids, and
            // `ty` is the honest fallback: the compiler reports the parse.
            Derivation::Function(..) => ty,
        }
    }

    /// A function type with `result` as its return type.
    fn with_result(&mut self, signature: CType, result: CType) -> CType {
        match signature {
            CType::Function(mut function) => {
                function.result = result;
                CType::Function(function)
            }
            other => other,
        }
    }

    /// The function type a `Function` derivation describes, with the base as its
    /// return type.
    fn signature(&mut self, base: &CType, derivation: &Derivation) -> CType {
        let Derivation::Function(parameters, variadic, _) = derivation else {
            return base.clone();
        };
        let mut params = Vec::new();
        for parameter in parameters {
            let parameter_base = self.specifier_type(&parameter.base);
            let parameter_ty = match &parameter.declarator {
                Some(inner) => self.declarator_type(&parameter_base, inner),
                None => parameter_base,
            };
            // C adjusts a parameter's type before the body sees it: an array
            // parameter is a pointer parameter, and a function parameter is a
            // pointer parameter. Without this the body would be handed an array
            // to subscript and a function to call.
            params.push(parameter_ty.decayed());
        }
        // `f(void)` names no parameters. `f()` in a *definition* also names none,
        // and the distinction only matters in a declaration, where a lone `void`
        // parameter is the only way to say it.
        if params.len() == 1 && matches!(params[0], CType::Void) {
            params.clear();
        }
        CType::Function(Box::new(FunctionType {
            result: base.clone(),
            params,
            variadic: *variadic,
            names: Vec::new(),
        }))
    }

    fn type_name_type(&mut self, name: &TypeName) -> CType {
        let base = self.specifier_type(&name.base);
        let declarator = Declarator {
            name: None,
            derivation: name.derivation.clone(),
            initial: None,
            span: name.span.clone(),
        };
        self.declarator_type(&base, &declarator)
    }

    /// Records how many bytes a `sizeof` expression is, keyed by where it starts.
    ///
    /// The same reasoning as `cast_types` applies, and for the same reason: a
    /// `sizeof` operand names a *type*, and the emitter has no way to build one —
    /// it would need this stage's typedef and tag tables. Worse, the answer is not
    /// something the emitter can work out from the operand it lowers, because
    /// `sizeof` never decays: `sizeof buffer` is the array's size and
    /// `sizeof (buffer + 0)` is a word, and the two differ only in the checker's
    /// view of the operand. A `sizeof` that answered a word for both made every
    /// `char` buffer in the runtime eight times too large.
    fn record_sizeof(&mut self, expression: &Expression, ty: &CType) {
        // C has no rule for the size of a function, and says it is 1. A type with
        // no size at all — `void`, or an incomplete record — is an error the
        // checker has already reported, and a byte keeps the lowering going so
        // the program still gets its other diagnostics.
        let size = match ty {
            CType::Function(_) => 1,
            other => other.size_in_bytes().unwrap_or(1),
        };
        let _ = self
            .sizeofs
            .insert(expression_span(expression).start().as_u32(), size);
    }

    // -- declarations --

    fn collect_typedefs(&mut self, var: &VarDecl) {
        if !var.typedef {
            return;
        }
        let base = self.specifier_type(&var.base);
        for declarator in &var.declarators {
            let Some(name) = &declarator.name else {
                continue;
            };
            if declarator
                .derivation
                .last()
                .is_some_and(|derivation| matches!(derivation, Derivation::Function(_, _, _)))
            {
                // A prototype is a function with no body, and a function with no
                // body has no storage and no code. Its type is in
                // `function_types`, which is where a call finds it; there is
                // nothing to lay out here.
                continue;
            }
            let ty = self.declarator_type(&base, declarator);
            self.typedefs.insert(name.clone(), ty);
        }
    }

    fn record(&mut self, definition: &RecordDefinition) {
        let key = definition.tag.clone().unwrap_or_default();
        self.build_record(definition, &key);
    }

    fn enumeration(&mut self, definition: &EnumDefinition) {
        let reference = EnumReference {
            tag: definition.tag.clone(),
            definition: Some(Box::new(definition.clone())),
            span: definition.span.clone(),
        };
        self.enum_type(&reference);
    }

    fn static_assertion(&mut self, assertion: &StaticAssert) {
        let value = self.constant_value(&assertion.condition);
        match value {
            Some(value) if value != 0 => {}
            Some(_) => {
                let message = assertion
                    .message
                    .clone()
                    .unwrap_or_else(|| String::from("the condition is false"));
                self.error(
                    assertion.span.clone(),
                    codes::STATIC_ASSERT,
                    alloc::format!("this assertion is false: {message}"),
                    "a static assertion is checked at compile time, and its condition must \
                     be an integer constant expression that is not zero",
                );
            }
            None => {
                self.error(
                    assertion.span.clone(),
                    codes::STATIC_ASSERT,
                    String::from("a static assertion's condition is not a constant"),
                    String::from("the condition must be computable at compile time"),
                );
            }
        }
    }

    fn global(&mut self, var: &VarDecl) {
        if var.typedef {
            return;
        }
        self.collect_typedefs(var);
        let base = self.specifier_type(&var.base);
        for declarator in &var.declarators {
            let Some(name) = &declarator.name else {
                continue;
            };
            if declarator
                .derivation
                .last()
                .is_some_and(|derivation| matches!(derivation, Derivation::Function(_, _, _)))
            {
                // A prototype is a function with no body, and a function with no
                // body has no storage and no code. Its type is in
                // `function_types`, which is where a call finds it; there is
                // nothing here to lay out, and laying something out would give
                // the linker a data segment for a function nobody can call.
                continue;
            }
            let ty = self.declarator_type(&base, declarator);
            if let (false, Some(initial)) = (var.extern_, &declarator.initial) {
                self.check_initialiser(initial, &ty, declarator.span.clone());
            }
            if self.globals.iter().any(|existing| existing.name == *name) {
                self.error(
                    declarator.span.clone(),
                    codes::REDEFINED,
                    alloc::format!("`{name}` is already defined at file scope"),
                    "a file-scope name has one definition",
                );
                continue;
            }
            self.globals.push(CheckedVariable {
                name: name.clone(),
                ty,
                initial: declarator.initial.clone(),
                span: declarator.span.clone(),
                file_scope: true,
            });
        }
    }

    fn function(&mut self, definition: &FunctionDefinition) {
        let base = self.specifier_type(&definition.base);
        let ty = self.declarator_type(&base, &definition.declarator);
        let CType::Function(signature) = ty.clone() else {
            self.error(
                definition.span.clone(),
                codes::MISMATCH,
                alloc::format!("`{}` is not a function", definition.name),
                "a function definition's declarator must end in a function type",
            );
            return;
        };
        if signature.variadic {
            self.error(
                definition.span.clone(),
                codes::UNSUPPORTED,
                alloc::format!(
                    "`{}` is defined as a variadic function, which this machine cannot do",
                    definition.name
                ),
                "the machine's ABI has no place in a frame to put the arguments past the \
                 fixed ones, and a variadic function's body has to be able to find out how \
                 many it was given; a variadic *declaration* is fine, so `printf` can still \
                 be called",
            );
        }
        if signature
            .result
            .size_in_bytes()
            .is_some_and(|size| size > 8)
        {
            self.error(
                definition.span.clone(),
                codes::UNSUPPORTED,
                alloc::format!(
                    "`{}` returns `{}`, which is larger than a word",
                    definition.name,
                    signature.result.name()
                ),
                "the machine's ABI has one return register and no multiword return, so a \
                 value that does not fit in a word cannot be returned; return a pointer, or \
                 write through one",
            );
        }
        if self
            .functions
            .iter()
            .any(|existing| existing.name == definition.name)
        {
            self.error(
                definition.span.clone(),
                codes::REDEFINED,
                alloc::format!("`{}` is already defined", definition.name),
                "a function has one definition; a second one is a second symbol with the \
                 same name, and the linker would refuse the object",
            );
        }
        self.current_function = definition.name.clone();
        self.return_type = Some(signature.result.clone());
        self.scopes.push(BTreeMap::new());
        let mut parameters = Vec::new();
        if let Some(Derivation::Function(declarations, _, _)) =
            definition.declarator.derivation.last()
        {
            for parameter in declarations {
                let parameter_base = self.specifier_type(&parameter.base);
                let parameter_ty = match &parameter.declarator {
                    Some(inner) => self.declarator_type(&parameter_base, inner),
                    None => parameter_base.decayed(),
                };
                let name = parameter
                    .declarator
                    .as_ref()
                    .and_then(|inner| inner.name.clone());
                if let Some(name) = &name {
                    self.declare_local(name.clone(), parameter_ty.clone());
                }
                parameters.push((name, parameter_ty));
            }
        }
        // `f(void)` names no parameters, and `f()` in a *definition* also names
        // none — C says an empty parameter list in a definition means no
        // parameters. The distinction between the two only matters in a
        // declaration, and there a `void` parameter is the only way to say it.
        if parameters.len() == 1 && matches!(parameters[0].1, CType::Void) {
            parameters.clear();
        }
        self.body(&definition.body);
        self.scopes.pop();
        self.return_type = None;
        self.current_function = String::new();
        self.functions.push(CheckedFunction {
            name: definition.name.clone(),
            ty,
            parameters,
            body: (*definition.body).clone(),
            locals: core::mem::take(&mut self.locals),
            span: definition.span.clone(),
        });
    }

    fn declare_local(&mut self, name: String, ty: CType) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, ty);
        }
    }

    fn lookup(&self, name: &str) -> Option<CType> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    // -- statements --

    fn body(&mut self, block: &Block) {
        self.scopes.push(BTreeMap::new());
        for item in &block.items {
            match item {
                BlockItem::Declaration(var) => self.local(var),
                BlockItem::Statement(statement) => self.statement(statement),
            }
        }
        self.scopes.pop();
    }

    fn local(&mut self, var: &VarDecl) {
        if var.typedef {
            self.collect_typedefs(var);
            return;
        }
        let base = self.specifier_type(&var.base);
        for declarator in &var.declarators {
            let Some(name) = &declarator.name else {
                continue;
            };
            let ty = self.declarator_type(&base, declarator);
            if ty.is_void() {
                self.error(
                    declarator.span.clone(),
                    codes::MISMATCH,
                    alloc::format!("`{name}` cannot be `void`"),
                    "a variable needs a type it can hold, and `void` holds nothing",
                );
                continue;
            }
            if let Some(initial) = &declarator.initial {
                self.check_initialiser(initial, &ty, declarator.span.clone());
            }
            self.locals.push((name.clone(), ty.clone()));
            self.declare_local(name.clone(), ty);
        }
    }

    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Block(block) => self.body(block),
            Statement::Expression(expression) => {
                self.expression(expression);
            }
            Statement::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.scalar(condition);
                self.statement(then_branch);
                if let Some(else_branch) = else_branch {
                    self.statement(else_branch);
                }
            }
            Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
                self.scalar(condition);
                self.loop_depth += 1;
                self.statement(body);
                self.loop_depth -= 1;
            }
            Statement::For {
                initialiser,
                condition,
                step,
                body,
            } => {
                self.scopes.push(BTreeMap::new());
                if let Some(init) = initialiser {
                    match &**init {
                        ForInit::Declaration(var) => self.local(var),
                        ForInit::Expression(expression) => {
                            self.expression(expression);
                        }
                    }
                }
                if let Some(condition) = condition {
                    self.scalar(condition);
                }
                if let Some(step) = step {
                    self.expression(step);
                }
                self.loop_depth += 1;
                self.statement(body);
                self.loop_depth -= 1;
                self.scopes.pop();
            }
            Statement::Switch { condition, body } => {
                let ty = self.expression(condition);
                let promoted = self.promote(ty);
                if !promoted.is_integer() {
                    self.error(
                        condition_span(condition),
                        codes::WRONG_OPERAND,
                        alloc::format!(
                            "a `switch` needs an integer, and this is `{}`",
                            promoted.name()
                        ),
                        "there is no floating point on this machine, so there is nothing else \
                         a value could be",
                    );
                }
                self.switch_depth += 1;
                self.statement(body);
                self.switch_depth -= 1;
            }
            Statement::Case { value, statement } => {
                if self.switch_depth == 0 {
                    self.error(
                        value_span(value),
                        codes::BAD_CASE,
                        String::from("this `case` is not inside a `switch`"),
                        String::from("a `case` label belongs to the switch that encloses it"),
                    );
                }
                if self.constant_value(value).is_none() {
                    self.error(
                        value_span(value),
                        codes::BAD_CASE,
                        String::from("a `case` value must be a constant"),
                        String::from(
                            "the switch has to be able to compare against it without \
                                     running anything",
                        ),
                    );
                }
                self.statement(statement);
            }
            Statement::Default { statement, .. } => self.statement(statement),
            Statement::Break(span) => {
                if self.loop_depth == 0 && self.switch_depth == 0 {
                    self.error(
                        span.clone(),
                        codes::BAD_CASE,
                        String::from("there is no loop or `switch` to break out of"),
                        String::from("`break` applies to the innermost enclosing loop or switch"),
                    );
                }
            }
            Statement::Continue(span) => {
                if self.loop_depth == 0 {
                    self.error(
                        span.clone(),
                        codes::BAD_CASE,
                        String::from("there is no loop to continue"),
                        String::from("`continue` applies to the innermost enclosing loop"),
                    );
                }
            }
            Statement::Empty => {}
            Statement::StaticAssert(assertion) => self.static_assertion(assertion),
            Statement::Return { value, span } => {
                let expected = self.return_type.clone().unwrap_or(CType::Void);
                match value {
                    None => {
                        if !expected.is_void() {
                            self.error(
                                span.clone(),
                                codes::BAD_RETURN,
                                alloc::format!(
                                    "`{}` returns `{}`, so a `return` needs a value",
                                    self.current_function,
                                    expected.name()
                                ),
                                "a function that returns a value must return one on every path",
                            );
                        }
                    }
                    Some(value) => {
                        if expected.is_void() {
                            self.error(
                                span.clone(),
                                codes::BAD_RETURN,
                                alloc::format!(
                                    "`{}` returns nothing, so a `return` cannot have a value",
                                    self.current_function
                                ),
                                "drop the value, or give the function a return type",
                            );
                            self.expression(value);
                            return;
                        }
                        let actual = self.expression(value);
                        let zero = self.is_null_constant(value);
                        self.convertible(&actual, &expected, span.clone(), zero);
                    }
                }
            }
            Statement::Goto { .. } => {}
            Statement::Label { statement, .. } => self.statement(statement),
            Statement::Declaration(var) => self.local(var),
        }
    }

    /// Checks an expression used as a condition.
    ///
    /// C says a condition may have any *scalar* type, so `if (p)` and
    /// `if (*p)` are both right. What is refused is a record, which has no
    /// single truth value.
    fn scalar(&mut self, expression: &Expression) {
        let ty = self.expression(expression);
        let decayed = ty.decayed();
        if decayed.is_integer() || decayed.is_pointer() {
            return;
        }
        self.error(
            expression_span(expression),
            codes::WRONG_OPERAND,
            alloc::format!("a condition must be a value, and this is `{}`", ty.name()),
            "a condition may be any number or pointer; a whole record has no single truth value",
        );
    }

    fn check_initialiser(&mut self, initial: &Initializer, ty: &CType, span: SourceSpan) {
        match initial {
            Initializer::Scalar(expression) => {
                let actual = self.expression(expression);
                {
                    let zero = self.is_null_constant(expression);
                    self.convertible(&actual, &ty.decayed(), span, zero);
                }
            }
            Initializer::List {
                items,
                span: list_span,
            } => {
                match ty {
                    CType::Array { element, length } => {
                        // A braced list of a scalar type is legal C, and it means
                        // the one value: `int x = {1};`.
                        if matches!(**element, CType::Void) {
                            return;
                        }
                        if items.len() as u64 > u64::from(*length) {
                            self.error(
                                list_span.clone(),
                                codes::BAD_INITIALISER,
                                alloc::format!(
                                    "this list has {} values for an array of {}",
                                    items.len(),
                                    length
                                ),
                                "an array's initialiser may not have more values than it has \
                                 elements",
                            );
                        }
                        for item in items {
                            self.check_initialiser(&item.value, element, item_span(item));
                        }
                    }
                    CType::Struct(_) | CType::Union(_) => {
                        let record = self.record_of(ty);
                        if items.len() as u64 > record.fields.len() as u64 {
                            self.error(
                                list_span.clone(),
                                codes::BAD_INITIALISER,
                                alloc::format!(
                                    "this list has {} values for a type with {} members",
                                    items.len(),
                                    record.fields.len()
                                ),
                                "a record's initialiser may not have more values than it has \
                                 members",
                            );
                        }
                        for (index, item) in items.iter().enumerate() {
                            let target = match &item.designator {
                                Some(Designator::Field { name, span }) => {
                                    match record.fields.iter().find(|field| &field.name == name) {
                                        Some(field) => field.ty.clone(),
                                        None => {
                                            self.error(
                                                span.clone(),
                                                codes::NO_SUCH_MEMBER,
                                                alloc::format!(
                                                    "`{}` has no member named `{name}`",
                                                    ty.name()
                                                ),
                                                "check the member's name and its spelling",
                                            );
                                            CType::int()
                                        }
                                    }
                                }
                                Some(Designator::Index { span, .. }) => {
                                    self.error(
                                        span.clone(),
                                        codes::BAD_INITIALISER,
                                        String::from("a `[...]` designator is for an array"),
                                        String::from("use `.member` for a struct or a union"),
                                    );
                                    CType::int()
                                }
                                None => match record.fields.get(index) {
                                    Some(field) => field.ty.clone(),
                                    None => CType::int(),
                                },
                            };
                            self.check_initialiser(&item.value, &target, item_span(item));
                        }
                    }
                    other => {
                        if let Some(first) = items.first() {
                            // `int x = {1};` — a braced scalar is one value.
                            self.check_initialiser(&first.value, other, list_span.clone());
                        }
                    }
                }
            }
        }
    }

    fn record_of(&self, ty: &CType) -> RecordType {
        match ty {
            CType::Struct(record) | CType::Union(record) => (**record).clone(),
            _ => RecordType {
                tag: None,
                union: false,
                fields: Vec::new(),
                complete: false,
            },
        }
    }

    // -- expressions --

    /// Checks an expression and returns its type.
    fn expression(&mut self, expression: &Expression) -> CType {
        match expression {
            Expression::Integer { number, span } => self.integer_constant(number, span.clone()),
            Expression::Character { .. } => CType::int(),
            Expression::String { value, .. } => {
                // A string literal is `char[N+1]`, and the `+1` is the
                // terminating null: a program that reads one past the last
                // character is reading a zero, not another guest's memory.
                CType::array_of(
                    CType::Int {
                        bits: 8,
                        signed: true,
                    },
                    value.len() as u32 + 1,
                )
            }
            Expression::Name { name, span } => self.name(name, span.clone()),
            Expression::SizeofType(name) => {
                let ty = self.type_name_type(name);
                self.record_sizeof(expression, &ty);
                CType::ulong()
            }
            Expression::SizeofExpression(inner) => {
                let ty = self.expression(inner);
                self.record_sizeof(expression, &ty);
                CType::ulong()
            }
            Expression::Group(inner) => self.expression(inner),
            Expression::Address(inner) => {
                let ty = self.expression(inner);
                match &ty {
                    CType::Pointer(_) => {
                        self.error(
                            expression_span(inner),
                            codes::NOT_LVALUE,
                            String::from("a pointer has no address to take"),
                            String::from("`&p` is not something a program can mean"),
                        );
                        CType::Pointer(Box::new(CType::int()))
                    }
                    CType::Array { element, .. } => CType::Pointer(element.clone()),
                    CType::Function(_) => CType::Pointer(Box::new(ty)),
                    CType::Void => {
                        self.error(
                            expression_span(inner),
                            codes::NOT_LVALUE,
                            String::from("`void` has no address to take"),
                            String::from("`&x` where `x` is `void` names nothing"),
                        );
                        CType::Pointer(Box::new(CType::int()))
                    }
                    other => CType::Pointer(Box::new(other.clone())),
                }
            }
            Expression::Dereference(inner) => {
                let ty = self.expression(inner).decayed();
                match ty {
                    CType::Pointer(pointee) => *pointee,
                    other => {
                        self.error(
                            expression_span(inner),
                            codes::WRONG_OPERAND,
                            alloc::format!("`*` needs a pointer, and this is `{}`", other.name()),
                            "dereferencing a number would read whatever the number points at",
                        );
                        CType::int()
                    }
                }
            }
            Expression::Plus(inner) | Expression::Minus(inner) => {
                let ty = self.expression(inner);
                let promoted = self.promote(ty);
                if !promoted.is_integer() {
                    self.error(
                        expression_span(inner),
                        codes::WRONG_OPERAND,
                        alloc::format!(
                            "unary arithmetic needs a number, and this is `{}`",
                            promoted.name()
                        ),
                        "there is no floating point on this machine",
                    );
                }
                if matches!(expression, Expression::Minus(_))
                    && matches!(promoted, CType::Int { signed: false, .. })
                {
                    self.error(
                        expression_span(inner),
                        codes::WRONG_OPERAND,
                        alloc::format!(
                            "negating an unsigned `{}` wraps instead of becoming negative",
                            promoted.name()
                        ),
                        "C allows it, and this compiler says so rather than doing it silently",
                    );
                }
                promoted
            }
            Expression::BitNot(inner) => {
                let promoted = self.expression(inner);
                let ty = self.promote(promoted);
                if !ty.is_integer() {
                    self.error(
                        expression_span(inner),
                        codes::WRONG_OPERAND,
                        alloc::format!("`~` needs an integer, and this is `{}`", ty.name()),
                        "there is no floating point on this machine",
                    );
                }
                ty
            }
            Expression::Not(inner) => {
                let ty = self.expression(inner).decayed();
                if !ty.is_integer() && !ty.is_pointer() {
                    self.error(
                        expression_span(inner),
                        codes::WRONG_OPERAND,
                        alloc::format!("`!` needs a value, and this is `{}`", ty.name()),
                        "a whole record has no single truth value to negate",
                    );
                }
                CType::int()
            }
            Expression::Increment {
                operand, prefix, ..
            } => {
                let ty = self.expression(operand);
                if !ty.decayed().is_arithmetic() {
                    self.error(
                        expression_span(operand),
                        codes::NOT_LVALUE,
                        alloc::format!(
                            "`{}` cannot be incremented: it is `{}`",
                            if *prefix { "++" } else { "++'s result" },
                            ty.name()
                        ),
                        "only a number has a next value",
                    );
                }
                ty
            }
            Expression::Binary { op, left, right } => {
                let left_ty = self.expression(left).decayed();
                let right_ty = self.expression(right).decayed();
                self.binary(*op, &left_ty, &right_ty, left, right)
            }
            Expression::Logical { left, right, .. } => {
                let left_ty = self.expression(left).decayed();
                let right_ty = self.expression(right).decayed();
                for (ty, span) in [(left_ty, left), (right_ty, right)] {
                    if !ty.is_integer() && !ty.is_pointer() {
                        self.error(
                            expression_span(span),
                            codes::WRONG_OPERAND,
                            alloc::format!(
                                "a short-circuiting operator needs a value, and this is `{}`",
                                ty.name()
                            ),
                            "there is no floating point on this machine",
                        );
                    }
                }
                // C says `&&` and `||` produce an `int`, not a `bool`, and the
                // difference is visible: `printf("%d", a && b)` is correct.
                CType::int()
            }
            Expression::Conditional {
                condition,
                then_value,
                else_value,
            } => {
                self.scalar(condition);
                let then_ty = self.expression(then_value).decayed();
                let else_ty = self.expression(else_value).decayed();
                if then_ty.is_arithmetic() && else_ty.is_arithmetic() {
                    return self.usual_arithmetic(&then_ty, &else_ty);
                }
                if then_ty.is_pointer() && else_ty.is_pointer() {
                    return then_ty;
                }
                then_ty
            }
            Expression::Call { callee, arguments } => self.call(callee, arguments),
            Expression::Subscript { array, index } => {
                let array_ty = self.expression(array).decayed();
                let index_ty = self.expression(index);
                let element = match array_ty {
                    CType::Pointer(pointee) => *pointee,
                    other => {
                        self.error(
                            expression_span(array),
                            codes::WRONG_OPERAND,
                            alloc::format!(
                                "`[]` needs an array or a pointer, and this is `{}`",
                                other.name()
                            ),
                            "subscripting a number would read wherever that number points",
                        );
                        return CType::int();
                    }
                };
                if !index_ty.decayed().is_integer() {
                    self.error(
                        expression_span(index),
                        codes::WRONG_OPERAND,
                        alloc::format!(
                            "a subscript must be an integer, and this is `{}`",
                            index_ty.name()
                        ),
                        "there is no floating point on this machine",
                    );
                }
                element
            }
            Expression::Member {
                record,
                member,
                arrow,
                span,
            } => self.member(record, member, *arrow, span.clone()),
            Expression::CompoundAssign { op, target, value } => {
                let target_ty = self.expression(target);
                let value_ty = self.expression(value);
                if !self.is_lvalue(target) {
                    self.error(
                        expression_span(target),
                        codes::NOT_LVALUE,
                        String::from("the left of a compound assignment must be a place"),
                        "there is nowhere to store the result",
                    );
                }
                if target_ty.is_pointer() {
                    // Pointer arithmetic: `p += 1` advances by one element, which
                    // is the only pointer arithmetic C has, and it needs the
                    // element's size at run time.
                    if !matches!(op, BinaryOp::Add | BinaryOp::Subtract) {
                        self.error(
                            expression_span(target),
                            codes::WRONG_OPERAND,
                            alloc::format!("`{}=` does not work on a pointer", op.spelling()),
                            "a pointer moves by adding or subtracting",
                        );
                    }
                    if !value_ty.decayed().is_integer() {
                        self.error(
                            expression_span(value),
                            codes::WRONG_OPERAND,
                            alloc::format!(
                                "moving a pointer needs a number of elements, and this is `{}`",
                                value_ty.name()
                            ),
                            "there is no floating point on this machine",
                        );
                    }
                    return target_ty;
                }
                self.binary(*op, &target_ty, &value_ty, target, value);
                target_ty
            }
            Expression::Assign { target, value } => {
                let target_ty = self.expression(target);
                let value_ty = self.expression(value);
                if !self.is_lvalue(target) {
                    self.error(
                        expression_span(target),
                        codes::NOT_LVALUE,
                        String::from("the left of an assignment must be a place"),
                        "there is nowhere to store the result",
                    );
                }
                match &target_ty {
                    CType::Void => {
                        self.error(
                            expression_span(target),
                            codes::NOT_ASSIGNABLE,
                            String::from("nothing can be assigned to a `void`"),
                            String::from("`void` is the type of no value at all"),
                        );
                    }
                    CType::Array { .. } | CType::Function(_) => {
                        self.error(
                            expression_span(target),
                            codes::NOT_ASSIGNABLE,
                            alloc::format!("`{}` cannot be assigned to", target_ty.name()),
                            "an array and a function are not places a value can be stored in",
                        );
                    }
                    _ => {
                        let zero = self.is_null_constant(value);
                        self.convertible(&value_ty, &target_ty, expression_span(target), zero);
                    }
                }
                target_ty
            }
            Expression::Comma { left, right } => {
                self.expression(left);
                self.expression(right)
            }
            Expression::Cast { ty, operand } => {
                let to = self.type_name_type(ty);
                // The target is recorded so the emitter can apply it. A cast that is
                // dropped is not a missed optimisation: `(unsigned char)` on a signed
                // load is a *different value*, and `(int)` on a pointer-sized value is
                // a different width.
                let _ = self
                    .cast_types
                    .insert(expression_span(expression).start().as_u32(), to.clone());
                let from = self.expression(operand);
                if to.is_void() {
                    return CType::Void;
                }
                if !to.is_arithmetic() && !to.is_pointer() {
                    self.error(
                        ty.span.clone(),
                        codes::MISMATCH,
                        alloc::format!("a cast needs a scalar type, and this is `{}`", to.name()),
                        "a record has no single representation to convert a value into",
                    );
                }
                if !from.decayed().is_arithmetic() && !from.decayed().is_pointer() {
                    self.error(
                        expression_span(operand),
                        codes::MISMATCH,
                        alloc::format!(
                            "a cast needs something to convert, and this is `{}`",
                            from.name()
                        ),
                        "a record has no single value to convert",
                    );
                }
                to
            }
        }
    }

    /// A name's type, whether it is a local, a global or a function.
    ///
    /// The three tables are consulted in that order: a local shadows a global,
    /// and a function is neither. A name in none of them was reported by the
    /// resolver, so the `int` here is a placeholder for a program that already
    /// failed — and it is `int` rather than a panic because a second failure on
    /// an already-failing program helps nobody.
    /// The type a name has, reporting it if nothing declares one.
    ///
    /// Every source of a name is asked in turn — a local, a function, the
    /// standard library, the OS ABI, a file-scope variable — and the first that
    /// knows it answers. If none does, that is an error, and it was not always
    /// one: an unknown name used to be given the type `int`, which is the same
    /// thing as assuming the program was right about a declaration nobody wrote.
    /// `printf("hi")` therefore type-checked as *calling an `int`*, and the
    /// program was told `int` cannot be called, which is a complaint about the
    /// wrong name in the wrong place.
    fn name(&mut self, name: &str, span: SourceSpan) -> CType {
        if let Some(ty) = self.lookup(name) {
            return ty;
        }
        if let Some(ty) = self.function_types.get(name) {
            return ty.clone();
        }
        if let Some(ty) = library_signature(name) {
            return ty;
        }
        if let Some(ty) = abi_signature(name) {
            return ty;
        }
        if let Some((_, ty)) = self
            .program_globals
            .iter()
            .find(|(declared, _)| declared == name)
        {
            return ty.clone();
        }
        self.error(
            span,
            codes::UNDECLARED,
            alloc::format!("`{name}` is not declared"),
            "a name has to come from a declaration, from the standard library, or \
             from the OS ABI",
        );
        // An `int` so the rest of the program still gets checked and still gets
        // its other diagnostics. Every stage past this one keeps going after an
        // error on purpose; a second error on the same name would be noise.
        CType::int()
    }

    fn is_lvalue(&self, expression: &Expression) -> bool {
        match expression {
            Expression::Name { .. } => true,
            Expression::Dereference(_) => true,
            Expression::Subscript { .. } => true,
            Expression::Member { .. } => true,
            Expression::Group(inner) => self.is_lvalue(inner),
            _ => false,
        }
    }

    fn member(&mut self, record: &Expression, name: &str, arrow: bool, span: SourceSpan) -> CType {
        let ty = self.expression(record).decayed();
        let record_ty = if arrow {
            match &ty {
                CType::Pointer(pointee) => (**pointee).clone(),
                other => {
                    self.error(
                        expression_span(record),
                        codes::WRONG_OPERAND,
                        alloc::format!("`->` needs a pointer, and this is `{}`", other.name()),
                        "`->` is shorthand for `(*x).member`, so `x` must be a pointer",
                    );
                    return CType::int();
                }
            }
        } else {
            ty
        };
        if !matches!(&record_ty, CType::Struct(_) | CType::Union(_)) {
            self.error(
                expression_span(record),
                codes::WRONG_OPERAND,
                alloc::format!(
                    "`.` needs a struct or a union, and this is `{}`",
                    record_ty.name()
                ),
                "a number has no members",
            );
            return CType::int();
        };
        let layout = self.record_of(&record_ty);
        if !layout.complete {
            self.error(
                expression_span(record),
                codes::INCOMPLETE,
                alloc::format!("`{}` is not a complete type", record_ty.name()),
                "its members are not known yet, so there is nothing to read",
            );
            return CType::int();
        }
        match layout.fields.iter().find(|field| field.name == name) {
            Some(field) => {
                if field.name.is_empty() {
                    // An anonymous member: the name belongs to a member of it,
                    // and the field type is that member's own record.
                    self.member(
                        &Expression::Name {
                            name: name.to_string(),
                            span: span.clone(),
                        },
                        name,
                        false,
                        span.clone(),
                    )
                } else {
                    field.ty.clone()
                }
            }
            None => {
                self.error(
                    span,
                    codes::NO_SUCH_MEMBER,
                    alloc::format!("`{}` has no member named `{name}`", record_ty.name()),
                    {
                        let members: Vec<&str> = layout
                            .fields
                            .iter()
                            .filter(|field| !field.name.is_empty())
                            .map(|field| field.name.as_str())
                            .collect();
                        if members.is_empty() {
                            "it has no members at all".to_string()
                        } else {
                            alloc::format!("it has: {}", members.join(", "))
                        }
                    },
                );
                CType::int()
            }
        }
    }

    fn call(&mut self, callee: &Expression, arguments: &[Expression]) -> CType {
        let callee_ty = self.expression(callee).decayed();
        let mut argument_types = Vec::new();
        for argument in arguments {
            argument_types.push(self.expression(argument).decayed());
        }
        let signature = match &callee_ty {
            CType::Pointer(pointee) => match &**pointee {
                CType::Function(signature) => signature.clone(),
                _ => {
                    self.error(
                        expression_span(callee),
                        codes::WRONG_OPERAND,
                        alloc::format!("`{}` is not a function", callee_ty.name()),
                        "a call needs something to call",
                    );
                    return CType::int();
                }
            },
            CType::Function(signature) => signature.clone(),
            other => {
                self.error(
                    expression_span(callee),
                    codes::WRONG_OPERAND,
                    alloc::format!("`{}` cannot be called", other.name()),
                    "a call needs a function or a pointer to one",
                );
                return CType::int();
            }
        };
        if argument_types.len() < signature.params.len() {
            self.error(
                expression_span(callee),
                codes::ARITY,
                alloc::format!(
                    "this function takes {} argument{} but {} {} passed",
                    signature.params.len(),
                    if signature.params.len() == 1 { "" } else { "s" },
                    argument_types.len(),
                    if argument_types.len() == 1 {
                        "was"
                    } else {
                        "were"
                    },
                ),
                "every parameter a prototype names has to be given a value",
            );
        } else if argument_types.len() > signature.params.len() && !signature.variadic {
            self.error(
                expression_span(callee),
                codes::ARITY,
                alloc::format!(
                    "this function takes {} argument{} but {} were passed",
                    signature.params.len(),
                    if signature.params.len() == 1 { "" } else { "s" },
                    argument_types.len(),
                ),
                "a function that is not variadic has no place to put an extra argument",
            );
        }
        for (index, actual) in argument_types
            .iter()
            .enumerate()
            .take(signature.params.len())
        {
            let expected = &signature.params[index];
            // A zero argument is a null pointer constant and is allowed wherever
            // a pointer is expected, which is why the check asks about the *value*
            // and not only the type.
            let zero = self.is_null_constant(&arguments[index]);
            self.convertible(actual, expected, expression_span(&arguments[index]), zero);
        }
        signature.result.clone()
    }

    fn binary(
        &mut self,
        op: BinaryOp,
        left: &CType,
        right: &CType,
        left_expr: &Expression,
        right_expr: &Expression,
    ) -> CType {
        if op.is_comparison() {
            if op == BinaryOp::Equal || op == BinaryOp::NotEqual {
                // A pointer and a null pointer constant, or two pointers, may be
                // compared for equality. Two *integers* may be compared for
                // anything, and saying otherwise would refuse `c == 0` and
                // `i == 3` — which are the two comparisons a C program writes
                // most. Only a pointer on one side brings the rules, because only
                // then is the comparison about addresses.
                let any_pointer = left.is_pointer() || right.is_pointer();
                if any_pointer {
                    let both_pointers = left.is_pointer() && right.is_pointer();
                    let pointer_and_zero = left.is_pointer() && self.is_null_constant(right_expr)
                        || right.is_pointer() && self.is_null_constant(left_expr);
                    if !both_pointers && !pointer_and_zero {
                        self.error(
                            expression_span(left_expr),
                            codes::WRONG_OPERAND,
                            alloc::format!(
                                "cannot compare `{}` with `{}`",
                                left.name(),
                                right.name()
                            ),
                            "two pointers may be compared, and a pointer may be compared with a \
                             null pointer constant; anything else is not a comparison of \
                             addresses",
                        );
                    }
                } else if !left.is_integer() || !right.is_integer() {
                    self.error(
                        expression_span(left_expr),
                        codes::WRONG_OPERAND,
                        alloc::format!("cannot compare `{}` with `{}`", left.name(), right.name()),
                        "there is no floating point on this machine, and a record has no single \
                         value to compare",
                    );
                }
            } else if !left.is_integer() || !right.is_integer() {
                // `<` and `>` on two pointers is real C and a library needs it: a
                // `memmove` decides its copy direction by asking which region
                // starts lower. C requires the result to be meaningful only
                // within one array, and comparing unrelated pointers is undefined
                // — so this accepts the comparison and says what it means, rather
                // than refusing a line every C string routine is written with.
                let both_pointers = left.is_pointer() && right.is_pointer();
                if !both_pointers {
                    self.error(
                        expression_span(left_expr),
                        codes::WRONG_OPERAND,
                        alloc::format!(
                            "`<` and `>` need integers, and these are `{}` and `{}`",
                            left.name(),
                            right.name()
                        ),
                        "there is no floating point on this machine; two pointers may also be \
                         compared, and the result is meaningful only within one array",
                    );
                }
            }
            // A comparison produces an `int` in C, not a `_Bool`, and the
            // difference is visible in `printf("%d", a < b)`.
            return CType::int();
        }
        if left.is_pointer() || right.is_pointer() {
            // Pointer arithmetic, and the one place it is allowed: adding an
            // integer to a pointer, or subtracting one pointer from another.
            if op == BinaryOp::Subtract && left.is_pointer() && right.is_pointer() {
                return CType::long();
            }
            if op == BinaryOp::Add || op == BinaryOp::Subtract {
                if left.is_pointer() {
                    if !right.is_integer() {
                        self.error(
                            expression_span(right_expr),
                            codes::WRONG_OPERAND,
                            alloc::format!(
                                "a pointer can only move by a whole number of elements, and \
                                 this is `{}`",
                                right.name()
                            ),
                            "there is no floating point on this machine",
                        );
                    }
                    return left.clone();
                }
                if right.is_pointer() {
                    if !left.is_integer() {
                        self.error(
                            expression_span(left_expr),
                            codes::WRONG_OPERAND,
                            alloc::format!(
                                "a pointer can only move by a whole number of elements, and \
                                 this is `{}`",
                                left.name()
                            ),
                            "there is no floating point on this machine",
                        );
                    }
                    return right.clone();
                }
            }
            self.error(
                expression_span(left_expr),
                codes::WRONG_OPERAND,
                alloc::format!(
                    "`{}` does not work on `{}` and `{}`",
                    op.spelling(),
                    left.name(),
                    right.name()
                ),
                "a pointer can be added to or subtracted from an integer, and two pointers \
                 can be subtracted from each other; nothing else",
            );
            return CType::int();
        }
        if !left.is_integer() || !right.is_integer() {
            self.error(
                expression_span(left_expr),
                codes::WRONG_OPERAND,
                alloc::format!(
                    "`{}` needs integers, and these are `{}` and `{}`",
                    op.spelling(),
                    left.name(),
                    right.name()
                ),
                "there is no floating point on this machine",
            );
            return CType::int();
        }
        if op.is_shift() {
            // A shift's right operand is not part of the common type: C says it
            // is promoted on its own, and the *left* operand's type is the
            // result. `1 << 40` is an `int` and overflows, while `(long)1 << 40`
            // is a `long` and does not.
            return self.promote(left.clone());
        }
        self.usual_arithmetic(left, right)
    }

    /// C's usual arithmetic conversions.
    fn usual_arithmetic(&mut self, left: &CType, right: &CType) -> CType {
        let left = self.promote(left.clone());
        let right = self.promote(right.clone());
        let (
            CType::Int {
                bits: lb,
                signed: ls,
            },
            CType::Int {
                bits: rb,
                signed: rs,
            },
        ) = (left.clone(), right.clone())
        else {
            return left;
        };
        if lb == rb && ls == rs {
            return left;
        }
        if lb == rb {
            // Equal widths with different signedness: the unsigned one wins.
            return if ls { right } else { left };
        }
        if lb > rb {
            // The wider one wins, unless it cannot represent every value of the
            // narrower one, in which case the narrower one's unsigned version
            // does. That case is why `long` and `unsigned int` are both `long`
            // on this machine: a `long` holds every `unsigned int`.
            if ls || !rs || lb >= 64 {
                return left;
            }
            return CType::Int {
                bits: lb,
                signed: false,
            };
        }
        if rs || !ls || rb >= 64 {
            return right;
        }
        CType::Int {
            bits: rb,
            signed: false,
        }
    }

    /// C's integer promotion.
    fn promote(&self, ty: CType) -> CType {
        match ty {
            CType::Int { bits, .. } if bits < 32 => CType::int(),
            CType::Bool => CType::int(),
            CType::Enum(_) => CType::int(),
            other => other,
        }
    }

    /// Reports a conversion that changes what the value means.
    ///
    /// A conversion to a *narrower* type is allowed — C programs do it on
    /// purpose, and `c = 300` for a `char` is a documented truncation. A
    /// conversion to a *wider signed* type from a value that does not fit is
    /// not truncation but a wrong number, and that is reported.
    fn convertible(&mut self, from: &CType, to: &CType, span: SourceSpan, zero: bool) {
        if to.is_void() {
            return;
        }
        if from.is_arithmetic() && to.is_arithmetic() {
            return;
        }
        if to.is_pointer() {
            if from.is_pointer() || self.is_pointer_like(from) {
                return;
            }
            if from.is_integer() {
                // A null pointer constant is a way of *writing* a null pointer,
                // and `return 0;` from a function returning a pointer is the most
                // ordinary line in C. Anything else is refused, because a
                // pointer is an address and a number is not one.
                if zero {
                    return;
                }
                self.error(
                    span,
                    codes::WRONG_OPERAND,
                    alloc::format!("a number cannot be used where `{}` is expected", to.name()),
                    "a pointer is an address, and a number is not one; write `0` for a null \
                     pointer, or cast if the conversion is deliberate",
                );
                return;
            }
        }
        if from.is_pointer() && to.is_integer() {
            // Legal with a cast, which `Expression::Cast` allows and this path
            // does not see, because a cast does not go through here.
            return;
        }
        if from.is_pointer() && to.is_pointer() {
            return;
        }
        self.error(
            span,
            codes::MISMATCH,
            alloc::format!(
                "`{}` cannot be used where `{}` is expected",
                from.name(),
                to.name()
            ),
            "these are different kinds of value, and converting between them would mean \
             something the program did not say",
        );
    }

    fn is_pointer_like(&self, ty: &CType) -> bool {
        matches!(ty, CType::Array { .. } | CType::Function(_))
    }

    /// Whether an expression is the constant zero.
    fn is_null_constant(&mut self, expression: &Expression) -> bool {
        self.constant_value(expression) == Some(0)
    }

    /// An integer constant expression's value, if it has one.
    fn constant_value(&mut self, expression: &Expression) -> Option<i64> {
        match expression {
            Expression::Integer { number, span } => self.integer_value(number, span.clone()),
            Expression::Character { value, .. } => Some(*value),
            Expression::Group(inner) => self.constant_value(inner),
            Expression::SizeofType(name) => {
                let ty = self.type_name_type(name);
                ty.size_in_bytes().map(i64::from)
            }
            Expression::SizeofExpression(inner) => {
                let ty = self.expression(inner);
                ty.size_in_bytes().map(i64::from)
            }
            Expression::Cast { ty, operand } => {
                let target = self.type_name_type(ty);
                let value = self.constant_value(operand)?;
                if !target.is_integer() {
                    return None;
                }
                Some(truncate_to(value, &target))
            }
            Expression::Conditional {
                condition,
                then_value,
                else_value,
            } => {
                let test = self.constant_value(condition)?;
                if test != 0 {
                    self.constant_value(then_value)
                } else {
                    self.constant_value(else_value)
                }
            }
            Expression::Binary { op, left, right } => {
                let left = self.constant_value(left)?;
                let right = self.constant_value(right)?;
                if op.is_comparison() {
                    return Some(match op {
                        BinaryOp::Equal => i64::from(left == right),
                        BinaryOp::NotEqual => i64::from(left != right),
                        BinaryOp::Less => i64::from(left < right),
                        BinaryOp::LessEqual => i64::from(left <= right),
                        BinaryOp::Greater => i64::from(left > right),
                        BinaryOp::GreaterEqual => i64::from(left >= right),
                        _ => return None,
                    });
                }
                if op.is_shift() {
                    return match op {
                        BinaryOp::ShiftLeft if (0..64).contains(&right) => {
                            Some(left.wrapping_shl(right as u32))
                        }
                        BinaryOp::ShiftRight if (0..64).contains(&right) => {
                            Some(left.wrapping_shr(right as u32))
                        }
                        _ => None,
                    };
                }
                let common = self
                    .usual_arithmetic(&self.promote(CType::long()), &self.promote(CType::long()));
                let _ = common;
                match op {
                    BinaryOp::Add => left.checked_add(right),
                    BinaryOp::Subtract => left.checked_sub(right),
                    BinaryOp::Multiply => left.checked_mul(right),
                    BinaryOp::Divide if right != 0 => {
                        Some(truncate_to(left.wrapping_div(right), &CType::long()))
                    }
                    BinaryOp::Remainder if right != 0 => {
                        Some(truncate_to(left.wrapping_rem(right), &CType::long()))
                    }
                    BinaryOp::BitAnd => Some(left & right),
                    BinaryOp::BitOr => Some(left | right),
                    BinaryOp::BitXor => Some(left ^ right),
                    _ => None,
                }
            }
            Expression::Not(_)
            | Expression::Minus(_)
            | Expression::Plus(_)
            | Expression::BitNot(_) => None,
            Expression::Logical { .. } => None,
            Expression::Name { .. } | Expression::String { .. } => None,
            _ => None,
        }
    }

    /// The type and value of an integer constant, following C's rules.
    ///
    /// The magnitude is read once and used twice — for the type the constant *earns*
    /// and for the bits it *carries* — because a frontend that derives the type from
    /// one reading of the digits and the value from another has two chances to be
    /// wrong, and did.
    fn integer_constant(&mut self, number: &crate::lexer::Number, span: SourceSpan) -> CType {
        let magnitude = self.integer_magnitude(number, &span);
        let (bits, signed) = self.constant_type(number, magnitude);
        CType::Int { bits, signed }
    }

    /// A literal's magnitude as a `u64`, with a diagnostic if it is not one.
    ///
    /// The value is read as unsigned because that is what a constant's digits are:
    /// `18446744073709551615ul` is a perfectly ordinary constant, and reading its
    /// digits as a *signed* 64-bit number is what used to make it fail.
    fn integer_magnitude(&mut self, number: &crate::lexer::Number, span: &SourceSpan) -> u64 {
        if number.digits.is_empty() {
            return 0;
        }
        match u64::from_str_radix(&number.digits, number.base) {
            Ok(magnitude) => magnitude,
            Err(_) => {
                self.error(
                    span.clone(),
                    codes::CONSTANT_TOO_LARGE,
                    alloc::format!(
                        "`{}` is not a constant this machine can hold",
                        number.digits
                    ),
                    "use a smaller value, or a type wide enough for it",
                );
                0
            }
        }
    }

    fn integer_value(&mut self, number: &crate::lexer::Number, span: SourceSpan) -> Option<i64> {
        if number.digits.is_empty() {
            return Some(0);
        }
        let magnitude = self.integer_magnitude(number, &span);
        // C computes a constant in the type the *value* earns it, and only refuses
        // one that has no type at all. The type is therefore worked out from the
        // magnitude — the same ladder `constant_type` walks — and a constant is
        // refused only when the type that ladder picked cannot hold it. That last
        // case is a decimal constant too large for `long`, which C says has no type,
        // and a `u`/`ul` constant too large for its type, which cannot be written.
        //
        // This check used to work out the type from a *zero* value, which made every
        // unsuffixed constant look like a small `int` and so refused anything above
        // `INT_MAX` — including `5000000000`, which C gives type `long`, and
        // `0x80000000`, which it gives type `unsigned int`. Both are legal C and both
        // were rejected.
        let (bits, signed) = self.constant_type(number, magnitude.min(i64::MAX as u64));
        // A 64-bit type has no limit above `u64::MAX`, so there is nothing to
        // compare against, and `1u64 << 64` does not exist. The width is checked
        // before the shift rather than after: computing it unconditionally meant
        // every unsigned-long constant panicked this function in a debug build,
        // `1ul` as much as `18446744073709551615ul`.
        if bits < 64 {
            let limit = if signed {
                1u64 << (bits - 1)
            } else {
                1u64 << bits
            };
            if magnitude >= limit {
                self.error(
                    span,
                    codes::CONSTANT_TOO_LARGE,
                    alloc::format!(
                        "`{}` does not fit in a {}",
                        number.digits,
                        signed_type_name(bits, signed)
                    ),
                    "give it a wider type with a suffix, or use a smaller value",
                );
                return None;
            }
        } else if signed && magnitude > i64::MAX as u64 {
            // A *signed* 64-bit type does have a limit, and it is `LONG_MAX`: a
            // decimal constant above it has no type at all, which C says and this
            // reports. (An *unsigned* long above `LONG_MAX` is perfectly ordinary —
            // the IR carries the same 64 bits either way, and the caller treats them
            // as unsigned.)
            self.error(
                span,
                codes::CONSTANT_TOO_LARGE,
                alloc::format!("`{}` does not fit in a `long`", number.digits),
                "give it a `u` suffix for an `unsigned long`",
            );
            return None;
        }
        Some(magnitude as i64)
    }

    /// C's rule for an integer constant's type, from its value, base and suffix.
    ///
    /// The value is the constant's magnitude, clamped to what fits an `i64` so that
    /// the comparisons below are total: a magnitude above `LONG_MAX` is larger than
    /// every threshold this function tests, which is the answer it wants, and
    /// clamping cannot change that.
    fn constant_type(&self, number: &crate::lexer::Number, magnitude: u64) -> (u16, bool) {
        let unsigned = number.suffix.contains('u');
        let long = number.suffix.contains('l');
        let decimal = number.base == 10;
        if unsigned {
            return if long { (64, false) } else { (32, false) };
        }
        if long {
            // A `long` literal, or one too big for an `int`, is a `long`.
            return (64, true);
        }
        if magnitude < 1 << 31 {
            return (32, true);
        }
        // An octal or hexadecimal constant that does not fit an `int` gets an
        // `unsigned int`, and one that does not fit that gets an `unsigned long`.
        // A *decimal* one does not: a decimal constant too large for `long` has
        // no type at all, which C says and this reports.
        if !decimal {
            if magnitude < 1 << 32 {
                return (32, false);
            }
            return (64, false);
        }
        (64, true)
    }
}

/// The name of an integer type with a stated width and signedness.
fn signed_type_name(bits: u16, signed: bool) -> String {
    match (bits, signed) {
        (8, false) => String::from("`unsigned char`"),
        (8, true) => String::from("`signed char`"),
        (16, false) => String::from("`unsigned short`"),
        (16, true) => String::from("`short`"),
        (32, false) => String::from("`unsigned int`"),
        (32, true) => String::from("`int`"),
        (64, false) => String::from("`unsigned long`"),
        (64, true) => String::from("`long`"),
        _ => String::from("integer of that width"),
    }
}

/// Truncates a value to a type's width, as a conversion to it would.
fn truncate_to(value: i64, ty: &CType) -> i64 {
    let (bits, signed) = match ty {
        CType::Int { bits, signed } => (*bits, *signed),
        _ => return value,
    };
    if bits >= 64 {
        return value;
    }
    let mask = (1i64 << bits) - 1;
    let truncated = value & mask;
    if signed && truncated >= 1i64 << (bits - 1) {
        truncated - (1i64 << bits)
    } else {
        truncated
    }
}

/// An expression's own span, for a diagnostic about it.
pub fn expression_span(expression: &Expression) -> SourceSpan {
    match expression {
        Expression::Name { span, .. }
        | Expression::Integer { span, .. }
        | Expression::Character { span, .. }
        | Expression::String { span, .. }
        | Expression::Member { span, .. } => span.clone(),
        Expression::SizeofType(name) => name.span.clone(),
        Expression::SizeofExpression(inner) => expression_span(inner),
        Expression::Group(inner) => expression_span(inner),
        Expression::Address(inner)
        | Expression::Dereference(inner)
        | Expression::Plus(inner)
        | Expression::Minus(inner)
        | Expression::BitNot(inner)
        | Expression::Not(inner) => expression_span(inner),
        Expression::Increment { operand, .. } => expression_span(operand),
        Expression::Binary { left, .. }
        | Expression::Logical { left, .. }
        | Expression::Comma { left, .. } => expression_span(left),
        Expression::Conditional { condition, .. } => expression_span(condition),
        Expression::Call { callee, .. } => expression_span(callee),
        Expression::Subscript { array, .. } => expression_span(array),
        Expression::CompoundAssign { target, .. } | Expression::Assign { target, .. } => {
            expression_span(target)
        }
        Expression::Cast { ty, .. } => ty.span.clone(),
    }
}

/// A condition expression's span.
pub fn condition_span(expression: &Expression) -> SourceSpan {
    expression_span(expression)
}

/// A `case` value's span.
pub fn value_span(expression: &Expression) -> SourceSpan {
    expression_span(expression)
}

/// An initialiser item's span.
pub fn item_span(item: &InitItem) -> SourceSpan {
    match &item.designator {
        Some(Designator::Field { span, .. }) | Some(Designator::Index { span, .. }) => span.clone(),
        None => match &*item.value {
            Initializer::Scalar(expression) => expression_span(expression),
            Initializer::List { span, .. } => span.clone(),
        },
    }
}

/// Rounds a value up to a whole number of words, for a frame size.
pub fn round_up_word(value: u32) -> u32 {
    ctypes::align_up(value, 8)
}

/// The C signature of a call the OS ABI provides.
///
/// A C program reaches the machine's facilities through its syscalls, and a call
/// to one of them has to *check*: an arity check and a type check are the only
/// things standing between a program's mistake and a kernel reading a number
/// where it expected an address.
///
/// The signatures below are the ABI's, taken from `docs/os-abi.md`, and they are
/// stated here rather than guessed from the names. A name the ABI has but whose
/// signature the ABI has not stated is accepted with *any* arguments and a
/// `long` result: refusing it would make a facility unavailable, and inventing a
/// signature would be worse than declining to check it. The distinction is
/// deliberate, and it is the one place this compiler declines to check rather
/// than refuses.
pub fn abi_signature(name: &str) -> Option<CType> {
    let byte_pointer = CType::pointer_to(CType::Int {
        bits: 8,
        signed: true,
    });
    // Every signature below is `docs/os-abi.md`'s, argument for argument. They are
    // not inferred from the names: `write` takes an `IoResult` to report into, and
    // a C program that passed three arguments would leave the fourth register
    // holding whatever it held before — which reads as a write that wrote nothing.
    let io_result = CType::pointer_to(CType::Int {
        bits: 32,
        signed: true,
    });
    let params = match name {
        "exit" => vec![CType::int()],
        "write" | "read" => vec![
            CType::int(),
            byte_pointer.clone(),
            CType::ulong(),
            io_result.clone(),
            CType::uint(),
        ],
        // The fourth argument is where the handle goes, not a mode. A `u32 *`
        // out-parameter, like every other call that has something to report.
        "open" => vec![
            byte_pointer.clone(),
            CType::ulong(),
            CType::uint(),
            CType::pointer_to(CType::uint()),
        ],
        "close" => vec![CType::int()],
        "seek" => vec![
            CType::int(),
            CType::long(),
            CType::int(),
            CType::pointer_to(CType::long()),
        ],
        "stat" => vec![byte_pointer.clone(), CType::ulong(), byte_pointer.clone()],
        "list_directory" => vec![
            byte_pointer.clone(),
            CType::ulong(),
            byte_pointer.clone(),
            CType::ulong(),
            io_result.clone(),
        ],
        "time" => vec![CType::pointer_to(CType::long())],
        "sleep" => vec![CType::ulong()],
        "allocate_memory" => vec![CType::ulong(), CType::ulong(), byte_pointer.clone()],
        "spawn_process" => vec![
            byte_pointer.clone(),
            CType::ulong(),
            byte_pointer.clone(),
            CType::ulong(),
            CType::pointer_to(CType::int()),
        ],
        "wait_process" => vec![CType::int(), CType::pointer_to(CType::int())],
        "clear_screen" => Vec::new(),
        "input_poll" => vec![byte_pointer.clone(), CType::ulong(), io_result.clone()],
        "display_open" => vec![
            CType::uint(),
            CType::uint(),
            byte_pointer.clone(),
            byte_pointer.clone(),
        ],
        "display_present" => vec![
            byte_pointer.clone(),
            CType::ulong(),
            CType::ulong(),
            CType::uint(),
        ],
        // A name the ABI has numbered but whose C signature it has not stated.
        _ => return unchecked_abi_signature(name),
    };
    Some(CType::Function(Box::new(FunctionType {
        result: CType::long(),
        params,
        variadic: false,
        names: Vec::new(),
    })))
}

/// A signature for an ABI call this compiler does not check.
fn unchecked_abi_signature(name: &str) -> Option<CType> {
    lazalith_os_abi::abi_syscall(name)?;
    Some(CType::Function(Box::new(FunctionType {
        result: CType::long(),
        params: Vec::new(),
        // Variadic, because an unchecked call may pass any arguments at all and
        // saying "no parameters" would be a check this stage has declined to do.
        variadic: true,
        names: Vec::new(),
    })))
}

/// The C signature of a function the standard library provides.
///
/// A C library function is not a syscall. `write` is a syscall and `printf` is
/// not, and a compiler that lowered both the same way would be claiming the
/// kernel has a `printf`. So the two are separate tables, and a call resolves
/// through the library first and the ABI second.
///
/// The signatures here are the runtime's, from `lazalith-c-runtime`, and they are
/// C's own: `size_t` is `unsigned long`, a null pointer is written as `0`, and a
/// function that can fail returns a null pointer or `-1` rather than a status
/// the caller has to check twice.
pub fn library_signature(name: &str) -> Option<CType> {
    let byte_pointer = CType::pointer_to(CType::Int {
        bits: 8,
        signed: true,
    });
    let void_pointer = CType::Pointer(Box::new(CType::Void));
    let string = |params: Vec<CType>| {
        Some(CType::Function(Box::new(FunctionType {
            result: CType::long(),
            params,
            variadic: false,
            names: Vec::new(),
        })))
    };
    let returns = |result: CType, params: Vec<CType>, variadic: bool| {
        Some(CType::Function(Box::new(FunctionType {
            result,
            params,
            variadic,
            names: Vec::new(),
        })))
    };
    match name {
        // <string.h>
        "strlen" => string(vec![byte_pointer.clone()]),
        // `memcmp`'s third argument is a *length* in C, not an end pointer, and
        // the string functions' third argument is a length too — so all three
        // have the same shape here. `memcmp` takes `const void *` and the string
        // functions `const char *`; the ABI is a word either way.
        "strcmp" | "strncmp" | "memcmp" => string(vec![
            byte_pointer.clone(),
            byte_pointer.clone(),
            CType::ulong(),
        ]),
        "strcpy" | "strcat" | "strchr" | "strrchr" | "strstr" => {
            string(vec![byte_pointer.clone(), byte_pointer.clone()])
        }
        "strncpy" | "strncat" => string(vec![
            byte_pointer.clone(),
            byte_pointer.clone(),
            CType::ulong(),
        ]),
        "memcpy" | "memmove" => string(vec![
            void_pointer.clone(),
            void_pointer.clone(),
            CType::ulong(),
        ]),
        "memset" => string(vec![void_pointer.clone(), CType::int(), CType::ulong()]),
        // <stdlib.h>
        "malloc" => returns(byte_pointer.clone(), vec![CType::ulong()], false),
        "calloc" => returns(
            byte_pointer.clone(),
            vec![CType::ulong(), CType::ulong()],
            false,
        ),
        "free" => returns(CType::Void, vec![void_pointer.clone()], false),
        // `exit` is deliberately absent. It is an ABI syscall, and the compiler asks the
        // library table *before* the ABI, so a runtime wrapper of the same name would
        // win — and the wrapper cannot exit, because there is no other exit.
        // `abort` has no ABI call to shadow, so it is a runtime function and is here.
        "abort" => returns(CType::Void, vec![CType::int()], false),
        "atoi" => returns(CType::int(), vec![byte_pointer.clone()], false),
        "abs" | "labs" => returns(
            if name == "abs" {
                CType::int()
            } else {
                CType::long()
            },
            vec![if name == "abs" {
                CType::int()
            } else {
                CType::long()
            }],
            false,
        ),
        // <stdio.h>
        "puts" => returns(CType::int(), vec![byte_pointer.clone()], false),
        "putchar" => returns(CType::int(), vec![CType::int()], false),
        // A file handle is an `int` throughout, not a `FILE *`, and there is no `fopen`
        // at all: the ABI reports a new file's handle in a return register no
        // calling convention hands a caller. `docs/c-runtime.md` says so in full.
        "fputs" => returns(
            CType::int(),
            vec![byte_pointer.clone(), CType::int()],
            false,
        ),
        "fwrite" => returns(
            CType::ulong(),
            vec![
                void_pointer.clone(),
                CType::ulong(),
                CType::ulong(),
                CType::int(),
            ],
            false,
        ),
        "fread" => returns(
            CType::ulong(),
            vec![
                void_pointer.clone(),
                CType::ulong(),
                CType::ulong(),
                CType::int(),
            ],
            false,
        ),
        "fseek" => returns(
            CType::int(),
            vec![CType::int(), CType::long(), CType::int()],
            false,
        ),
        "ftell" => returns(CType::long(), vec![CType::int()], false),
        "fclose" => returns(CType::int(), vec![CType::int()], false),
        "fflush" => returns(CType::int(), vec![CType::int()], false),
        _ => None,
    }
}
