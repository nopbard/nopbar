//! Follow the production module tree; test-only fixtures never enter this scan.
use std::{collections::HashSet, path::{Path, PathBuf}};
use syn::visit::Visit;

struct Scan {
    root: PathBuf,
    file: PathBuf,
    module_dir: PathBuf,
    path_base: PathBuf,
    seen: HashSet<PathBuf>,
    data: std::collections::HashMap<String, Vec<String>>,
    errors: Vec<String>,
}

fn test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("test") || a.path().is_ident("cfg")
        && a.parse_args::<syn::Path>().is_ok_and(|p| p.is_ident("test")))
}

impl Scan {
    fn file(&mut self, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if !self.seen.insert(path.clone()) { return; }
        let source = std::fs::read_to_string(&path).unwrap();
        let tree = syn::parse_file(&source).unwrap();
        let parent = path.parent().unwrap().to_path_buf();
        let stem = path.file_stem().unwrap().to_str().unwrap();
        let dir = if matches!(stem, "mod" | "main" | "lib") { parent.clone() } else { parent.join(stem) };
        let old = (self.file.clone(), self.module_dir.clone(), self.path_base.clone());
        self.file = path; self.module_dir = dir; self.path_base = parent;
        self.visit_file(&tree);
        (self.file, self.module_dir, self.path_base) = old;
    }

    fn literal(&mut self, value: &str) {
        if !value.chars().any(|c| matches!(c, '\u{1100}'..='\u{11ff}' | '\u{3130}'..='\u{318f}' | '\u{ac00}'..='\u{d7a3}')) { return; }
        let file = self.file.strip_prefix(&self.root).unwrap().to_string_lossy().replace('\\', "/");
        if !self.data.get(&file).is_some_and(|values| values.iter().any(|v| v == value)) {
            self.errors.push(format!("{file}: {value:?}"));
        }
    }

    fn tokens(&mut self, tokens: proc_macro2::TokenStream) {
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Group(g) => self.tokens(g.stream()),
                proc_macro2::TokenTree::Literal(l) => {
                    if let Ok(s) = syn::parse_str::<syn::LitStr>(&l.to_string()) { self.literal(&s.value()); }
                }
                _ => {}
            }
        }
    }
}

impl<'a> Visit<'a> for Scan {
    // Documentation attributes contain comments, not UI text.
    fn visit_attribute(&mut self, _: &'a syn::Attribute) {}
    fn visit_item(&mut self, item: &'a syn::Item) {
        let attrs = match item {
            syn::Item::Fn(i) => &i.attrs, syn::Item::Mod(i) => &i.attrs,
            syn::Item::Const(i) => &i.attrs, syn::Item::Static(i) => &i.attrs,
            syn::Item::Impl(i) => &i.attrs, syn::Item::Struct(i) => &i.attrs,
            syn::Item::Enum(i) => &i.attrs, syn::Item::Use(i) => &i.attrs,
            _ => { syn::visit::visit_item(self, item); return; }
        };
        if !test_only(attrs) { syn::visit::visit_item(self, item); }
    }
    fn visit_item_mod(&mut self, module: &'a syn::ItemMod) {
        if test_only(&module.attrs) { return; }
        if let Some((_, items)) = &module.content {
            let old = (self.module_dir.clone(), self.path_base.clone());
            self.module_dir = self.module_dir.join(module.ident.to_string());
            self.path_base = self.module_dir.clone();
            for item in items { self.visit_item(item); }
            (self.module_dir, self.path_base) = old;
        } else {
            let explicit = module.attrs.iter().find_map(|a| {
                if !a.path().is_ident("path") { return None; }
                let syn::Meta::NameValue(n) = &a.meta else { return None; };
                let syn::Expr::Lit(l) = &n.value else { return None; };
                let syn::Lit::Str(s) = &l.lit else { return None; };
                Some(self.path_base.join(s.value()))
            });
            let path = explicit.unwrap_or_else(|| {
                let path = self.module_dir.join(format!("{}.rs", module.ident));
                if path.is_file() { path } else { self.module_dir.join(module.ident.to_string()).join("mod.rs") }
            });
            self.file(&path);
        }
    }
    fn visit_lit_str(&mut self, literal: &'a syn::LitStr) { self.literal(&literal.value()); }
    fn visit_expr_if(&mut self, branch: &'a syn::ExprIf) {
        if matches!(branch.cond.as_ref(), syn::Expr::Macro(m) if m.mac.path.is_ident("cfg") && m.mac.tokens.to_string() == "test") {
            if let Some((_, otherwise)) = &branch.else_branch { self.visit_expr(otherwise); }
        } else { syn::visit::visit_expr_if(self, branch); }
    }
    fn visit_macro(&mut self, mac: &'a syn::Macro) {
        if !mac.path.segments.last().is_some_and(|s| matches!(s.ident.to_string().as_str(), "log" | "println" | "eprintln")) {
            self.tokens(mac.tokens.clone());
        }
    }
}

#[test]
fn production_ui_has_no_korean_literals() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").canonicalize().unwrap();
    let mut scan = Scan { root: root.clone(), file: PathBuf::new(), module_dir: PathBuf::new(), path_base: PathBuf::new(), seen: HashSet::new(), data: serde_json::from_str(include_str!("source_data.json")).unwrap(), errors: Vec::new() };
    scan.file(&root.join("main.rs"));
    assert!(scan.seen.len() > 200, "scan must follow the production module tree");
    assert!(scan.errors.is_empty(), "Korean UI literals must use locale keys:\n{}", scan.errors.join("\n"));
}

#[test]
fn literal_scan_covers_raw_and_nested_macro_strings() {
    let root = PathBuf::from("root");
    let mut scan = Scan { root: root.clone(), file: root.join("sample.rs"), module_dir: root.clone(), path_base: root, seen: HashSet::new(), data: Default::default(), errors: Vec::new() };
    let tree = syn::parse_file("fn ui() { label!(r#\"한국어\"#, nested!(\"라벨\")); } #[cfg(test)] mod tests { const LABEL: &str = \"테스트\"; } fn logs() { crate::log!(\"로그\"); }").unwrap();
    scan.visit_file(&tree);
    assert_eq!(scan.errors.len(), 2);
}
