//! A SCIP index of a workspace, and its files' text, turned into the plain model.

mod accessors;
mod blocks;
mod forwarders;
mod impls;
mod source;
mod targets;
mod unanalyzed;
mod wrappers;

use std::collections::{HashMap, HashSet};

use crate::Error;
use crate::model::{
    File, FileId, Item, ItemId, ItemKind, Model, NodeId, NodeKind, Reference, Tree, Unanalyzed,
};
use crate::scip::{Document, Impl, Index, Kind, Occurrence, Place, Range, Symbol};
use crate::workspace::{Package, Workspace};
use blocks::Blocks;
use source::{Access, ImplBlock, Plain, Source};

pub fn build(index: &Index, workspace: &Workspace) -> Result<Model, Error> {
    let mut builder = Builder::new(workspace);
    for document in &index.documents {
        builder.add_document(document)?;
    }
    let unanalyzed = builder.unanalyzed()?;
    Ok(builder.finish(index, unanalyzed))
}

fn is_test_module(name: &str) -> bool {
    name == "tests" || name.ends_with("_tests") || name.starts_with("test_")
}

struct Builder<'a> {
    workspace: &'a Workspace,
    tree: Tree,
    files: Vec<File>,
    items: Vec<Item>,
    references: Vec<Reference>,
    /// By the place of each reference in `references`: what its code does
    /// with what it names.
    accesses: Vec<Access>,
    packages: HashMap<(&'a str, &'a str), NodeId>,
    symbols: HashMap<&'a str, Named>,
    /// Where each item that is not a module is written, as its symbol says.
    places: HashMap<ItemId, Place<'a>>,
    functions: HashSet<ItemId>,
    /// Each item's definition, by its unit and whole range; `None` for an
    /// item defined more than once, which no one text describes.
    definitions: HashMap<ItemId, Option<(usize, Range)>>,
    units: Vec<Unit<'a>>,
}

/// What a symbol of the index stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// A parameter, variable, or type parameter, which only its document names.
    Local,
    Item(ItemId),
    /// Something outside the workspace, by what its last descriptor says it is.
    Outside(Kind),
}

struct Unit<'a> {
    file: FileId,
    document: &'a Document<'a>,
    source: Source,
    impl_blocks: Vec<ImplBlock>,
    inline_modules: Blocks<NodeId>,
    /// The items whose definitions hold code: functions, types, fields, constants.
    holders: Blocks<ItemId>,
    named_modules: Vec<NodeId>,
}

