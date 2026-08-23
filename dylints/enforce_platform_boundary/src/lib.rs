#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_span;

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::marker::PhantomData;
use std::sync::{LazyLock, Mutex};

use rustc_ast::token::{LitKind, TokenKind};
use rustc_ast::tokenstream::{TokenStream, TokenTree};
use rustc_ast::visit::{self, Visitor};
use rustc_ast::{
    Attribute, Item, ItemKind, MacCall, MetaItem, MetaItemInner, MetaItemKind, Path, UseTree,
};
use rustc_errors::DiagDecorator;
use rustc_lint::{EarlyContext, EarlyLintPass, LintContext};
use rustc_session::Session;
use rustc_span::{FileName, RemapPathScopeComponents, Span};

dylint_linting::declare_pre_expansion_lint! {
    /// Enforces the single `fbuild_core::platform` host-selection boundary.
    ///
    /// The pass runs before expansion so inactive host branches, private
    /// items, macro tokens, and inline tests are checked. Existing debt is
    /// admitted only by `baseline.txt`; a second identical occurrence in a
    /// grandfathered file exceeds the recorded count and fails.
    pub ENFORCE_PLATFORM_BOUNDARY,
    Deny,
    "confine host-platform selection and native APIs to fbuild_core::platform"
}

const HOST_KEYS: &[&str] = &[
    "windows",
    "unix",
    "target_abi",
    "target_arch",
    "target_endian",
    "target_env",
    "target_family",
    "target_feature",
    "target_os",
    "target_pointer_width",
    "target_vendor",
];
const NATIVE_ROOTS: &[&str] = &[
    "interprocess",
    "libc",
    "mach2",
    "nix",
    "portable_pty",
    "socket2",
    "winapi",
    "windows",
    "windows_sys",
];
const CONCRETE_MODULES: &[&str] = &[
    "platform_imp",
    "platform_win",
    "platform_windows",
    "platform_linux",
    "platform_macos",
];
const BASELINE_TEXT: &str = include_str!("baseline.txt");
const OBSERVATION_PATH_ENV: &str = "FBUILD_PLATFORM_BOUNDARY_OBSERVED";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    AttrCfg,
    CfgMacro,
    CompileHostFact,
    NativeImport,
    ModuleRef,
}

impl Kind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::AttrCfg => "attr_cfg",
            Self::CfgMacro => "cfg_macro",
            Self::CompileHostFact => "compile_host_fact",
            Self::NativeImport => "native_import",
            Self::ModuleRef => "module_ref",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Selector,
    Concrete,
    Facade,
    Ui,
    Production,
    OutOfScope,
}

type Key = (String, String, String);

struct Baseline {
    counts: HashMap<Key, u32>,
}

impl Baseline {
    fn parse(text: &str) -> Self {
        let mut counts = HashMap::new();
        for line in text
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() == 4 {
                let key = (
                    fields[0].to_owned(),
                    fields[1].to_owned(),
                    fields[2].to_owned(),
                );
                *counts.entry(key).or_insert(0) += 1;
            }
        }
        Self { counts }
    }
}

static BASELINE: LazyLock<Baseline> = LazyLock::new(|| Baseline::parse(BASELINE_TEXT));
static COUNTS: LazyLock<Mutex<HashMap<Key, u32>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static OBSERVED_SOURCES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Identity stamped on every observation line so the checker can tell one
/// driver process's findings from another's.
///
/// A bare PID is not that identity. `cargo dylint --all-targets` runs one
/// driver process per crate-target, so a crate's lib and lib-test targets
/// compile the same sources twice — and Windows reuses PIDs aggressively
/// enough that the second process can be handed the first one's number once
/// it has exited. The two runs' findings then merge under a single key and
/// every count for the shared sources reads exactly double, which surfaced
/// as `expected={('attr_cfg', 'windows'): 6} actual={...: 12}` on
/// `fbuild-toolchain` while the Linux leg of the same commit was green.
///
/// Appending a nanosecond stamp taken once per process separates them:
/// reusing a PID requires the original holder to have exited first, so the
/// two processes cannot have started in the same nanosecond.
static PROCESS_IDENTITY: LazyLock<String> = LazyLock::new(|| {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nonce:x}", std::process::id())
});

