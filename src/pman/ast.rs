use crate::pman::token::Span;

/// A complete .pman file.
#[derive(Debug)]
pub struct File {
    pub config: Option<ConfigBlock>,
    pub jobs: Vec<JobDef>,
    pub events: Vec<EventDef>,
}

#[derive(Debug)]
pub struct ConfigBlock {
    pub logs: Option<StringLit>,
    pub env: Vec<EnvBinding>,
    pub args: Vec<ArgDef>,
    pub span: Span,
}

#[derive(Debug)]
pub struct ArgDef {
    pub name: String,
    pub arg_type: Option<ArgType>,
    pub default: Option<Expr>,
    pub short: Option<StringLit>,
    pub description: Option<StringLit>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArgType {
    String,
    Bool,
}

#[derive(Debug)]
pub struct JobDef {
    pub name: String,
    pub condition: Option<Expr>,
    pub body: JobBody,
    #[allow(dead_code)]
    pub span: Span,
}

#[derive(Debug)]
pub struct EventDef {
    pub name: String,
    pub body: JobBody,
    #[allow(dead_code)]
    pub span: Span,
}

#[derive(Debug)]
pub struct JobBody {
    pub once: Option<bool>,
    pub env: Vec<EnvBinding>,
    pub wait: Option<WaitBlock>,
    pub watches: Vec<WatchDef>,
    pub run_section: RunSection,
}

/// Either a direct run command or a for-loop wrapping env+run.
#[derive(Debug)]
pub enum RunSection {
    Direct(ShellBlock),
    ForLoop(Box<ForLoop>),
}

#[derive(Debug)]
pub struct ForLoop {
    pub var: String,
    pub iterable: Iterable,
    pub env: Vec<EnvBinding>,
    pub run: ShellBlock,
    #[allow(dead_code)]
    pub span: Span,
}

#[derive(Debug)]
pub enum Iterable {
    Glob(StringLit),
    Array(Vec<Expr>),
    RangeExclusive(Expr, Expr),
    RangeInclusive(Expr, Expr),
}

#[derive(Debug)]
pub struct EnvBinding {
    pub key: String,
    pub value: Expr,
    #[allow(dead_code)]
    pub span: Span,
}

#[derive(Debug)]
pub enum ShellBlock {
    Inline(StringLit),
    Fenced(String, #[allow(dead_code)] Span),
}

#[derive(Debug)]
pub struct StringLit {
    pub value: String,
    #[allow(dead_code)]
    pub span: Span,
}

/// Wait block — ordered list of conditions.
#[derive(Debug)]
pub struct WaitBlock {
    pub conditions: Vec<WaitCondition>,
    #[allow(dead_code)]
    pub span: Span,
}

#[derive(Debug)]
pub struct WaitCondition {
    pub negated: bool,
    pub kind: ConditionKind,
    pub options: ConditionOptions,
    #[allow(dead_code)]
    pub span: Span,
}

#[derive(Debug)]
pub enum ConditionKind {
    After {
        job: String,
    },
    Http {
        url: StringLit,
    },
    Connect {
        address: StringLit,
    },
    Exists {
        path: StringLit,
    },
    Running {
        pattern: StringLit,
    },
    Contains {
        path: StringLit,
        format: String,
        key: StringLit,
        var: Option<String>,
    },
}

#[derive(Debug, Default)]
pub struct ConditionOptions {
    pub status: Option<Expr>,
    pub timeout: Option<Expr>,
    pub poll: Option<Expr>,
    pub retry: Option<Expr>,
}

#[derive(Debug)]
pub struct WatchDef {
    pub name: String,
    pub condition: WaitCondition,
    pub initial_delay: Option<Expr>,
    pub poll: Option<Expr>,
    pub threshold: Option<Expr>,
    pub on_fail: Option<OnFailAction>,
    #[allow(dead_code)]
    pub span: Span,
}

#[derive(Debug)]
pub enum OnFailAction {
    Shutdown,
    Debug,
    Log,
    Spawn(String),
}

/// Expression — evaluated at runtime.
#[derive(Debug, Clone)]
pub enum Expr {
    StringLit(String, Span),
    NumberLit(f64, Span),
    BoolLit(bool, Span),
    DurationLit(f64, Span),
    NoneLit(Span),
    ArgsRef(String, Span),              // args.name
    JobOutputRef(String, String, Span), // @job.KEY
    LocalVar(String, Span),             // bare identifier in expression context
    BinOp(Box<Expr>, BinOp, Box<Expr>, Span),
    UnaryNot(Box<Expr>, Span),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}