impl<'a> Builder<'a> {
    fn new(workspace: &'a Workspace) -> Builder<'a> {
        let mut builder = Builder {
            workspace,
            tree: Tree::new(),
            files: Vec::new(),
            items: Vec::new(),
            references: Vec::new(),
            accesses: Vec::new(),
            packages: HashMap::new(),
            symbols: HashMap::new(),
            places: HashMap::new(),
            functions: HashSet::new(),
            definitions: HashMap::new(),
            units: Vec::new(),
        };
        builder.add_packages();
        builder
    }

    /// The module files the documents declare that the index lacks.
    fn unanalyzed(&self) -> Result<Vec<Unanalyzed>, Error> {
        let analyzed: Vec<unanalyzed::Analyzed> = self
            .units
            .iter()
            .map(|unit| {
                let file = &self.files[unit.file.index()];
                unanalyzed::Analyzed {
                    path: &file.path,
                    module: file.module,
                    source: &unit.source,
                }
            })
            .collect();
        unanalyzed::find(&analyzed, self.workspace)
    }

    /// The model, once every document is in.
    fn finish(mut self, index: &Index, unanalyzed: Vec<Unanalyzed>) -> Model {
        self.read_symbol_information(index);
        self.mark_test_code();
        self.place_in_types();
        self.mark_copies();
        let wrappers = self.wrappers();
        Model {
            tree: self.tree,
            files: self.files,
            unanalyzed,
            items: self.items,
            references: self.references,
            wrappers,
        }
    }

    fn add_packages(&mut self) {
        let mut packages: Vec<&Package> = self.workspace.packages.iter().collect();
        packages.sort_by_key(|package| package.directory.split('/').count());
        let root = self.tree.root();
        let mut placed: Vec<(&str, NodeId)> = Vec::new();
        for package in packages {
            let node = if package.directory.is_empty() {
                let node = self.tree.node_mut(root);
                node.kind = NodeKind::Package;
                node.name = package.crate_name.clone();
                root
            } else {
                let parent = placed
                    .iter()
                    .filter(|(directory, _)| is_under(&package.directory, directory))
                    .max_by_key(|(directory, _)| directory.len())
                    .map_or(root, |&(_, node)| node);
                self.tree
                    .child(parent, NodeKind::Package, &package.crate_name)
            };
            placed.push((&package.directory, node));
            self.packages
                .insert((&package.name, &package.version), node);
        }
    }

    fn add_document(&mut self, document: &'a Document<'a>) -> Result<(), Error> {
        let not_understood = || {
            Error::new(format!(
                "{}: the index does not start with the file's module",
                document.path
            ))
        };
        let first = document.occurrences.first().ok_or_else(not_understood)?;
        let symbol = parse(first.symbol)?.ok_or_else(not_understood)?;
        let (Some(path), true) = (symbol.module(), first.definition) else {
            return Err(not_understood());
        };
        let Some(module) = self.module(&symbol, &path) else {
            return Ok(());
        };
        let on_disk = self.workspace.root.join(document.path);
        let text = std::fs::read_to_string(&on_disk)
            .map_err(|e| Error::new(format!("{}: {e}", on_disk.display())))?;
        let file = FileId::new(self.files.len());
        self.files.push(File {
            path: document.path.to_owned(),
            module,
            test: false,
            lines: text.lines().count(),
        });
        let at = self.units.len();
        let mut inline_modules = Vec::new();
        let mut holders = Vec::new();
        let mut named_modules = Vec::new();
        for occurrence in &document.occurrences[1..] {
            let Some(item) = self.item(occurrence.symbol)? else {
                continue;
            };
            let item_module = self.items[item.index()].module;
            if !occurrence.definition {
                named_modules.extend(item_module);
                continue;
            }
            let Some(block) = occurrence.enclosing else {
                continue;
            };
            if let Some(module) = item_module {
                inline_modules.push((block, module));
            } else {
                holders.push((block, item));
                self.definitions
                    .entry(item)
                    .and_modify(|known| *known = None)
                    .or_insert(Some((at, block)));
            }
        }
        let source = Source::new(text);
        let unit = Unit {
            file,
            document,
            impl_blocks: source.impl_blocks(),
            source,
            inline_modules: Blocks::new(inline_modules),
            holders: Blocks::new(holders),
            named_modules,
        };
        let imports = unit.source.use_declarations();
        for occurrence in &document.occurrences {
            self.add_definition_or_reference(occurrence, &unit, module, &imports)?;
        }
        self.units.push(unit);
        Ok(())
    }

    /// `imports`: where the unit's `use` declarations are, in order.
    fn add_definition_or_reference(
        &mut self,
        occurrence: &Occurrence<'a>,
        unit: &Unit,
        module: NodeId,
        imports: &[Range],
    ) -> Result<(), Error> {
        let Some(item) = self.item(occurrence.symbol)? else {
            return Ok(());
        };
        if occurrence.definition {
            self.items[item.index()].file.get_or_insert(unit.file);
            return Ok(());
        }
        let before = imports.partition_point(|import| import.start <= occurrence.range.start);
        if before > 0 && imports[before - 1].holds(occurrence.range) {
            return Ok(());
        }
        let from = unit
            .inline_modules
            .innermost(occurrence.range)
            .unwrap_or(module);
        self.references.push(Reference {
            file: unit.file,
            line: occurrence.range.start.line + 1,
            from,
            holder: unit.holders.innermost(occurrence.range),
            item,
            copies: false,
            test: false,
        });
        self.accesses.push(unit.source.access(occurrence.range));
        Ok(())
    }

    /// Marks the references to fields that only read the field's value: a
    /// copy of any field, a shared borrow of plain data, a method call on a
    /// number.
    fn mark_copies(&mut self) {
        let mut plain: HashMap<ItemId, Plain> = HashMap::new();
        for (at, access) in self.accesses.iter().enumerate() {
            let field = self.references[at].item;
            if self.items[field.index()].kind != ItemKind::Field {
                continue;
            }
            let holds = *plain.entry(field).or_insert_with(|| {
                match self.definitions.get(&field).copied().flatten() {
                    Some((unit, range)) => self.units[unit].source.plain(range),
                    None => Plain::No,
                }
            });
            self.references[at].copies = match access {
                Access::Copy => true,
                Access::Borrow => holds != Plain::No,
                Access::Call => holds == Plain::Scalar,
                Access::Reach => false,
            };
        }
    }

    fn item(&mut self, text: &'a str) -> Result<Option<ItemId>, Error> {
        match self.named(text)? {
            Named::Item(item) => Ok(Some(item)),
            Named::Local | Named::Outside(_) => Ok(None),
        }
    }

    fn named(&mut self, text: &'a str) -> Result<Named, Error> {
        if let Some(&known) = self.symbols.get(text) {
            return Ok(known);
        }
        let named = match parse(text)? {
            Some(symbol) => self
                .new_item(&symbol)
                .map_or(Named::Outside(symbol.kind()), Named::Item),
            None => Named::Local,
        };
        self.symbols.insert(text, named);
        Ok(named)
    }

    fn new_item(&mut self, symbol: &Symbol<'a>) -> Option<ItemId> {
        let id = ItemId::new(self.items.len());
        if let Some(path) = symbol.module() {
            let module = self.module(symbol, &path)?;
            let owner = self.tree.parent(module).unwrap_or(module);
            self.items.push(Item {
                name: self.tree.node(module).name.clone(),
                owner,
                module: Some(module),
                file: None,
                kind: ItemKind::Module,
                parent: None,
                trait_impl: false,
                takes_self: false,
            });
            return Some(id);
        }
        let owner = self.module(symbol, &symbol.namespaces())?;
        let place = symbol.place();
        let (kind, trait_impl) = match &place {
            Place::Impl(Impl { is_block: true, .. }) => (ItemKind::Impl, false),
            Place::Impl(block) => (ItemKind::Other, block.of_trait),
            Place::Module | Place::In(_) => (ItemKind::Other, false),
        };
        self.items.push(Item {
            name: symbol.item_name(),
            owner,
            module: None,
            file: None,
            kind,
            parent: None,
            trait_impl,
            takes_self: false,
        });
        if symbol.is_function() {
            self.functions.insert(id);
        }
        self.places.insert(id, place);
        Some(id)
    }

    fn read_symbol_information(&mut self, index: &Index) {
        for information in index.documents.iter().flat_map(|d| &d.symbols) {
            let Some(&Named::Item(id)) = self.symbols.get(information.symbol) else {
                continue;
            };
            let item = &mut self.items[id.index()];
            if item.module.is_none() && item.kind != ItemKind::Impl {
                item.kind = item_kind(information.kind);
            }
        }
    }

    fn module(&mut self, symbol: &Symbol, path: &[&str]) -> Option<NodeId> {
        let mut node = symbol.package_in(&self.packages)?;
        for name in path {
            node = self.tree.child(node, NodeKind::Module, name);
        }
        Some(node)
    }

    fn mark_test_code(&mut self) {
        for id in self.tree.ids() {
            let node = self.tree.node_mut(id);
            node.test = node.kind == NodeKind::Module && is_test_module(&node.name);
        }
        let target_units: Vec<targets::Unit> = self
            .units
            .iter()
            .map(|unit| {
                let module = self.files[unit.file.index()].module;
                let mut defines = vec![module];
                defines.extend(unit.inline_modules.iter().map(|(_, module)| module));
                targets::Unit {
                    path: unit.document.path,
                    module,
                    defines,
                    names: unit.named_modules.clone(),
                }
            })
            .collect();
        let roots: Vec<_> = self
            .workspace
            .packages
            .iter()
            .flat_map(|package| &package.targets)
            .map(|target| (target.kind, target.root.as_str()))
            .collect();
        let kinds = targets::assign(&target_units, &roots, &self.tree);
        for (unit, kind) in self.units.iter().zip(kinds) {
            self.files[unit.file.index()].test = kind.is_some_and(|kind| kind.is_test());
        }
        for reference in &mut self.references {
            reference.test =
                self.files[reference.file.index()].test || self.tree.is_test(reference.from);
        }
    }
}

fn item_kind(kind: u64) -> ItemKind {
    const ASSOCIATED_TYPE: u64 = 3;
    const CONSTANT: u64 = 8;
    const ENUM: u64 = 11;
    const ENUM_MEMBER: u64 = 12;
    const FIELD: u64 = 15;
    const FUNCTION: u64 = 17;
    const METHOD: u64 = 26;
    const STRUCT: u64 = 49;
    const TRAIT: u64 = 53;
    const TYPE_ALIAS: u64 = 55;
    const UNION: u64 = 59;
    const TRAIT_METHOD: u64 = 70;
    const STATIC_METHOD: u64 = 80;
    const STATIC_VARIABLE: u64 = 82;
    match kind {
        ASSOCIATED_TYPE | ENUM | STRUCT | TRAIT | TYPE_ALIAS | UNION => ItemKind::Type,
        FUNCTION | METHOD | TRAIT_METHOD | STATIC_METHOD => ItemKind::Function,
        FIELD => ItemKind::Field,
        ENUM_MEMBER => ItemKind::Variant,
        CONSTANT | STATIC_VARIABLE => ItemKind::Constant,
        _ => ItemKind::Other,
    }
}

fn parse(text: &str) -> Result<Option<Symbol<'_>>, Error> {
    Symbol::parse(text).map_err(|e| Error::new(e.to_string()))
}