fn append_observation(line: &str) {
    let Some(path) = std::env::var(OBSERVATION_PATH_ENV)
        .ok()
        .or_else(|| option_env!("FBUILD_PLATFORM_BOUNDARY_OBSERVED").map(str::to_owned))
    else {
        return;
    };
    let Ok(mut output) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = output.write_all(line.as_bytes());
}

fn observe_source(context: &EarlyContext<'_>, span: Span) {
    let Some(path) =
        source_filename(context.sess(), span).and_then(|filename| repo_relative_path(&filename))
    else {
        return;
    };
    if classify(&path) != Scope::Production {
        return;
    }
    let mut sources = OBSERVED_SOURCES
        .lock()
        .expect("platform lint source counter poisoned");
    if sources.insert(path.clone()) {
        append_observation(&format!("{}\t{path}\tsource_seen\t-\n", *PROCESS_IDENTITY));
    }
}

fn exceeds_baseline(
    baseline: &HashMap<Key, u32>,
    counts: &mut HashMap<Key, u32>,
    key: &Key,
) -> bool {
    let ordinal = counts.get(key).copied().unwrap_or(0);
    counts.insert(key.clone(), ordinal + 1);
    ordinal >= baseline.get(key).copied().unwrap_or(0)
}

fn classify(path: &str) -> Scope {
    if path.starts_with("ui/")
        || path.starts_with("./ui/")
        || path.starts_with("dylints/enforce_platform_boundary/ui/")
    {
        return Scope::Ui;
    }
    if ["windows", "linux", "macos"]
        .iter()
        .any(|host| path.starts_with(&format!("crates/fbuild-core/src/platform/{host}/")))
    {
        return Scope::Concrete;
    }
    if path == "crates/fbuild-core/src/platform/mod.rs" {
        return Scope::Selector;
    }
    if path.starts_with("crates/fbuild-core/src/platform/") {
        return Scope::Facade;
    }
    if path.starts_with("crates/") {
        return Scope::Production;
    }
    Scope::OutOfScope
}

fn repo_relative_path(filename: &str) -> Option<String> {
    let normalized = filename.replace('\\', "/").replace("/./", "/");
    for marker in ["crates/", "dylints/"] {
        if let Some(index) = normalized.rfind(marker) {
            return Some(normalized[index..].to_owned());
        }
    }
    if let Some(relative) = normalized.strip_prefix("./ui/") {
        return Some(format!("ui/{relative}"));
    }
    normalized.starts_with("ui/").then_some(normalized)
}

fn source_filename(session: &Session, span: Span) -> Option<String> {
    match session.source_map().span_to_filename(span) {
        FileName::Real(real) => Some(
            real.local_path()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| {
                    real.path(RemapPathScopeComponents::DIAGNOSTICS)
                        .to_string_lossy()
                        .into_owned()
                }),
        ),
        name => Some(
            name.display(RemapPathScopeComponents::DIAGNOSTICS)
                .to_string(),
        ),
    }
}

fn record(context: &EarlyContext<'_>, span: Span, kind: Kind, normalized: &str) {
    let Some(path) =
        source_filename(context.sess(), span).and_then(|filename| repo_relative_path(&filename))
    else {
        return;
    };
    match classify(&path) {
        Scope::Selector | Scope::Concrete | Scope::OutOfScope => return,
        Scope::Facade
            if path == "crates/fbuild-core/src/platform/executable.rs"
                && kind == Kind::NativeImport
                && normalized == "std::env::current_exe" =>
        {
            return;
        }
        Scope::Facade | Scope::Ui => {
            emit(context, span, kind, normalized);
            return;
        }
        Scope::Production => {}
    }
    let key = (path, kind.as_str().to_owned(), normalized.to_owned());
    append_observation(&format!(
        "{}\t{}\t{}\t{}\n",
        *PROCESS_IDENTITY, key.0, key.1, key.2
    ));
    let mut counts = COUNTS.lock().expect("platform lint counter poisoned");
    if exceeds_baseline(&BASELINE.counts, &mut counts, &key) {
        emit(context, span, kind, normalized);
    }
}

