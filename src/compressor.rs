/// Compresses source files and API graph items into dense semantic representations.
///
/// The goal: maximum information per token. A 400-line struct becomes ~8 lines
/// of pure signal that Copilot can parse in a fraction of the context cost.
use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use quote::quote;
use syn::visit::Visit;
use walkdir::WalkDir;

use crate::model::{ApiGraphItem, CodeMember, CodeUnit};

// ── Public entry points ───────────────────────────────────────────────────────

/// Compress all .rs files under `dir` into CodeUnits.
pub fn compress_dir(dir: &Path) -> Result<Vec<CodeUnit>> {
    let mut units = Vec::new();

    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |x| x == "rs"))
    {
        let path = entry.path();
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let file = match syn::parse_file(&src) {
            Ok(f) => f,
            Err(_) => continue,
        };

        let module_path = derive_module_path(dir, path);
        let mut visitor = CompressVisitor {
            units: Vec::new(),
            members: Vec::new(),
            module_path: module_path.clone(),
            pending_impls: Vec::new(),
        };
        visitor.visit_file(&file);
        visitor.flush_impls();
        units.extend(visitor.units);
    }

    Ok(units)
}

/// Ingest an api-graph.json produced by quartz-ctx into CodeUnits.
/// This avoids re-parsing source when quartz-ctx has already done the work.
pub fn compress_api_graph(items: &[ApiGraphItem]) -> Vec<CodeUnit> {
    items.iter().map(compress_api_item).collect()
}

pub fn compress_api_item(item: &ApiGraphItem) -> CodeUnit {
    let module_path = item.module_path.join("::");
    let compressed = render_compressed_api(item);
    let summary = build_summary_api(item);
    let id = format!("{}::{}", module_path, item.name);

    CodeUnit {
        id,
        kind: item.kind.clone(),
        name: item.name.clone(),
        module_path,
        summary,
        term_vector: build_term_vector_str(&compressed),
        compressed,
        indexed_at: chrono::Utc::now(),
    }
}

// ── Compression renderers ─────────────────────────────────────────────────────

fn render_compressed_api(item: &ApiGraphItem) -> String {
    let mut s = String::new();

    // Header: [kind: Name] (module)
    let module_hint = if item.module_path.is_empty() {
        String::new()
    } else {
        format!(" ({})", item.module_path.join("::"))
    };
    s.push_str(&format!("[{}: {}{}]\n", item.kind, item.name, module_hint));

    // Doc summary — first line only
    if let Some(doc_line) = item.doc.lines().next() {
        let trimmed = doc_line.trim();
        if !trimmed.is_empty() {
            s.push_str(&format!("// {}\n", trimmed));
        }
    }

    // Signature — compressed
    if !item.signature.is_empty() {
        s.push_str(&format!("sig: {}\n", item.signature.trim()));
    }

    // Fields
    if !item.fields.is_empty() {
        let fields: Vec<String> = item.fields.iter()
            .map(|f| {
                if f.doc.is_empty() {
                    format!("{}: {}", f.name, f.ty)
                } else {
                    format!("{}: {} // {}", f.name, f.ty, first_line(&f.doc))
                }
            })
            .collect();
        s.push_str(&format!("fields: {}\n", fields.join(", ")));
    }

    // Variants (enums — the most critical for Quartz)
    if !item.variants.is_empty() {
        s.push_str("variants:\n");
        for v in &item.variants {
            let fields = if v.fields.is_empty() {
                String::new()
            } else {
                let fstr: Vec<String> = v.fields.iter()
                    .map(|f| {
                        if f.name.starts_with('_') {
                            f.ty.clone()
                        } else {
                            format!("{}: {}", f.name, f.ty)
                        }
                    })
                    .collect();
                format!(" {{ {} }}", fstr.join(", "))
            };
            let doc = if v.doc.is_empty() {
                String::new()
            } else {
                format!(" // {}", first_line(&v.doc))
            };
            s.push_str(&format!("  {}::{}{}{}\n", item.name, v.name, fields, doc));
        }
    }

    // Methods — names + compressed signatures only
    if !item.methods.is_empty() {
        let methods: Vec<String> = item.methods.iter()
            .map(|m| {
                // Strip body keywords for brevity
                let sig = m.signature
                    .replace("pub fn ", "")
                    .replace("fn ", "");
                if m.doc.is_empty() {
                    sig
                } else {
                    format!("{} // {}", sig, first_line(&m.doc))
                }
            })
            .collect();
        s.push_str(&format!("methods: {}\n", methods.join(" | ")));
    }

    // Trait impls
    if !item.traits_impl.is_empty() {
        s.push_str(&format!("impl: {}\n", item.traits_impl.join(", ")));
    }

    s
}