fn is_under(path: &str, directory: &str) -> bool {
    directory.is_empty()
        || path
            .strip_prefix(directory)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn lines(range: Range) -> u32 {
    let last = if range.end.column == 0 {
        range.end.line
    } else {
        range.end.line + 1
    };
    last.saturating_sub(range.start.line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Package;

    #[test]
    fn a_package_is_inside_the_package_whose_directory_holds_it_and_a_plain_directory_is_no_level()
    {
        let package = |name: &str, directory: &str| Package {
            name: name.into(),
            version: "0.1.0".into(),
            crate_name: name.into(),
            directory: directory.into(),
            targets: Vec::new(),
        };
        let workspace = Workspace {
            root: "/w".into(),
            target_directory: "/w/target".into(),
            packages: vec![
                package("helper", "app/tests/programs/helper"),
                package("lib", "crates/lib"),
                package("app", "app"),
            ],
        };
        let builder = Builder::new(&workspace);
        let tree = &builder.tree;
        let node = |name| builder.packages[&(name, "0.1.0")];
        assert_eq!(tree.parent(node("lib")), Some(tree.root()));
        assert_eq!(tree.parent(node("app")), Some(tree.root()));
        assert_eq!(tree.parent(node("helper")), Some(node("app")));
    }

    #[test]
    fn test_modules_by_name() {
        assert!(is_test_module("tests"));
        assert!(is_test_module("shuttle_tests"));
        assert!(is_test_module("test_machine"));
        assert!(!is_test_module("contests"));
        assert!(!is_test_module("testing"));
    }
}