fn emit(context: &EarlyContext<'_>, span: Span, kind: Kind, normalized: &str) {
    let construct = kind.as_str().replace('_', " ");
    let message = format!(
        "{construct} `{normalized}` is outside fbuild_core::platform; use a neutral facade"
    );
    context.opt_span_lint(
        ENFORCE_PLATFORM_BOUNDARY,
        Some(span),
        DiagDecorator(move |diagnostic| {
            diagnostic.primary_message(message);
        }),
    );
}

struct State<'a> {
    context: &'a EarlyContext<'a>,
}

impl State<'_> {
    fn check_meta(&self, meta: &MetaItem) {
        match &meta.kind {
            MetaItemKind::Word | MetaItemKind::NameValue(_) => {
                if let Some(segment) = meta.path.segments.first() {
                    let name = segment.ident.name.as_str();
                    if HOST_KEYS.contains(&name) {
                        record(self.context, meta.span, Kind::AttrCfg, name);
                    }
                }
            }
            MetaItemKind::List(items) => {
                for item in items {
                    if let MetaItemInner::MetaItem(nested) = item {
                        self.check_meta(nested);
                    }
                }
            }
        }
    }

    fn check_tokens(&self, span: Span, tokens: &TokenStream, kind: Kind) {
        for tree in tokens.iter() {
            match tree {
                TokenTree::Token(token, _) => match token.kind {
                    TokenKind::Ident(name, _) if HOST_KEYS.contains(&name.as_str()) => {
                        record(self.context, span, kind, name.as_str());
                    }
                    TokenKind::Literal(literal)
                        if matches!(literal.kind, LitKind::Str | LitKind::StrRaw(_)) =>
                    {
                        let value = literal.symbol.as_str();
                        if value.starts_with("CARGO_CFG_TARGET_") {
                            record(self.context, span, Kind::CompileHostFact, value);
                        }
                    }
                    _ => {}
                },
                TokenTree::Delimited(_, _, _, inner) => self.check_tokens(span, inner, kind),
            }
        }
    }

    fn check_macro(&self, span: Span, name: &str, tokens: &TokenStream) {
        match name {
            "cfg" | "cfg_if" | "cfg_select" => {
                self.check_tokens(span, tokens, Kind::CfgMacro);
            }
            "env" | "option_env" => {
                self.check_tokens(span, tokens, Kind::CompileHostFact);
            }
            _ => self.check_unexpanded_tokens(span, tokens),
        }
    }

    fn check_unexpanded_tokens(&self, span: Span, tokens: &TokenStream) {
        let trees: Vec<&TokenTree> = tokens.iter().collect();
        for (index, tree) in trees.iter().enumerate() {
            let TokenTree::Token(token, _) = tree else {
                if let TokenTree::Delimited(_, _, _, inner) = tree {
                    self.check_unexpanded_tokens(span, inner);
                }
                continue;
            };
            let TokenKind::Ident(name, _) = token.kind else {
                continue;
            };

            if matches!(trees.get(index + 1), Some(TokenTree::Token(next, _)) if next.kind == TokenKind::Bang)
            {
                if let Some(TokenTree::Delimited(_, _, _, inner)) = trees.get(index + 2) {
                    match name.as_str() {
                        "cfg" | "cfg_if" | "cfg_select" => {
                            self.check_tokens(span, inner, Kind::CfgMacro);
                        }
                        "env" | "option_env" => {
                            self.check_tokens(span, inner, Kind::CompileHostFact);
                        }
                        _ => {}
                    }
                }
            }

            if index > 0
                && matches!(trees.get(index - 1), Some(TokenTree::Token(previous, _)) if previous.kind == TokenKind::PathSep)
            {
                continue;
            }
            let mut segments = vec![name.as_str().to_owned()];
            let mut cursor = index + 1;
            while matches!(trees.get(cursor), Some(TokenTree::Token(separator, _)) if separator.kind == TokenKind::PathSep)
            {
                let Some(TokenTree::Token(next, _)) = trees.get(cursor + 1) else {
                    break;
                };
                let TokenKind::Ident(next_name, _) = next.kind else {
                    break;
                };
                segments.push(next_name.as_str().to_owned());
                cursor += 2;
            }
            self.check_path_segments(span, &segments);
        }
    }

    fn check_path_segments(&self, span: Span, segments: &[String]) {
        if segments.len() >= 3 && segments[..3] == ["std", "env", "current_exe"] {
            record(
                self.context,
                span,
                Kind::NativeImport,
                "std::env::current_exe",
            );
            return;
        }
        if segments.len() >= 4
            && segments[..3] == ["std", "env", "consts"]
            && matches!(segments[3].as_str(), "OS" | "ARCH")
        {
            record(
                self.context,
                span,
                Kind::CompileHostFact,
                &segments[..4].join("::"),
            );
            return;
        }
        if segments.len() >= 3
            && segments[0] == "std"
            && segments[1] == "os"
            && matches!(segments[2].as_str(), "windows" | "unix" | "linux" | "macos")
        {
            record(
                self.context,
                span,
                Kind::NativeImport,
                &segments[..3].join("::"),
            );
            return;
        }
        if let Some(root) = segments.first() {
            if segments.len() >= 2 && NATIVE_ROOTS.contains(&root.as_str()) {
                record(self.context, span, Kind::NativeImport, root);
                return;
            }
            if root == "tokio"
                && segments.get(1).is_some_and(|segment| segment == "net")
                && segments.iter().any(|segment| {
                    matches!(segment.as_str(), "windows" | "UnixListener" | "UnixStream")
                })
            {
                record(self.context, span, Kind::NativeImport, root);
                return;
            }
        }
        for concrete in CONCRETE_MODULES {
            if segments.iter().any(|segment| segment == concrete) {
                record(self.context, span, Kind::ModuleRef, concrete);
                return;
            }
        }
    }

    fn check_path(&self, path: &Path) {
        let segments: Vec<String> = path
            .segments
            .iter()
            .map(|segment| segment.ident.name.as_str().to_owned())
            .collect();
        self.check_path_segments(path.span, &segments);
    }
}