fn build_summary_api(item: &ApiGraphItem) -> String {
    let doc = first_line(&item.doc);
    let variant_count = if item.variants.is_empty() {
        String::new()
    } else {
        format!(" [{} variants]", item.variants.len())
    };
    let field_count = if item.fields.is_empty() {
        String::new()
    } else {
        format!(" [{} fields]", item.fields.len())
    };

    if doc.is_empty() {
        format!("{} `{}`{}{}", item.kind, item.name, variant_count, field_count)
    } else {
        format!("{} `{}` — {}{}{}", item.kind, item.name, doc, variant_count, field_count)
    }
}

// ── syn visitor (for raw .rs files not covered by quartz-ctx) ────────────────

struct PendingImpl {
    self_ty: String,
    methods: Vec<String>,
}

struct CompressVisitor {
    units: Vec<CodeUnit>,
    members: Vec<CodeMember>,
    module_path: String,
    pending_impls: Vec<PendingImpl>,
}

impl CompressVisitor {
    fn flush_impls(&mut self) {
        for pending in self.pending_impls.drain(..) {
            if let Some(unit) = self.units.iter_mut().find(|u| u.name == pending.self_ty) {
                if !pending.methods.is_empty() {
                    unit.compressed.push_str(&format!(
                        "methods: {}\n",
                        pending.methods.join(" | ")
                    ));
                    // Rebuild term vector after augmenting compressed
                    let compressed = unit.compressed.clone();
                    unit.term_vector = build_term_vector_str(&compressed);
                }
            }
        }
    }

    fn make_unit(&self, kind: &str, name: &str, compressed: String) -> CodeUnit {
        let summary = format!("{} `{}`", kind, name);
        let id = if self.module_path.is_empty() {
            name.to_string()
        } else {
            format!("{}::{}", self.module_path, name)
        };
        CodeUnit {
            id,
            kind: kind.to_string(),
            name: name.to_string(),
            module_path: self.module_path.clone(),
            summary,
            term_vector: build_term_vector_str(&compressed),
            compressed,
            indexed_at: chrono::Utc::now(),
        }
    }
}

impl<'ast> Visit<'ast> for CompressVisitor {
    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        if !is_pub(&node.vis) { return; }

        let name = node.ident.to_string();
        let doc = extract_doc(&node.attrs);
        let fields: Vec<String> = if let syn::Fields::Named(nf) = &node.fields {
            nf.named.iter()
                .filter(|f| is_pub(&f.vis))
                .map(|f| format!("{}: {}", f.ident.as_ref().unwrap(), ty_str(&f.ty)))
                .collect()
        } else { vec![] };

        let mut compressed = format!("[struct: {}]\n", name);
        if let Some(d) = doc.lines().next() { if !d.trim().is_empty() { compressed.push_str(&format!("// {}\n", d.trim())); } }
        if !fields.is_empty() { compressed.push_str(&format!("fields: {}\n", fields.join(", "))); }

        self.units.push(self.make_unit("struct", &name, compressed));
        syn::visit::visit_item_struct(self, node);
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        if !is_pub(&node.vis) { return; }

        let name = node.ident.to_string();
        let doc = extract_doc(&node.attrs);
        let mut compressed = format!("[enum: {}]\n", name);
        if let Some(d) = doc.lines().next() { if !d.trim().is_empty() { compressed.push_str(&format!("// {}\n", d.trim())); } }

        compressed.push_str("variants:\n");
        for v in &node.variants {
            let vdoc = extract_doc(&v.attrs);
            let fields: Vec<String> = match &v.fields {
                syn::Fields::Named(nf) => nf.named.iter()
                    .map(|f| format!("{}: {}", f.ident.as_ref().unwrap(), ty_str(&f.ty)))
                    .collect(),
                syn::Fields::Unnamed(uf) => uf.unnamed.iter()
                    .map(|f| ty_str(&f.ty))
                    .collect(),
                syn::Fields::Unit => vec![],
            };
            let fstr = if fields.is_empty() { String::new() } else { format!(" {{ {} }}", fields.join(", ")) };
            let dstr = if vdoc.is_empty() { String::new() } else { format!(" // {}", first_line(&vdoc)) };
            compressed.push_str(&format!("  {}::{}{}{}\n", name, v.ident, fstr, dstr));
        }

        self.units.push(self.make_unit("enum", &name, compressed));
        syn::visit::visit_item_enum(self, node);
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !is_pub(&node.vis) { return; }
        let name = node.sig.ident.to_string();
        let sig = quote!(#(node.sig)).to_string();
        let doc = extract_doc(&node.attrs);
        let mut compressed = format!("[fn: {}]\n", name);
        if let Some(d) = doc.lines().next() { if !d.trim().is_empty() { compressed.push_str(&format!("// {}\n", d.trim())); } }
        compressed.push_str(&format!("sig: {}\n", sig));
        self.units.push(self.make_unit("fn", &name, compressed));
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let self_ty = match node.self_ty.as_ref() {
            syn::Type::Path(p) => p.path.segments.last()
                .map(|s| s.ident.to_string()).unwrap_or_default(),
            _ => return,
        };
        let methods: Vec<String> = node.items.iter().filter_map(|item| {
            if let syn::ImplItem::Fn(m) = item {
                if !is_pub(&m.vis) { return None; }
                Some(m.sig.ident.to_string())
            } else { None }
        }).collect();

        self.pending_impls.push(PendingImpl { self_ty, methods });
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if let Some((_, items)) = &node.content {
            let old = self.module_path.clone();
            if self.module_path.is_empty() {
                self.module_path = node.ident.to_string();
            } else {
                self.module_path = format!("{}::{}", self.module_path, node.ident);
            }
            for item in items { self.visit_item(item); }
            self.flush_impls();
            self.module_path = old;
        }
    }
}

// ── TF-IDF term vectors ───────────────────────────────────────────────────────

/// Builds a normalised TF-IDF-style term vector from text.
/// Stored in the DB; used for cosine similarity search without external ML deps.
pub fn build_term_vector_str(text: &str) -> Vec<(String, f32)> {
    let tokens = tokenise(text);
    if tokens.is_empty() { return vec![]; }

    let mut tf: HashMap<String, f32> = HashMap::new();
    for tok in &tokens {
        *tf.entry(tok.clone()).or_insert(0.0) += 1.0;
    }
    let total = tokens.len() as f32;
    let mut vec: Vec<(String, f32)> = tf.into_iter()
        .map(|(k, v)| (k, v / total))
        .collect();

    // Normalise
    let magnitude = (vec.iter().map(|(_, w)| w * w).sum::<f32>()).sqrt();
    if magnitude > 0.0 {
        for (_, w) in &mut vec { *w /= magnitude; }
    }

    vec.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    vec
}

/// Cosine similarity between two term vectors.
pub fn cosine_similarity(a: &[(String, f32)], b: &[(String, f32)]) -> f32 {
    let b_map: HashMap<&str, f32> = b.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    a.iter()
        .filter_map(|(k, v)| b_map.get(k.as_str()).map(|bv| v * bv))
        .sum()
}

fn tokenise(text: &str) -> Vec<String> {
    // Split on non-alphanumeric, lowercase, filter stop words and short tokens
    let stop: &[&str] = &[
        "the", "a", "an", "is", "it", "in", "of", "to", "and", "or", "for",
        "on", "at", "be", "as", "by", "fn", "pub", "let", "use", "mut", "self",
    ];
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|t| t.to_lowercase())
        .filter(|t| t.len() >= 3 && !stop.contains(&t.as_str()))
        .collect()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn derive_module_path(base: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(base).unwrap_or(file);
    relative
        .with_extension("")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .filter(|s| s != "mod" && s != "lib" && s != "main")
        .collect::<Vec<_>>()
        .join("::")
}

fn is_pub(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

fn extract_doc(attrs: &[syn::Attribute]) -> String {
    attrs.iter().filter_map(|a| {
        if !a.path().is_ident("doc") { return None; }
        if let syn::Meta::NameValue(nv) = &a.meta {
            if let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &nv.value {
                return Some(s.value().trim().to_string());
            }
        }
        None
    }).collect::<Vec<_>>().join("\n")
}

fn ty_str(ty: &syn::Type) -> String {
    quote!(#ty).to_string()
        .replace(" :: ", "::")
        .replace("< ", "<")
        .replace(" >", ">")
}

fn first_line(s: &str) -> &str {
    s.lines().next().map(str::trim).unwrap_or("")
}