struct PathScanner<'a, 'ast> {
    context: &'a EarlyContext<'a>,
    marker: PhantomData<&'ast ()>,
}

impl<'a, 'ast> Visitor<'ast> for PathScanner<'a, 'ast> {
    fn visit_item(&mut self, _item: &'ast Item) {}

    fn visit_use_tree(&mut self, use_tree: &'ast UseTree) {
        let segments = &use_tree.prefix.segments;
        if segments.len() == 1 {
            let root = segments[0].ident.name.as_str();
            if NATIVE_ROOTS.contains(&root) {
                record(self.context, use_tree.span(), Kind::NativeImport, root);
            }
        }
        visit::walk_use_tree(self, use_tree);
    }

    fn visit_path(&mut self, path: &'ast Path) {
        State {
            context: self.context,
        }
        .check_path(path);
    }
}

impl EarlyLintPass for EnforcePlatformBoundary {
    fn check_attribute(&mut self, context: &EarlyContext<'_>, attribute: &Attribute) {
        observe_source(context, attribute.span);
        if let Some(meta) = attribute.meta() {
            State { context }.check_meta(&meta);
        }
    }

    fn check_item(&mut self, context: &EarlyContext<'_>, item: &Item) {
        observe_source(context, item.span);
        if let ItemKind::ExternCrate(original, identifier) = &item.kind {
            for name in [
                original.as_ref().map(|symbol| symbol.as_str()),
                Some(identifier.name.as_str()),
            ]
            .into_iter()
            .flatten()
            {
                if NATIVE_ROOTS.contains(&name) {
                    record(context, item.span, Kind::NativeImport, name);
                }
            }
        }
        visit::walk_item(
            &mut PathScanner {
                context,
                marker: PhantomData,
            },
            item,
        );
    }

    fn check_mac(&mut self, context: &EarlyContext<'_>, mac: &MacCall) {
        observe_source(context, mac.span());
        if let Some(first) = mac.path.segments.first() {
            State { context }.check_macro(mac.span(), first.ident.name.as_str(), &mac.args.tokens);
        }
    }
}

#[test]
fn second_identical_occurrence_exceeds_exact_baseline() {
    let key = (
        "crates/example/src/lib.rs".to_owned(),
        "cfg_macro".to_owned(),
        "windows".to_owned(),
    );
    let baseline = HashMap::from([(key.clone(), 1)]);
    let mut counts = HashMap::new();

    assert!(!exceeds_baseline(&baseline, &mut counts, &key));
    assert!(exceeds_baseline(&baseline, &mut counts, &key));
}
